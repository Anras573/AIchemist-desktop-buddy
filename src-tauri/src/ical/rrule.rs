//! `RRULE` parsing and expansion (RFC 5545 section 3.3.10), for the subset
//! calendar servers actually send: FREQ DAILY/WEEKLY/MONTHLY/YEARLY with
//! INTERVAL, COUNT, UNTIL, BYDAY (with ordinals), BYMONTHDAY, BYMONTH,
//! BYSETPOS and WKST.
//!
//! Expansion works on naive wall-clock date-times in the event's own time
//! zone, so a 09:00 meeting stays at 09:00 across DST changes. Converting to
//! UTC is the caller's job.

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, Weekday};

use crate::domain::{Error, Result};

/// Upper bound on periods (days, weeks, months, years) walked for one rule,
/// so a rule that never matches (e.g. `BYMONTHDAY=31;BYMONTH=2`) terminates.
const MAX_PERIODS: u32 = 50_000;

/// Recurrence frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// `UNTIL`, as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Until {
    /// A DATE: inclusive through the end of that day.
    Date(NaiveDate),
    /// A floating or local DATE-TIME (interpreted in the event's zone).
    Local(NaiveDateTime),
    /// A UTC DATE-TIME (the normal form when DTSTART has a TZID).
    Utc(NaiveDateTime),
}

/// A parsed recurrence rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Until>,
    pub by_month: Vec<u32>,
    pub by_month_day: Vec<i32>,
    /// (ordinal, weekday); ordinal `None` means every such weekday.
    pub by_day: Vec<(Option<i32>, Weekday)>,
    pub by_set_pos: Vec<i32>,
    pub wkst: Weekday,
}

/// Outcome of parsing an `RRULE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// A rule this module can expand.
    Rule(Rule),
    /// A valid rule using parts this module does not implement
    /// (SECONDLY/MINUTELY/HOURLY, BYHOUR, BYMINUTE, BYSECOND, BYWEEKNO,
    /// BYYEARDAY, non-Gregorian RSCALE). The caller falls back to DTSTART.
    Unsupported,
}

