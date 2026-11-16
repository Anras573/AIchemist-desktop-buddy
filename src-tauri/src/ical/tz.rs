//! Time zones from embedded `VTIMEZONE` definitions (RFC 5545 section
//! 3.6.5).
//!
//! Each `VTIMEZONE` is turned into a sorted list of UTC-offset transitions by
//! expanding its STANDARD/DAYLIGHT observances (DTSTART, RRULE, RDATE) with
//! the same recurrence engine used for events. Only the years around the
//! requested window are materialised.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};

use super::parser::Component;
use super::rrule::{self, Expansion, Parsed, Until};
use super::value::{parse_raw_date, parse_utc_offset, split_list, RawDate};
use crate::domain::{Clock, Error, Result};

/// A zone defined by a `VTIMEZONE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VTimeZone {
    pub tzid: String,
    /// Offset in effect before the first transition (seconds east of UTC).
    initial_offset: i32,
    /// (transition instant as naive UTC, offset from then on), sorted.
    transitions: Vec<(NaiveDateTime, i32)>,
}

fn bad(line: usize, what: &str) -> Error {
    Error::protocol(format!("malformed VTIMEZONE at line {line}: {what}"))
}

impl VTimeZone {
    /// Builds the zone, materialising transitions between `from_year` and
    /// `to_year` (inclusive), plus each observance's DTSTART.
    pub fn from_component(c: &Component, from_year: i32, to_year: i32) -> Result<Self> {
        let tzid = c
            .prop("TZID")
            .map(|p| p.value.trim().to_owned())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| bad(c.line, "missing TZID"))?;

        let mut transitions: Vec<(NaiveDateTime, i32, i32)> = Vec::new();
        for obs in c
            .children
            .iter()
            .filter(|o| o.name == "STANDARD" || o.name == "DAYLIGHT")
        {
            let offset = |name: &str| {
                obs.prop(name)
                    .and_then(|p| parse_utc_offset(&p.value))
                    .ok_or_else(|| bad(obs.line, "missing or invalid UTC offset"))
            };
            let from = offset("TZOFFSETFROM")?;
            let to = offset("TZOFFSETTO")?;
            let dtstart = match obs
                .prop("DTSTART")
                .and_then(|p| parse_raw_date(&p.value, false))
            {
                Some(RawDate::DateTime { local, .. }) => local,
                Some(RawDate::Date(d)) => d.and_time(chrono::NaiveTime::MIN),
                None => return Err(bad(obs.line, "missing or invalid DTSTART")),
            };

            // Onsets are wall-clock times in the offset in effect before them.
            let mut onsets = vec![dtstart];
            for p in obs.props_named("RRULE") {
                if let Parsed::Rule(rule) = rrule::parse(&p.value, p.line)? {
                    let until = rule.until.map(|u| match u {
                        Until::Date(d) => d.and_hms_opt(23, 59, 59).unwrap_or_default(),
                        Until::Local(l) => l,
                        Until::Utc(u) => shift(u, from),
                    });
                    let x = Expansion {
                        dtstart,
                        until,
                        skip_before: year_start(from_year),
                        stop_after: year_start(to_year.saturating_add(1)).unwrap_or(dtstart),
                        max: 1_000,
                    };
                    onsets.extend(rrule::expand(&rule, &x).into_iter().skip(1));
                }
            }
            for p in obs.props_named("RDATE") {
                for v in split_list(&p.value) {
                    match parse_raw_date(v, false) {
                        Some(RawDate::DateTime { local, utc: false }) => onsets.push(local),
                        Some(RawDate::DateTime { local, utc: true }) => {
                            onsets.push(shift(local, from));
                        }
                        Some(RawDate::Date(d)) => onsets.push(d.and_time(chrono::NaiveTime::MIN)),
                        None => return Err(bad(p.line, "invalid RDATE")),
                    }
                }
            }
            for local in onsets {
                transitions.push((shift(local, -from), from, to));
            }
        }
        if transitions.is_empty() {
            return Err(bad(c.line, "no STANDARD or DAYLIGHT observance"));
        }
        transitions.sort_by_key(|t| t.0);
        transitions.dedup_by_key(|t| t.0);
        Ok(Self {
            tzid,
            initial_offset: transitions[0].1,
            transitions: transitions
                .into_iter()
                .map(|(at, _, to)| (at, to))
                .collect(),
        })
    }

    /// Offset (seconds east of UTC) in effect at the UTC instant `utc`.
    pub fn offset_at(&self, utc: NaiveDateTime) -> i32 {
        let idx = self.transitions.partition_point(|(at, _)| *at <= utc);
        if idx == 0 {
            self.initial_offset
        } else {
            self.transitions[idx - 1].1
        }
    }

    /// UTC instant → wall clock.
    pub fn utc_to_local(&self, utc: NaiveDateTime) -> NaiveDateTime {
        shift(utc, self.offset_at(utc))
    }

    /// Wall clock → UTC instant. Ambiguous times (fall back) take the
    /// earlier instant; times in a gap (spring forward) use the offset in
    /// effect before the gap, so 02:30 becomes 03:30 (RFC 5545 3.3.5).
    pub fn local_to_utc(&self, local: NaiveDateTime) -> NaiveDateTime {
        let day = Duration::days(1);
        let before = self.offset_at(local.checked_sub_signed(day).unwrap_or(local));
        let after = self.offset_at(local.checked_add_signed(day).unwrap_or(local));
        let mut valid: Vec<NaiveDateTime> = [before, after]
            .into_iter()
            .map(|o| shift(local, -o))
            .filter(|utc| self.utc_to_local(*utc) == local)
            .collect();
        valid.sort_unstable();
        valid
            .first()
            .copied()
            .unwrap_or_else(|| shift(local, -before))
    }
}

