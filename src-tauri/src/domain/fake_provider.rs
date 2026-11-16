//! In-memory [`CalendarProvider`] for tests.

use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};

use super::{
    AccountId, CalendarId, CalendarInfo, CalendarProvider, Error, Event, EventTime, Result,
};

#[derive(Debug, Default)]
struct State {
    calendars: Vec<CalendarInfo>,
    events: Vec<Event>,
    failure: Option<Error>,
    fetch_count: usize,
}

/// A configurable fake calendar provider for unit and integration tests.
///
/// Holds calendars and events in memory, filters them like a real provider
/// would, can be told to fail every call with a given error, and counts
/// `fetch_events` calls. All setters take `&self`, so a test can keep an
/// `Arc<FakeCalendarProvider>` and change it while the code under test holds
/// another reference.
#[derive(Debug)]
pub struct FakeCalendarProvider {
    account_id: AccountId,
    state: Mutex<State>,
}

impl FakeCalendarProvider {
    /// An empty, healthy provider for `account_id`.
    pub fn new(account_id: impl Into<AccountId>) -> Self {
        Self {
            account_id: account_id.into(),
            state: Mutex::new(State::default()),
        }
    }

    /// Builder: sets the calendars.
    pub fn with_calendars(self, calendars: Vec<CalendarInfo>) -> Self {
        self.set_calendars(calendars);
        self
    }

    /// Builder: sets the events.
    pub fn with_events(self, events: Vec<Event>) -> Self {
        self.set_events(events);
        self
    }

    /// Replaces the calendars.
    pub fn set_calendars(&self, calendars: Vec<CalendarInfo>) {
        self.state().calendars = calendars;
    }

    /// Replaces the events.
    pub fn set_events(&self, events: Vec<Event>) {
        self.state().events = events;
    }

    /// Makes every subsequent call fail with `error` until
    /// [`clear_failure`](Self::clear_failure) is called.
    pub fn fail_with(&self, error: Error) {
        self.state().failure = Some(error);
    }

    /// Makes calls succeed again.
    pub fn clear_failure(&self) {
        self.state().failure = None;
    }

    /// Number of `fetch_events` calls so far, including failed ones.
    pub fn fetch_count(&self) -> usize {
        self.state().fetch_count
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn check_failure(state: &State) -> Result<()> {
        match &state.failure {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

/// Half-open overlap with `[from, to)`. Zero-length events count if they
/// start inside the window. All-day dates are compared in UTC.
fn overlaps(time: &EventTime, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    match *time {
        EventTime::Timed { start, end } => start < to && (end > from || start >= from),
        EventTime::AllDay { start, end } => start <= to.date_naive() && end > from.date_naive(),
    }
}

impl CalendarProvider for FakeCalendarProvider {
    fn account_id(&self) -> &AccountId {
        &self.account_id
    }

    fn list_calendars(&self) -> Result<Vec<CalendarInfo>> {
        let state = self.state();
        Self::check_failure(&state)?;
        Ok(state.calendars.clone())
    }

    fn fetch_events(
        &self,
        calendar_ids: &[CalendarId],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>> {
        let mut state = self.state();
        state.fetch_count += 1;
        Self::check_failure(&state)?;
        Ok(state
            .events
            .iter()
            .filter(|e| calendar_ids.contains(&e.calendar_id) && overlaps(&e.time, from, to))
            .cloned()
            .collect())
    }

    fn test_connection(&self) -> Result<()> {
        Self::check_failure(&self.state())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EventId, ResponseStatus};
    use chrono::{NaiveDate, TimeZone};

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, h, 0, 0).unwrap()
    }

    fn event(id: &str, cal: &str, time: EventTime) -> Event {
        Event {
            id: EventId::new(id),
            account_id: AccountId::new("acc"),
            calendar_id: CalendarId::new(cal),
            title: format!("Event {id}"),
            time,
            location: None,
            response: ResponseStatus::Accepted,
        }
    }

    fn provider() -> FakeCalendarProvider {
        FakeCalendarProvider::new("acc")
            .with_calendars(vec![CalendarInfo {
                id: CalendarId::new("work"),
                account_id: AccountId::new("acc"),
                name: "Work".into(),
                color: None,
            }])
            .with_events(vec![
                event(
                    "in",
                    "work",
                    EventTime::Timed {
                        start: t(9),
                        end: t(10),
                    },
                ),
                event(
                    "other-cal",
                    "home",
                    EventTime::Timed {
                        start: t(9),
                        end: t(10),
                    },
                ),
                event(
                    "after",
                    "work",
                    EventTime::Timed {
                        start: t(20),
                        end: t(21),
                    },
                ),
                event(
                    "all-day",
                    "work",
                    EventTime::AllDay {
                        start: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
                        end: NaiveDate::from_ymd_opt(2026, 11, 17).unwrap(),
                    },
                ),
            ])
    }

    #[test]
    fn filters_by_calendar_and_window() {
        let p = provider();
        let ids: Vec<String> = p
            .fetch_events(&[CalendarId::new("work")], t(8), t(12))
            .unwrap()
            .into_iter()
            .map(|e| e.id.into_inner())
            .collect();
        assert_eq!(ids, ["in", "all-day"]);
        assert_eq!(p.fetch_count(), 1);
        assert!(p.fetch_events(&[], t(0), t(23)).unwrap().is_empty());
    }

    #[test]
    fn can_be_told_to_fail() {
        let p = provider();
        p.fail_with(Error::network("offline"));
        assert_eq!(p.test_connection(), Err(Error::network("offline")));
        assert!(p.list_calendars().is_err());
        assert!(p
            .fetch_events(&[CalendarId::new("work")], t(0), t(23))
            .is_err());
        assert_eq!(p.fetch_count(), 1);
        p.clear_failure();
        assert!(p.test_connection().is_ok());
        assert_eq!(p.list_calendars().unwrap().len(), 1);
        assert_eq!(p.account_id(), &AccountId::new("acc"));
    }

    #[test]
    fn usable_as_trait_object() {
        let providers: Vec<Box<dyn CalendarProvider>> = vec![Box::new(provider())];
        assert!(providers[0].test_connection().is_ok());
    }
}