fn bad(line: usize, what: &str) -> Error {
    Error::protocol(format!("malformed RRULE at line {line}: {what}"))
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    Some(match s.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_int_list(v: &str, min: i32, max: i32, allow_neg: bool) -> Option<Vec<i32>> {
    v.split(',')
        .map(|s| {
            let n: i32 = s.trim().parse().ok()?;
            let ok = (min..=max).contains(&n.abs()) && (allow_neg || n > 0);
            ok.then_some(n)
        })
        .collect()
}

/// Parses an `RRULE` value. `line` is only used in error messages.
pub(crate) fn parse(value: &str, line: usize) -> Result<Parsed> {
    let mut freq = None;
    let mut rule = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_month: Vec::new(),
        by_month_day: Vec::new(),
        by_day: Vec::new(),
        by_set_pos: Vec::new(),
        wkst: Weekday::Mon,
    };
    let mut unsupported = false;

    for part in value.split(';').filter(|p| !p.trim().is_empty()) {
        let (key, v) = part
            .split_once('=')
            .ok_or_else(|| bad(line, "part without '='"))?;
        let v = v.trim();
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Some(Freq::Daily),
                    "WEEKLY" => Some(Freq::Weekly),
                    "MONTHLY" => Some(Freq::Monthly),
                    "YEARLY" => Some(Freq::Yearly),
                    "SECONDLY" | "MINUTELY" | "HOURLY" => None,
                    _ => return Err(bad(line, "unknown FREQ")),
                });
            }
            "INTERVAL" => {
                rule.interval = v
                    .parse()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| bad(line, "invalid INTERVAL"))?;
            }
            "COUNT" => {
                rule.count = Some(v.parse().map_err(|_| bad(line, "invalid COUNT"))?);
            }
            "UNTIL" => {
                let raw = super::value::parse_raw_date(v, false)
                    .ok_or_else(|| bad(line, "invalid UNTIL"))?;
                rule.until = Some(match raw {
                    super::value::RawDate::Date(d) => Until::Date(d),
                    super::value::RawDate::DateTime { local, utc: true } => Until::Utc(local),
                    super::value::RawDate::DateTime { local, utc: false } => Until::Local(local),
                });
            }
            "BYMONTH" => {
                rule.by_month = parse_int_list(v, 1, 12, false)
                    .ok_or_else(|| bad(line, "invalid BYMONTH"))?
                    .into_iter()
                    .map(i32::unsigned_abs)
                    .collect();
            }
            "BYMONTHDAY" => {
                rule.by_month_day = parse_int_list(v, 1, 31, true)
                    .ok_or_else(|| bad(line, "invalid BYMONTHDAY"))?;
            }
            "BYSETPOS" => {
                rule.by_set_pos =
                    parse_int_list(v, 1, 366, true).ok_or_else(|| bad(line, "invalid BYSETPOS"))?;
            }
            "BYDAY" => {
                for item in v.split(',') {
                    let item = item.trim();
                    if item.len() < 2 || !item.is_char_boundary(item.len() - 2) {
                        return Err(bad(line, "invalid BYDAY"));
                    }
                    let (ord, wd) = item.split_at(item.len() - 2);
                    let wd = parse_weekday(wd).ok_or_else(|| bad(line, "invalid BYDAY"))?;
                    let ord = if ord.is_empty() {
                        None
                    } else {
                        let n: i32 = ord.parse().map_err(|_| bad(line, "invalid BYDAY"))?;
                        if n == 0 || n.abs() > 53 {
                            return Err(bad(line, "invalid BYDAY"));
                        }
                        Some(n)
                    };
                    rule.by_day.push((ord, wd));
                }
            }
            "WKST" => {
                rule.wkst = parse_weekday(v).ok_or_else(|| bad(line, "invalid WKST"))?;
            }
            "BYHOUR" | "BYMINUTE" | "BYSECOND" | "BYWEEKNO" | "BYYEARDAY" => unsupported = true,
            "RSCALE" => unsupported |= !v.eq_ignore_ascii_case("GREGORIAN"),
            // SKIP (RFC 7529) and unknown extension parts are ignored.
            _ => {}
        }
    }

    match freq {
        None => Err(bad(line, "missing FREQ")),
        Some(None) => Ok(Parsed::Unsupported),
        Some(Some(_)) if unsupported => Ok(Parsed::Unsupported),
        Some(Some(f)) => {
            rule.freq = f;
            Ok(Parsed::Rule(rule))
        }
    }
}

/// Inputs to [`expand`].
pub(crate) struct Expansion {
    /// DTSTART as wall-clock time in the event's zone (midnight for dates).
    pub dtstart: NaiveDateTime,
    /// UNTIL, already converted to wall-clock time in the event's zone.
    pub until: Option<NaiveDateTime>,
    /// Occurrences before this are not returned (DTSTART always is). Rules
    /// without COUNT also skip whole periods up to it, which keeps expanding
    /// old daily events cheap. Pass a value safely before the window (allow
    /// for event duration and time zone offsets).
    pub skip_before: Option<NaiveDateTime>,
    /// Stop after this wall-clock time.
    pub stop_after: NaiveDateTime,
    /// Return at most this many occurrences.
    pub max: usize,
}