/// `t + seconds`, saturating at the representable range.
pub(crate) fn shift(t: NaiveDateTime, seconds: i32) -> NaiveDateTime {
    let fallback = if seconds < 0 {
        NaiveDateTime::MIN
    } else {
        NaiveDateTime::MAX
    };
    t.checked_add_signed(Duration::seconds(i64::from(seconds)))
        .unwrap_or(fallback)
}

fn year_start(year: i32) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(year, 1, 1).map(|d| d.and_time(chrono::NaiveTime::MIN))
}

/// How a DATE-TIME's wall-clock value maps to an instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zone<'a> {
    /// The `Z` form, or a TZID that names UTC.
    Utc,
    /// No TZID (or an unknown one): the user's local zone, via the `Clock`.
    Floating,
    /// A `VTIMEZONE` from the same calendar object.
    Defined(&'a VTimeZone),
}

impl Zone<'_> {
    /// Wall clock in this zone → UTC instant.
    pub fn to_utc(self, local: NaiveDateTime, clock: &dyn Clock) -> DateTime<Utc> {
        match self {
            Self::Utc => Utc.from_utc_datetime(&local),
            Self::Floating => clock.local_to_utc(local),
            Self::Defined(tz) => Utc.from_utc_datetime(&tz.local_to_utc(local)),
        }
    }

    /// UTC instant → wall clock in this zone.
    pub fn to_local(self, utc: DateTime<Utc>, clock: &dyn Clock) -> NaiveDateTime {
        match self {
            Self::Utc => utc.naive_utc(),
            Self::Floating => clock.utc_to_local(utc),
            Self::Defined(tz) => tz.utc_to_local(utc.naive_utc()),
        }
    }
}

/// TZIDs that mean UTC even without a `VTIMEZONE`.
fn is_utc_alias(tzid: &str) -> bool {
    let t = tzid.trim().trim_start_matches('/');
    [
        "UTC",
        "Etc/UTC",
        "GMT",
        "Etc/GMT",
        "Z",
        "Zulu",
        "Universal",
        "Etc/Universal",
        "Coordinated Universal Time",
        "UTC+00:00",
    ]
    .iter()
    .any(|a| a.eq_ignore_ascii_case(t))
}

