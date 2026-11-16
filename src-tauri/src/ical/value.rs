//! Property value types (RFC 5545 section 3.3): DATE, DATE-TIME, DURATION,
//! UTC-OFFSET and TEXT.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

/// A DATE or DATE-TIME value, before time zone resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawDate {
    /// `VALUE=DATE`.
    Date(NaiveDate),
    /// A DATE-TIME; `utc` is true for the `Z` form.
    DateTime { local: NaiveDateTime, utc: bool },
}

/// Parses `YYYYMMDD`.
pub(crate) fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y = s[0..4].parse().ok()?;
    let m = s[4..6].parse().ok()?;
    let d = s[6..8].parse().ok()?;
    NaiveDate::from_ymd_opt(y, m, d)
}

/// Parses `YYYYMMDDTHHMMSS[Z]`.
fn parse_date_time(s: &str) -> Option<(NaiveDateTime, bool)> {
    let s = s.trim();
    let (s, utc) = match s.strip_suffix(['Z', 'z']) {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    let (date, time) = s.split_once(['T', 't'])?;
    let date = parse_date(date)?;
    if time.len() != 6 || !time.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h = time[0..2].parse().ok()?;
    let mi = time[2..4].parse().ok()?;
    // A leap second (60) is clamped to 59.
    let sec: u32 = time[4..6].parse().ok()?;
    let t = NaiveTime::from_hms_opt(h, mi, sec.min(59))?;
    Some((date.and_time(t), utc))
}

/// Parses a DATE or DATE-TIME. `value_date` is true for `VALUE=DATE`; the
/// value's shape is also accepted on its own, because some producers omit
/// the parameter.
pub(crate) fn parse_raw_date(s: &str, value_date: bool) -> Option<RawDate> {
    let s = s.trim();
    if value_date || !s.contains(['T', 't']) {
        // Some producers send a DATE-TIME with VALUE=DATE; keep the date part.
        let date = s.get(..8)?;
        return parse_date(date).map(RawDate::Date);
    }
    parse_date_time(s).map(|(local, utc)| RawDate::DateTime { local, utc })
}

/// A DURATION value split into the nominal day part (weeks and days, which
/// follow wall-clock time across DST changes) and the exact seconds part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct IcalDuration {
    pub days: i64,
    pub seconds: i64,
}

/// Parses `[+-]P[nW][nD][T[nH][nM][nS]]`.
pub(crate) fn parse_duration(s: &str) -> Option<IcalDuration> {
    let s = s.trim();
    let (neg, s) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let s = s.strip_prefix(['P', 'p'])?;
    let mut d = IcalDuration::default();
    let mut in_time = false;
    let mut num: Option<i64> = None;
    let mut any = false;
    for c in s.chars() {
        match c.to_ascii_uppercase() {
            '0'..='9' => {
                let digit = i64::from(c.to_digit(10)?);
                num = Some(num.unwrap_or(0).checked_mul(10)?.checked_add(digit)?);
            }
            'T' if !in_time && num.is_none() => in_time = true,
            unit => {
                let n = num.take()?;
                any = true;
                match (unit, in_time) {
                    ('W', false) => d.days = d.days.checked_add(n.checked_mul(7)?)?,
                    ('D', false) => d.days = d.days.checked_add(n)?,
                    ('H', true) => d.seconds = d.seconds.checked_add(n.checked_mul(3600)?)?,
                    ('M', true) => d.seconds = d.seconds.checked_add(n.checked_mul(60)?)?,
                    ('S', true) => d.seconds = d.seconds.checked_add(n)?,
                    _ => return None,
                }
            }
        }
    }
    if num.is_some() || !any {
        return None;
    }
    // Keep durations sane (100 years) so later date arithmetic cannot overflow.
    if d.days > 36_600 || d.seconds > 36_600 * 86_400 {
        return None;
    }
    if neg {
        d.days = -d.days;
        d.seconds = -d.seconds;
    }
    Some(d)
}

/// Parses a UTC-OFFSET (`+HHMM` or `+HHMMSS`) into seconds east of UTC.
pub(crate) fn parse_utc_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = &s[1..];
    if !(digits.len() == 4 || digits.len() == 6) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits[0..2].parse().ok()?;
    let m: i32 = digits[2..4].parse().ok()?;
    let sec: i32 = if digits.len() == 6 {
        digits[4..6].parse().ok()?
    } else {
        0
    };
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60 + sec))
}

/// Unescapes a TEXT value: `\n`/`\N` become a newline, `\,` `\;` `\\` the
/// literal character. Unknown escapes keep the character after the
/// backslash.
pub(crate) fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a multi-valued property value on commas (DATE / DATE-TIME / PERIOD
/// lists contain no escaped commas).
pub(crate) fn split_list(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(str::trim).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_date_times() {
        assert_eq!(
            parse_raw_date("20261116", true),
            Some(RawDate::Date(
                NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
            ))
        );
        assert_eq!(
            parse_raw_date("20261116", false),
            Some(RawDate::Date(
                NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
            ))
        );
        let dt = NaiveDate::from_ymd_opt(2026, 11, 16)
            .unwrap()
            .and_hms_opt(9, 30, 0)
            .unwrap();
        assert_eq!(
            parse_raw_date("20261116T093000Z", false),
            Some(RawDate::DateTime {
                local: dt,
                utc: true
            })
        );
        assert_eq!(
            parse_raw_date("20261116T093000", false),
            Some(RawDate::DateTime {
                local: dt,
                utc: false
            })
        );
        for bad in [
            "2026111",
            "20261332",
            "20261116T9300",
            "x",
            "20261116T250000",
        ] {
            assert_eq!(parse_raw_date(bad, false), None, "{bad}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(
            parse_duration("PT1H30M"),
            Some(IcalDuration {
                days: 0,
                seconds: 5400
            })
        );
        assert_eq!(
            parse_duration("P1W"),
            Some(IcalDuration {
                days: 7,
                seconds: 0
            })
        );
        assert_eq!(
            parse_duration("-P1DT2S"),
            Some(IcalDuration {
                days: -1,
                seconds: -2
            })
        );
        for bad in [
            "P",
            "PT",
            "1H",
            "P1H",
            "PT1D",
            "P1",
            "P99999999999999999999D",
        ] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_utc_offset("+0100"), Some(3600));
        assert_eq!(parse_utc_offset("-0500"), Some(-18000));
        assert_eq!(parse_utc_offset("+053000"), Some(19800));
        assert_eq!(parse_utc_offset("0100"), None);
    }

    #[test]
    fn text_unescaping() {
        assert_eq!(
            unescape_text(r"Line 1\nLine 2\, with comma\; and \\ backslash"),
            "Line 1\nLine 2, with comma; and \\ backslash"
        );
    }
}