/// Expands `rule` into occurrence start times (wall clock), in order.
/// DTSTART is always the first occurrence and counts towards COUNT.
pub(crate) fn expand(rule: &Rule, x: &Expansion) -> Vec<NaiveDateTime> {
    let mut out = Vec::new();
    let dtstart = x.dtstart;
    if x.until.is_some_and(|u| dtstart > u) || rule.count == Some(0) || x.max == 0 {
        return out;
    }
    out.push(dtstart);
    let mut emitted: u32 = 1;
    let start_date = dtstart.date();
    let time = dtstart.time();

    let first_period = match (rule.count, x.skip_before) {
        (None, Some(skip)) if skip > dtstart => periods_before(rule, start_date, skip.date()),
        _ => 0,
    };

    let stop_date = x.stop_after.date();
    for k in first_period..first_period.saturating_add(MAX_PERIODS) {
        let Some((period_begin, dates)) = period_dates(rule, start_date, k) else {
            break; // date arithmetic overflowed: far beyond any window
        };
        if period_begin > stop_date {
            break;
        }
        for date in dates {
            let dt = date.and_time(time);
            if dt <= dtstart {
                continue;
            }
            if x.until.is_some_and(|u| dt > u) || dt > x.stop_after {
                return out;
            }
            emitted += 1;
            if rule.count.is_some_and(|c| emitted > c) {
                return out;
            }
            if x.skip_before.is_none_or(|s| dt >= s) {
                out.push(dt);
                if out.len() >= x.max {
                    return out;
                }
            }
        }
    }
    out
}

/// Whole periods that can be skipped before `target` while staying at least
/// one period early.
fn periods_before(rule: &Rule, start: NaiveDate, target: NaiveDate) -> u32 {
    let interval = i64::from(rule.interval);
    let days = (target - start).num_days();
    let months = i64::from(target.year() - start.year()) * 12 + i64::from(target.month())
        - i64::from(start.month());
    let periods = match rule.freq {
        Freq::Daily => days / interval,
        Freq::Weekly => days / (7 * interval),
        Freq::Monthly => months / interval,
        Freq::Yearly => i64::from(target.year() - start.year()) / interval,
    };
    u32::try_from((periods - 1).max(0)).unwrap_or(u32::MAX / 2)
}

fn add_days(d: NaiveDate, n: i64) -> Option<NaiveDate> {
    d.checked_add_signed(Duration::try_days(n)?)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let first = NaiveDate::from_ymd_opt(year, month, 1);
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    match (first, next) {
        (Some(a), Some(b)) => u32::try_from((b - a).num_days()).unwrap_or(0),
        _ => 0,
    }
}

/// The `n`th (1-based; negative counts from the end) `wd` in `days`, which
/// must be consecutive and sorted.
fn nth_weekday(days: &[NaiveDate], wd: Weekday, n: i32) -> Option<NaiveDate> {
    let matching: Vec<NaiveDate> = days.iter().copied().filter(|d| d.weekday() == wd).collect();
    let idx = if n > 0 {
        usize::try_from(n - 1).ok()?
    } else {
        matching.len().checked_sub(usize::try_from(-n).ok()?)?
    };
    matching.get(idx).copied()
}

/// Expands BYDAY within a run of consecutive days (a month or a year).
fn by_day_in(rule: &Rule, days: &[NaiveDate]) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    for &(ord, wd) in &rule.by_day {
        match ord {
            Some(n) => out.extend(nth_weekday(days, wd, n)),
            None => out.extend(days.iter().copied().filter(|d| d.weekday() == wd)),
        }
    }
    out
}

fn month_days(year: i32, month: u32) -> Vec<NaiveDate> {
    (1..=days_in_month(year, month))
        .filter_map(|d| NaiveDate::from_ymd_opt(year, month, d))
        .collect()
}

