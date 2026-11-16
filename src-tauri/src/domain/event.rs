//! Calendar events and calendars.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use super::{AccountId, CalendarId, Clock, EventId};

/// A calendar offered by an account, as listed by
/// [`CalendarProvider::list_calendars`](super::CalendarProvider::list_calendars).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarInfo {
    /// Provider-assigned id, unique within the account.
    pub id: CalendarId,
    /// The account the calendar belongs to.
    pub account_id: AccountId,
    /// Display name.
    pub name: String,
    /// Display color as `#rrggbb`, if the provider reports one.
    pub color: Option<String>,
}

/// Globally unique reference to one event occurrence: the triple
/// (account, calendar, event occurrence id).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventKey {
    /// The owning account.
    pub account_id: AccountId,
    /// The owning calendar.
    pub calendar_id: CalendarId,
    /// The occurrence id within the calendar.
    pub event_id: EventId,
}

/// When an event happens.
///
/// Timed events are fixed instants. All-day events are floating calendar
/// dates that start at local midnight in whatever time zone the user is in,
/// so they are kept as dates and turned into instants only through a
/// [`Clock`].
///
/// Serializes as `{"kind": "timed", "start": "...Z", "end": "...Z"}` or
/// `{"kind": "allDay", "start": "2026-11-16", "end": "2026-11-17"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EventTime {
    /// A timed event. `start <= end`.
    Timed {
        /// Start instant.
        start: DateTime<Utc>,
        /// End instant.
        end: DateTime<Utc>,
    },
    /// An all-day event covering the dates `start..end` (end **exclusive**,
    /// as in iCalendar `DTEND;VALUE=DATE` and Graph). A one-day event on
    /// 16 Nov has `start = 2026-11-16`, `end = 2026-11-17`. `start < end`.
    AllDay {
        /// First day.
        start: NaiveDate,
        /// Day after the last day.
        end: NaiveDate,
    },
}

impl EventTime {
    /// Whether this is an all-day event.
    pub fn is_all_day(&self) -> bool {
        matches!(self, Self::AllDay { .. })
    }

    /// Start instant. For all-day events: local midnight of the start date,
    /// in the clock's time zone.
    pub fn start_utc<C: Clock + ?Sized>(&self, clock: &C) -> DateTime<Utc> {
        match *self {
            Self::Timed { start, .. } => start,
            Self::AllDay { start, .. } => clock.local_to_utc(start.and_time(NaiveTime::MIN)),
        }
    }

    /// End instant. For all-day events: local midnight of the (exclusive)
    /// end date, in the clock's time zone.
    pub fn end_utc<C: Clock + ?Sized>(&self, clock: &C) -> DateTime<Utc> {
        match *self {
            Self::Timed { end, .. } => end,
            Self::AllDay { end, .. } => clock.local_to_utc(end.and_time(NaiveTime::MIN)),
        }
    }
}

/// The user's response to an event invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseStatus {
    /// The user organizes the event.
    Organizer,
    /// Accepted.
    Accepted,
    /// Tentatively accepted.
    Tentative,
    /// Declined. Hidden from reminders when "skip declined" is on.
    Declined,
    /// Invited but not yet responded.
    NotResponded,
    /// The provider gave no response information (e.g. an event in the
    /// user's own calendar without attendees).
    #[default]
    Unknown,
}

/// One occurrence of a calendar event.
///
/// Recurring events are expanded by the provider; each occurrence is a
/// separate `Event` with its own [`EventId`]. Cancelled occurrences are never
/// returned. Event contents (title, location) must not be logged at default
/// log levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Occurrence id, unique within the calendar.
    pub id: EventId,
    /// The owning account.
    pub account_id: AccountId,
    /// The owning calendar.
    pub calendar_id: CalendarId,
    /// Title / subject. Empty string if the event has none.
    pub title: String,
    /// When the event happens.
    pub time: EventTime,
    /// Location, if present and non-empty.
    pub location: Option<String>,
    /// The user's response status.
    pub response: ResponseStatus,
}

impl Event {
    /// The globally unique key of this occurrence.
    pub fn key(&self) -> EventKey {
        EventKey {
            account_id: self.account_id.clone(),
            calendar_id: self.calendar_id.clone(),
            event_id: self.id.clone(),
        }
    }

    /// Whether the user declined this event.
    pub fn is_declined(&self) -> bool {
        self.response == ResponseStatus::Declined
    }

    /// Whether this is an all-day event.
    pub fn is_all_day(&self) -> bool {
        self.time.is_all_day()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FakeClock;
    use chrono::{FixedOffset, TimeZone};

    #[test]
    fn all_day_resolves_to_local_midnight() {
        let clock = FakeClock::with_offset(
            Utc.with_ymd_and_hms(2026, 11, 16, 0, 0, 0).unwrap(),
            FixedOffset::east_opt(2 * 3600).unwrap(),
        );
        let t = EventTime::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 11, 17).unwrap(),
        };
        assert!(t.is_all_day());
        assert_eq!(
            t.start_utc(&clock),
            Utc.with_ymd_and_hms(2026, 11, 15, 22, 0, 0).unwrap()
        );
        assert_eq!(
            t.end_utc(&clock),
            Utc.with_ymd_and_hms(2026, 11, 16, 22, 0, 0).unwrap()
        );
    }

    #[test]
    fn event_time_json_shape() {
        let t = EventTime::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 11, 17).unwrap(),
        };
        assert_eq!(
            serde_json::to_value(t).unwrap(),
            serde_json::json!({"kind": "allDay", "start": "2026-11-16", "end": "2026-11-17"})
        );
        let timed = EventTime::Timed {
            start: Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 11, 16, 9, 30, 0).unwrap(),
        };
        assert_eq!(
            serde_json::to_value(timed).unwrap(),
            serde_json::json!({"kind": "timed", "start": "2026-11-16T09:00:00Z", "end": "2026-11-16T09:30:00Z"})
        );
    }
}