/// Resolves a TZID against the zones defined in the calendar object.
///
/// Order: exact `VTIMEZONE` match, then a case-insensitive match, then UTC
/// aliases, then floating (the user's local zone). The last step is the
/// documented fallback for producers that reference a zone without
/// defining it, which RFC 5545 forbids.
pub(crate) fn resolve<'a>(tzid: Option<&str>, zones: &'a [VTimeZone]) -> Zone<'a> {
    let Some(tzid) = tzid.map(str::trim).filter(|t| !t.is_empty()) else {
        return Zone::Floating;
    };
    if let Some(z) = zones.iter().find(|z| z.tzid == tzid) {
        return Zone::Defined(z);
    }
    if let Some(z) = zones.iter().find(|z| z.tzid.eq_ignore_ascii_case(tzid)) {
        return Zone::Defined(z);
    }
    if is_utc_alias(tzid) {
        return Zone::Utc;
    }
    Zone::Floating
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical::parser::parse_components;

    const COPENHAGEN: &str = "BEGIN:VCALENDAR\r
BEGIN:VTIMEZONE\r
TZID:Europe/Copenhagen\r
BEGIN:DAYLIGHT\r
TZOFFSETFROM:+0100\r
TZOFFSETTO:+0200\r
TZNAME:CEST\r
DTSTART:19700329T020000\r
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r
END:DAYLIGHT\r
BEGIN:STANDARD\r
TZOFFSETFROM:+0200\r
TZOFFSETTO:+0100\r
TZNAME:CET\r
DTSTART:19701025T030000\r
RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r
END:STANDARD\r
END:VTIMEZONE\r
END:VCALENDAR\r
";

    fn zone() -> VTimeZone {
        let cal = parse_components(COPENHAGEN).unwrap();
        VTimeZone::from_component(&cal[0].children[0], 2025, 2027).unwrap()
    }

    fn ndt(y: i32, m: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    #[test]
    fn offsets_follow_dst_rules() {
        let z = zone();
        assert_eq!(z.offset_at(ndt(2026, 1, 15, 12, 0)), 3600);
        assert_eq!(z.offset_at(ndt(2026, 7, 15, 12, 0)), 7200);
        // 2026-03-29 01:00Z is the switch.
        assert_eq!(z.offset_at(ndt(2026, 3, 29, 0, 59)), 3600);
        assert_eq!(z.offset_at(ndt(2026, 3, 29, 1, 0)), 7200);
        // 2026-10-25 01:00Z switches back.
        assert_eq!(z.offset_at(ndt(2026, 10, 25, 0, 59)), 7200);
        assert_eq!(z.offset_at(ndt(2026, 10, 25, 1, 0)), 3600);
    }

    #[test]
    fn local_to_utc_handles_gap_and_overlap() {
        let z = zone();
        assert_eq!(
            z.local_to_utc(ndt(2026, 11, 16, 9, 0)),
            ndt(2026, 11, 16, 8, 0)
        );
        assert_eq!(
            z.local_to_utc(ndt(2026, 6, 16, 9, 0)),
            ndt(2026, 6, 16, 7, 0)
        );
        // Gap: 02:30 does not exist; interpreted with the pre-gap offset.
        assert_eq!(
            z.local_to_utc(ndt(2026, 3, 29, 2, 30)),
            ndt(2026, 3, 29, 1, 30)
        );
        // Overlap: 02:30 happens twice; earlier instant wins.
        assert_eq!(
            z.local_to_utc(ndt(2026, 10, 25, 2, 30)),
            ndt(2026, 10, 25, 0, 30)
        );
    }

    #[test]
    fn resolution_order() {
        let zones = vec![zone()];
        assert!(matches!(
            resolve(Some("Europe/Copenhagen"), &zones),
            Zone::Defined(_)
        ));
        assert!(matches!(
            resolve(Some("europe/copenhagen"), &zones),
            Zone::Defined(_)
        ));
        assert_eq!(resolve(Some("Etc/UTC"), &zones), Zone::Utc);
        assert_eq!(resolve(Some("Mars/Olympus"), &zones), Zone::Floating);
        assert_eq!(resolve(None, &zones), Zone::Floating);
    }

    #[test]
    fn missing_offsets_are_errors() {
        let cal = parse_components(
            "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:X\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();
        assert!(VTimeZone::from_component(&cal[0].children[0], 2026, 2026).is_err());
    }
}