/// Candidate dates within one month for MONTHLY and YEARLY rules.
fn dates_in_month(rule: &Rule, start: NaiveDate, year: i32, month: u32) -> Vec<NaiveDate> {
    let len = i32::try_from(days_in_month(year, month)).unwrap_or(0);
    let by_month_day: Option<Vec<NaiveDate>> = if !rule.by_month_day.is_empty() {
        Some(
            rule.by_month_day
                .iter()
                .filter_map(|&d| {
                    let day = if d > 0 { d } else { len + d + 1 };
                    (1..=len).contains(&day).then_some(day)
                })
                .filter_map(|d| NaiveDate::from_ymd_opt(year, month, d.unsigned_abs()))
                .collect(),
        )
    } else if rule.by_day.is_empty() {
        // Inherit the day of month from DTSTART; months without it are skipped.
        Some(
            NaiveDate::from_ymd_opt(year, month, start.day())
                .into_iter()
                .collect(),
        )
    } else {
        None
    };

    if rule.by_day.is_empty() {
        return by_month_day.unwrap_or_default();
    }
    let by_day = by_day_in(rule, &month_days(year, month));
    match by_month_day {
        Some(days) => days.into_iter().filter(|d| by_day.contains(d)).collect(),
        None => by_day,
    }
}

/// Simple BYxxx filters used where they limit rather than expand.
fn passes_filters(rule: &Rule, d: NaiveDate, check_by_day: bool) -> bool {
    if !rule.by_month.is_empty() && !rule.by_month.contains(&d.month()) {
        return false;
    }
    if !rule.by_month_day.is_empty() {
        let len = i32::try_from(days_in_month(d.year(), d.month())).unwrap_or(0);
        let day = i32::try_from(d.day()).unwrap_or(0);
        if !rule
            .by_month_day
            .iter()
            .any(|&m| m == day || m == day - len - 1)
        {
            return false;
        }
    }
    !check_by_day || rule.by_day.is_empty() || rule.by_day.iter().any(|&(_, wd)| wd == d.weekday())
}

