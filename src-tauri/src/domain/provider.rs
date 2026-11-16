//! Data source traits: calendars (spec 6.1) and tasks (spec 7.3).
//!
//! # Why the traits are blocking (not `async`)
//!
//! * Calls are rare (one sync every 5-15 minutes) and latency-insensitive, so
//!   a blocking call on a worker thread costs nothing measurable.
//! * Blocking traits are object-safe without the `async-trait` crate or boxed
//!   futures, so the sync engine can hold `Vec<Box<dyn CalendarProvider>>`
//!   for a mix of Graph and CalDAV accounts.
//! * Implementations can use a small blocking HTTP client instead of a
//!   hyper/reqwest stack, which helps the memory target.
//! * Tests (fake provider, recorded fixtures) need no runtime.
//!
//! Callers must not invoke these methods on the Tauri main thread or inside
//! an async task directly. Use a dedicated thread or
//! `tauri::async_runtime::spawn_blocking` (Tauri already ships tokio).

use chrono::{DateTime, Utc};

use super::{AccountId, CalendarId, CalendarInfo, Event, Result, Task};

/// Read-only access to one calendar account (Graph or CalDAV).
///
/// One instance per configured account. The rest of the app depends only on
/// this trait. Implementations hold their own credentials (loaded from the
/// keychain) and refresh tokens internally, using interior mutability where
/// needed.
pub trait CalendarProvider: Send + Sync {
    /// The account this provider serves. Every returned [`CalendarInfo`] and
    /// [`Event`] carries this id.
    fn account_id(&self) -> &AccountId;

    /// Lists all calendars in the account (not only the included ones).
    fn list_calendars(&self) -> Result<Vec<CalendarInfo>>;

    /// Fetches event occurrences from the given calendars that overlap the
    /// half-open window `[from, to)`.
    ///
    /// * Recurring events are expanded into occurrences (`RRULE`, `EXDATE`,
    ///   overridden instances); each has a distinct [`EventId`](super::EventId).
    /// * Cancelled occurrences are omitted.
    /// * All-day events are returned as [`EventTime::AllDay`](super::EventTime)
    ///   if any of their dates overlaps the window (interpreting dates in
    ///   UTC is acceptable; the scheduler filters precisely).
    /// * Declined events are included, with
    ///   [`ResponseStatus::Declined`](super::ResponseStatus); filtering is the
    ///   caller's job.
    /// * Order is unspecified. An empty `calendar_ids` returns an empty list.
    /// * Fails as a whole: a partial result is never returned.
    fn fetch_events(
        &self,
        calendar_ids: &[CalendarId],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>>;

    /// Checks that the server is reachable and the credentials work.
    fn test_connection(&self) -> Result<()>;
}

/// A source of tasks. v1 has only the local SQLite-backed source; external
/// sources (Microsoft To Do, VTODO) can be added later by implementing this
/// trait, without touching the scheduler.
pub trait TaskSource: Send + Sync {
    /// Returns all tasks, open and done, in unspecified order.
    fn fetch_tasks(&self) -> Result<Vec<Task>>;
}
