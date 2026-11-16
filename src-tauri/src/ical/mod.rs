//! iCalendar (RFC 5545) parser for CalDAV `calendar-data` (spec 6.3).
//!
//! [`parse_events`] turns iCalendar text into domain [`Event`]s that overlap
//! a time window, expanding recurring events. It is pure: no I/O, no
//! logging, and time zone and "now" questions go through the injected
//! [`Clock`].
//!
//! # Supported
//!
//! * Line unfolding (CRLF or LF, space or tab), quoted parameters, TEXT
//!   unescaping, several `VEVENT`s and several `VCALENDAR`s per input.
//! * `DTSTART` / `DTEND` / `DURATION`. Without either, a DATE event lasts one
//!   day and a DATE-TIME event is instantaneous (RFC 5545 3.6.1).
//!   `VALUE=DATE` events become
//!   [`EventTime::AllDay`](crate::domain::EventTime::AllDay) (end exclusive).
//! * UTC (`Z`), floating times (interpreted in the user's zone through the
//!   [`Clock`]) and `TZID` resolved against the `VTIMEZONE`s embedded in the
//!   same `VCALENDAR` (their STANDARD/DAYLIGHT `RRULE`s and `RDATE`s are
//!   interpreted). This covers IANA and Windows-style TZIDs alike, because
//!   servers (iCloud, Nextcloud, Exchange/Outlook) embed the definition. A
//!   TZID without a `VTIMEZONE` is treated as UTC if it names UTC, and as
//!   floating otherwise.
//! * `RRULE` with FREQ DAILY/WEEKLY/MONTHLY/YEARLY, INTERVAL, COUNT, UNTIL,
//!   BYDAY (with ordinals), BYMONTHDAY, BYMONTH, BYSETPOS and WKST,
//!   evaluated in the event's own zone so DST keeps the wall-clock time.
//!   Rules with parts outside this set (SECONDLY/MINUTELY/HOURLY, BYHOUR,
//!   BYMINUTE, BYSECOND, BYWEEKNO, BYYEARDAY, non-Gregorian RSCALE) yield
//!   only DTSTART and the `RDATE`s.
//! * `EXDATE` (several properties and lists, UTC/TZID/floating/DATE),
//!   `RDATE` (DATE, DATE-TIME, and the start of PERIOD values).
//! * Overridden occurrences (`RECURRENCE-ID`), including
//!   `RANGE=THISANDFUTURE` (later occurrences take the override's time shift,
//!   duration, title, location and response).
//! * `STATUS:CANCELLED` on the series or on an override omits it.
//! * `SUMMARY`, `LOCATION`, and the user's response from `ORGANIZER` /
//!   `ATTENDEE;PARTSTAT` matched against [`ParseOptions::user_addresses`].
//!   An override without `SUMMARY`, `LOCATION` or attendees inherits them
//!   from the series.
//!
//! # Not supported
//!
//! * `VTODO`, `VJOURNAL`, `VFREEBUSY` and `VALARM` are ignored.
//! * TZIDs without an embedded `VTIMEZONE` are not looked up in a time zone
//!   database (no `chrono-tz`); see above.
//! * Input is a `&str`: a producer that folds lines in the middle of a
//!   multi-byte UTF-8 sequence (allowed by RFC 5545, rare in practice) must
//!   be unfolded on bytes by the caller before decoding.
//!
//! # Event ids
//!
//! A non-recurring event's id is its `UID`. An occurrence of a recurring
//! event (one with `RRULE`/`RDATE`, or with overrides) gets
//! `UID/<original start as RFC 3339 UTC>`. A moved occurrence keeps the id
//! of its original slot (its `RECURRENCE-ID`), so reminders do not fire
//! again when a meeting is moved. All-day occurrences use the date at
//! midnight UTC, e.g. `uid/2026-11-16T00:00:00Z`.
//!
//! # Robustness
//!
//! Malformed input returns [`Error::Protocol`]; error messages contain line
//! numbers and property names only, never event contents (spec section 9).
//! Expansion is capped per rule ([`MAX_OCCURRENCES_PER_EVENT`] occurrences,
//! and a bounded number of periods walked), so runaway rules terminate.

mod event;
mod parser;
mod rrule;
mod tz;
mod value;

use chrono::{DateTime, Datelike, Utc};

use crate::domain::{AccountId, CalendarId, Clock, Error, Event, Result};

/// The most occurrences produced for one recurring event in one call.
pub const MAX_OCCURRENCES_PER_EVENT: usize = 1_000;

/// Context for [`parse_events`].
#[derive(Clone, Copy)]
pub struct ParseOptions<'a> {
    /// Stamped on every returned [`Event`].
    pub account_id: &'a AccountId,
    /// Stamped on every returned [`Event`].
    pub calendar_id: &'a CalendarId,
    /// The account user's calendar addresses, used to find their
    /// `ORGANIZER`/`ATTENDEE` entry. Email addresses, with or without the
    /// `mailto:` prefix; matched case-insensitively against the property
    /// value and its `EMAIL` parameter. May be empty, in which case every
    /// response is [`ResponseStatus::Unknown`](crate::domain::ResponseStatus).
    pub user_addresses: &'a [String],
    /// Source of the user's local time zone, for floating times and
    /// all-day events.
    pub clock: &'a dyn Clock,
}

impl std::fmt::Debug for ParseOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseOptions")
            .field("account_id", self.account_id)
            .field("calendar_id", self.calendar_id)
            .field("user_addresses", &self.user_addresses.len())
            .finish_non_exhaustive()
    }
}

/// Parses iCalendar text and returns the event occurrences that overlap the
/// half-open window `[from, to)`, sorted by start then id.
///
/// `ical` may hold one or more `VCALENDAR` objects (for CalDAV, typically
/// one `calendar-data` element per call). Zones are resolved per
/// `VCALENDAR`; overrides are matched to their master by `UID` within the
/// same `VCALENDAR`.
///
/// # Errors
///
/// [`Error::Protocol`] if the input is not well-formed iCalendar or a
/// `VEVENT` lacks `UID`/`DTSTART` or has an unparsable date, duration or
/// rule. The whole call fails; no partial result is returned.
pub fn parse_events(
    ical: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    opts: &ParseOptions<'_>,
) -> Result<Vec<Event>> {
    let calendars = parser::parse_components(ical)?;
    if from >= to {
        return Ok(Vec::new());
    }
    let users: Vec<String> = opts
        .user_addresses
        .iter()
        .map(|a| event::normalize_address(a))
        .filter(|a| !a.is_empty())
        .collect();

    let mut out = Vec::new();
    for cal in &calendars {
        let zones = cal
            .children_named("VTIMEZONE")
            .map(|c| {
                tz::VTimeZone::from_component(
                    c,
                    from.year().saturating_sub(1),
                    to.year().saturating_add(1),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let ctx = event::Context {
            from,
            to,
            opts,
            users: &users,
            zones: &zones,
        };
        event::expand_calendar(cal, &ctx, &mut out)?;
    }
    out.sort_by(|a, b| {
        a.time
            .start_utc(opts.clock)
            .cmp(&b.time.start_utc(opts.clock))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Returns a [`Error::Protocol`] that names only the location of the
/// problem.
fn protocol(line: usize, what: &str) -> Error {
    Error::protocol(format!("invalid VEVENT data at line {line}: {what}"))
}