/// The `k`th period: its first day and its candidate dates (sorted,
/// deduplicated, BYSETPOS applied). `None` on date overflow.
fn period_dates(rule: &Rule, start: NaiveDate, k: u32) -> Option<(NaiveDate, Vec<NaiveDate>)> {
    let step = i64::from(k).checked_mul(i64::from(rule.interval))?;
    let (begin, mut dates) = match rule.freq {
        Freq::Daily => {
            let d = add_days(start, step)?;
            (
                d,
                vec![d]
                    .into_iter()
                    .filter(|d| passes_filters(rule, *d, true))
                    .collect(),
            )
        }
        Freq::Weekly => {
            let offset = i64::from(
                (start.weekday().num_days_from_monday() + 7 - rule.wkst.num_days_from_monday()) % 7,
            );
            let week_start = add_days(start, step.checked_mul(7)?.checked_sub(offset)?)?;
            let days: Vec<NaiveDate> = if rule.by_day.is_empty() {
                vec![add_days(week_start, offset)?]
            } else {
                rule.by_day
                    .iter()
                    .filter_map(|&(_, wd)| {
                        let off =
                            (wd.num_days_from_monday() + 7 - rule.wkst.num_days_from_monday()) % 7;
                        add_days(week_start, i64::from(off))
                    })
                    .collect()
            };
            let days = days
                .into_iter()
                .filter(|d| passes_filters(rule, *d, false))
                .collect();
            (week_start, days)
        }
        Freq::Monthly => {
            let first = start.with_day(1)?;
            let month = first.checked_add_months(Months::new(u32::try_from(step).ok()?))?;
            let days = if rule.by_month.is_empty() || rule.by_month.contains(&month.month()) {
                dates_in_month(rule, start, month.year(), month.month())
            } else {
                Vec::new()
            };
            (month, days)
        }
        Freq::Yearly => {
            let year = start.year().checked_add(i32::try_from(step).ok()?)?;
            let jan1 = NaiveDate::from_ymd_opt(year, 1, 1)?;
            let days = if !rule.by_month.is_empty() {
                rule.by_month
                    .iter()
                    .flat_map(|&m| dates_in_month(rule, start, year, m))
                    .collect()
            } else if !rule.by_month_day.is_empty() {
                (1..=12)
                    .flat_map(|m| dates_in_month(rule, start, year, m))
                    .collect()
            } else if !rule.by_day.is_empty() {
                let all: Vec<NaiveDate> = (1..=12).flat_map(|m| month_days(year, m)).collect();
                by_day_in(rule, &all)
            } else {
                NaiveDate::from_ymd_opt(year, start.month(), start.day())
                    .into_iter()
                    .collect()
            };
            (jan1, days)
        }
    };
    dates.sort_unstable();
    dates.dedup();
    if !rule.by_set_pos.is_empty() {
        let len = dates.len();
        let mut picked: Vec<NaiveDate> = rule
            .by_set_pos
            .iter()
            .filter_map(|&p| {
                let idx = if p > 0 {
                    usize::try_from(p - 1).ok()?
                } else {
                    len.checked_sub(usize::try_from(-p).ok()?)?
                };
                dates.get(idx).copied()
            })
            .collect();
        picked.sort_unstable();
        picked.dedup();
        dates = picked;
    }
    Some((begin, dates))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, m: u32, d: u32, h: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    fn rule(s: &str) -> Rule {
        match parse(s, 1).unwrap() {
            Parsed::Rule(r) => r,
            Parsed::Unsupported => panic!("unsupported"),
        }
    }

    fn run(r: &str, start: NaiveDateTime, stop: NaiveDateTime) -> Vec<NaiveDateTime> {
        let r = rule(r);
        let until = r.until.map(|u| match u {
            Until::Date(d) => d.and_hms_opt(23, 59, 59).unwrap(),
            Until::Local(l) | Until::Utc(l) => l,
        });
        expand(
            &r,
            &Expansion {
                dtstart: start,
                until,
                skip_before: None,
                stop_after: stop,
                max: 1000,
            },
        )
    }

    #[test]
    fn daily_with_interval_and_count() {
        let got = run(
            "FREQ=DAILY;INTERVAL=2;COUNT=3",
            dt(2026, 1, 30, 9),
            dt(2027, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![dt(2026, 1, 30, 9), dt(2026, 2, 1, 9), dt(2026, 2, 3, 9)]
        );
    }

    #[test]
    fn weekly_byday_with_until() {
        // Mon 2026-11-16.
        let got = run(
            "FREQ=WEEKLY;BYDAY=MO,WE,FR;UNTIL=20261123T090000Z",
            dt(2026, 11, 16, 9),
            dt(2027, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![
                dt(2026, 11, 16, 9),
                dt(2026, 11, 18, 9),
                dt(2026, 11, 20, 9),
                dt(2026, 11, 23, 9)
            ]
        );
    }

    #[test]
    fn weekly_interval_respects_wkst() {
        // RFC 5545 example: WKST changes which days fall in the same week.
        let start = dt(1997, 8, 5, 9); // Tuesday
        let mo = run(
            "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO",
            start,
            dt(1998, 1, 1, 0),
        );
        assert_eq!(
            mo,
            vec![
                dt(1997, 8, 5, 9),
                dt(1997, 8, 10, 9),
                dt(1997, 8, 19, 9),
                dt(1997, 8, 24, 9)
            ]
        );
        let su = run(
            "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
            start,
            dt(1998, 1, 1, 0),
        );
        assert_eq!(
            su,
            vec![
                dt(1997, 8, 5, 9),
                dt(1997, 8, 17, 9),
                dt(1997, 8, 19, 9),
                dt(1997, 8, 31, 9)
            ]
        );
    }

    #[test]
    fn monthly_last_friday_and_second_monday() {
        let got = run(
            "FREQ=MONTHLY;BYDAY=-1FR;COUNT=3",
            dt(2026, 1, 30, 15),
            dt(2027, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![
                dt(2026, 1, 30, 15),
                dt(2026, 2, 27, 15),
                dt(2026, 3, 27, 15)
            ]
        );
        let got = run(
            "FREQ=MONTHLY;BYDAY=2MO;COUNT=2",
            dt(2026, 1, 12, 8),
            dt(2027, 1, 1, 0),
        );
        assert_eq!(got, vec![dt(2026, 1, 12, 8), dt(2026, 2, 9, 8)]);
    }

    #[test]
    fn monthly_31st_skips_short_months_and_negative_monthday() {
        let got = run(
            "FREQ=MONTHLY;COUNT=3",
            dt(2026, 1, 31, 9),
            dt(2027, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![dt(2026, 1, 31, 9), dt(2026, 3, 31, 9), dt(2026, 5, 31, 9)]
        );
        let got = run(
            "FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=3",
            dt(2026, 1, 31, 9),
            dt(2027, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![dt(2026, 1, 31, 9), dt(2026, 2, 28, 9), dt(2026, 3, 31, 9)]
        );
    }

    #[test]
    fn monthly_last_weekday_via_setpos() {
        let got = run(
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=3",
            dt(2026, 1, 30, 9),
            dt(2027, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![dt(2026, 1, 30, 9), dt(2026, 2, 27, 9), dt(2026, 3, 31, 9)]
        );
    }

    #[test]
    fn yearly_variants() {
        let got = run("FREQ=YEARLY;COUNT=3", dt(2024, 2, 29, 9), dt(2040, 1, 1, 0));
        assert_eq!(
            got,
            vec![dt(2024, 2, 29, 9), dt(2028, 2, 29, 9), dt(2032, 2, 29, 9)]
        );
        let got = run(
            "FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU;COUNT=3",
            dt(2025, 3, 30, 2),
            dt(2040, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![dt(2025, 3, 30, 2), dt(2026, 3, 29, 2), dt(2027, 3, 28, 2)]
        );
        let got = run(
            "FREQ=YEARLY;BYMONTH=1,7;COUNT=3",
            dt(2026, 1, 5, 9),
            dt(2040, 1, 1, 0),
        );
        assert_eq!(
            got,
            vec![dt(2026, 1, 5, 9), dt(2026, 7, 5, 9), dt(2027, 1, 5, 9)]
        );
    }

    #[test]
    fn never_matching_rule_terminates() {
        let got = run(
            "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30",
            dt(2026, 1, 1, 9),
            dt(9999, 1, 1, 0),
        );
        assert_eq!(got, vec![dt(2026, 1, 1, 9)]);
    }

    #[test]
    fn skip_before_fast_forwards_without_count() {
        let r = rule("FREQ=DAILY");
        let got = expand(
            &r,
            &Expansion {
                dtstart: dt(1990, 1, 1, 9),
                until: None,
                skip_before: Some(dt(2026, 11, 16, 0)),
                stop_after: dt(2026, 11, 18, 0),
                max: 1000,
            },
        );
        assert_eq!(
            got,
            vec![dt(1990, 1, 1, 9), dt(2026, 11, 16, 9), dt(2026, 11, 17, 9)]
        );
    }

    #[test]
    fn max_caps_output() {
        let r = rule("FREQ=DAILY");
        let got = expand(
            &r,
            &Expansion {
                dtstart: dt(2026, 1, 1, 9),
                until: None,
                skip_before: None,
                stop_after: dt(2030, 1, 1, 0),
                max: 5,
            },
        );
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn parse_errors_and_unsupported() {
        for bad in [
            "",
            "INTERVAL=2",
            "FREQ=FOO",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=WEEKLY;BYDAY=XX",
            "FREQ=MONTHLY;BYDAY=0MO",
            "FREQ=DAILY;UNTIL=nope",
            "FREQ=DAILY;BYMONTH=13",
            "FREQ",
        ] {
            assert!(parse(bad, 1).is_err(), "{bad}");
        }
        assert_eq!(parse("FREQ=HOURLY", 1).unwrap(), Parsed::Unsupported);
        assert_eq!(
            parse("FREQ=YEARLY;BYWEEKNO=20", 1).unwrap(),
            Parsed::Unsupported
        );
        assert_eq!(
            parse("FREQ=DAILY;BYHOUR=9,17", 1).unwrap(),
            Parsed::Unsupported
        );
    }
}
