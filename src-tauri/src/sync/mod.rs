//! Calendar sync engine (spec 6.4, 6.5).
//!
//! Fetches upcoming events from every configured account on one worker
//! thread, stores each account's result in a [`SyncSink`] (the SQLite event
//! cache in the app), merges all accounts into one sorted, de-duplicated list
//! and reports progress through a [`SyncListener`]. Nothing here depends on
//! Tauri or SQLite: the app plugs in a [`ProviderFactory`] (CalDAV, Graph), a
//! [`SyncSink`] (storage) and a [`SyncListener`] (IPC events, buddy state).
//!
//! # Pieces
//!
//! * [`merge_events`]: pure merge, included-calendar filter, de-duplication
//!   and sort.
//! * [`SyncEngine`]: single-threaded sync logic. [`SyncEngine::tick`] runs
//!   whatever is due at the clock's "now" and returns when to wake next, so
//!   intervals, backoff and triggers are tested with a [`FakeClock`] and no
//!   threads.
//! * [`SyncWorker`]: owns the engine on a dedicated thread and sleeps on one
//!   condition variable until the next deadline or a trigger
//!   ([`SyncHandle::sync_now`], [`SyncHandle::notify_resumed`],
//!   [`SyncHandle::update_settings`]).
//!
//! # When a sync runs
//!
//! A *full sync* (every account) runs on start, every
//! `reminders.syncIntervalMinutes` (clamped to 5..=15), on
//! [`SyncHandle::sync_now`], on resume from sleep, and when the account list
//! changes in a way that matters (an account added or removed, its connection
//! or included calendars changed). Syncing ignores the "pause reminders"
//! toggle (spec 6.5).
//!
//! # Failures and backoff
//!
//! A failing account never fails the others. Its cache is left untouched
//! (the sink's `replace_events` is not called), its status becomes
//! [`SyncState::Error`] with the error, and:
//!
//! * a retryable error ([`Error::is_retryable`], i.e. network) retries only
//!   that account after 30 s, 1 min, 2 min, 4 min, ... capped at the sync
//!   interval;
//! * any other error (auth, protocol, keychain, ...) waits for the next full
//!   sync. The account's provider is dropped, so the next attempt re-creates
//!   it through the factory and picks up new credentials.
//!
//! A provider or factory that panics is treated like an
//! [`Error::Internal`](crate::domain::Error::Internal) of that account; the
//! sync thread keeps running.
//!
//! [`FakeClock`]: crate::domain::FakeClock
//! [`SyncState::Error`]: crate::domain::ipc::SyncState::Error
//! [`Error::is_retryable`]: crate::domain::Error::is_retryable

mod engine;
mod memory;
mod merge;
mod worker;

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::domain::ipc::SyncState;
use crate::domain::{AccountConfig, AccountId, BuddyEvent, CalendarProvider, Error, Event, Result};

pub use engine::{SyncConfig, SyncEngine, TickOutcome, Triggers};
pub use memory::InMemorySink;
pub use merge::merge_events;
pub use worker::{SyncHandle, SyncWorker};

/// Creates the [`CalendarProvider`] for an account (CalDAV or Graph, with its
/// secret from the keychain). Called on the sync thread, at the first sync of
/// an account, after its connection settings change, and after a
/// non-retryable error.
pub trait ProviderFactory: Send + Sync {
    /// Builds a provider for `account`. An error (e.g. a missing keychain
    /// secret) is recorded as that account's sync error.
    fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>>;
}

/// Where sync results go: the event cache and the per-account sync record.
/// Implemented by storage in the app and by [`InMemorySink`] in tests.
/// Called only from the sync thread.
pub trait SyncSink: Send + Sync {
    /// Replaces **all** cached events of `account` with `events` (the
    /// account's included calendars, fetched for the sync window). Called
    /// only after a successful fetch. Every event belongs to `account`.
    fn replace_events(&self, account: &AccountId, events: &[Event]) -> Result<()>;

    /// Records the outcome of one sync attempt for `account` (the
    /// `sync_state` table). Called after every attempt, success or failure.
    fn record_sync(&self, account: &AccountId, outcome: &SyncOutcome) -> Result<()>;

    /// All cached events of all accounts, in any order. The engine filters
    /// them by the current settings and merges them after each sync.
    fn cached_events(&self) -> Result<Vec<Event>>;
}

/// The outcome of one sync attempt of one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    /// When the attempt started.
    pub attempted_at: DateTime<Utc>,
    /// `Ok` or the error. Error messages never contain secrets or event
    /// contents (see [`Error`]).
    pub result: std::result::Result<(), Error>,
}

/// Runtime sync status of one account, for the Settings account rows
/// (combine with the config and keychain state into
/// [`AccountStatus`](crate::domain::ipc::AccountStatus)) and the tray error
/// indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSyncStatus {
    /// The account.
    pub account_id: AccountId,
    /// Current state. `NeverSynced` also when no calendar is included.
    pub sync_state: SyncState,
    /// When the last successful sync of this account finished (since the
    /// engine started).
    pub last_synced_at: Option<DateTime<Utc>>,
    /// The last error, cleared on success.
    pub last_error: Option<Error>,
    /// When a retryable failure will be retried, if scheduled.
    pub next_retry_at: Option<DateTime<Utc>>,
}

impl AccountSyncStatus {
    fn new(account_id: AccountId) -> Self {
        Self {
            account_id,
            sync_state: SyncState::NeverSynced,
            last_synced_at: None,
            last_error: None,
            next_retry_at: None,
        }
    }
}

/// Something the sync engine reports.
///
/// Order within one sync run: `Started`, then per account
/// `AccountStatus(Syncing)` and `AccountStatus(Ok | Error)`, then
/// `EventsChanged` (only if the merged list changed), then `Ended`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncUpdate {
    /// A sync run started (the cat enters `thinking`).
    Started,
    /// The sync run ended, successfully or not.
    Ended,
    /// One account's status changed.
    AccountStatus(AccountSyncStatus),
    /// The merged, filtered, de-duplicated and sorted event list changed
    /// (see [`merge_events`]). Feed it to the scheduler. Also sent when the
    /// account settings change the list without a fetch (e.g. an account was
    /// removed).
    EventsChanged(Vec<Event>),
}

impl SyncUpdate {
    /// The buddy state-machine event for this update, if any.
    pub fn buddy_event(&self) -> Option<BuddyEvent> {
        match self {
            Self::Started => Some(BuddyEvent::SyncStarted),
            Self::Ended => Some(BuddyEvent::SyncEnded),
            Self::AccountStatus(_) | Self::EventsChanged(_) => None,
        }
    }
}

/// Receives [`SyncUpdate`]s, on the sync thread. Implementations must return
/// quickly (forward to the UI or the scheduler; do not block on the network).
pub trait SyncListener: Send + Sync {
    /// Called for every update, in order.
    fn on_update(&self, update: SyncUpdate);
}

#[cfg(test)]
mod tests;
