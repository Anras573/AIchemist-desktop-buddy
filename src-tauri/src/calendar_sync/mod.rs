//! Calendar sync wiring (task 3.6): the [`SyncEngine`] fed by the real
//! providers, writing the SQLite event cache and driving the scheduler, the
//! cat, the account rows and the tray (spec 6.5).
//!
//! # Pieces
//!
//! - [`StorageSink`]: the engine's [`SyncSink`] over [`Storage`] (event
//!   cache and `sync_state` rows).
//! - [`Listener`]: the engine's [`SyncListener`]; turns [`SyncUpdate`]s into
//!   [`SyncOutlet`] calls. Tauri-free, so it is tested with a fake outlet.
//! - [`start`]: builds the engine and its worker thread. [`init`] calls it
//!   with the app's services and keeps the [`CalendarSync`] in managed state.
//!
//! # What happens where
//!
//! - `Started` / `Ended`: the cat enters / leaves `thinking`
//!   (`BuddyEvent::SyncStarted` / `SyncEnded`) and the reminder service is
//!   told a sync is running (so a bubble closed mid-sync returns the cat to
//!   `thinking`). `Ended` also emits `upcoming-changed` (the popover shows
//!   "last synced").
//! - `AccountStatus`: [`crate::accounts::set_sync_status`] (account rows via
//!   `accounts-changed`, tray error line). The engine only knows successes
//!   since the app started, so a missing `last_synced_at` is filled from the
//!   `sync_state` table.
//! - `EventsChanged`: [`crate::reminders::events_changed`] (the scheduler)
//!   and `upcoming-changed`.
//!
//! # Offline
//!
//! The reminder service loads the cached events into the scheduler at
//! startup ([`crate::reminders::Reminders::start`]), before any sync, so
//! meeting reminders fire without a network. The engine's first sync then
//! replaces that list with the merged, filtered cache, even when every
//! account fails (a failed account keeps its cache).
//!
//! # Resume from sleep
//!
//! Tauri/tao report `Resumed` only on mobile, so desktop builds rely on the
//! worker's own detection: it wakes at least once a minute and treats a
//! wall-clock jump against the monotonic clock as a resume. Where the
//! monotonic clock also counts sleep, no jump is seen; the regular sync
//! then runs at most a minute after wake-up if its interval has passed.
//!
//! # Threads
//!
//! Everything here runs on the `calendar-sync` thread except [`init`] and
//! [`sync_now`]. The worker is never joined: a slow server must not delay
//! quitting, and the thread ends with the process.

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::domain::ipc::{event, SyncState};
use crate::domain::{AccountId, BuddyEvent, CalendarId, Clock, Event, Result, Settings};
use crate::storage::Storage;
use crate::sync::{
    AccountSyncStatus, InMemorySink, ProviderFactory, SyncConfig, SyncEngine, SyncHandle,
    SyncListener, SyncOutcome, SyncSink, SyncUpdate, SyncWorker,
};
use crate::BUDDY_WINDOW_LABEL;

/// Whether an account is still configured. Guards cache writes that race
/// with the removal of an account.
pub type AccountFilter = Arc<dyn Fn(&AccountId) -> bool + Send + Sync>;

/// [`SyncSink`] over the app database.
pub struct StorageSink {
    storage: Storage,
    window: Duration,
    configured: AccountFilter,
}

impl StorageSink {
    /// A sink writing to `storage`. `cached_events` returns the events of
    /// `[now, now + window)`; `configured` drops writes for accounts that
    /// were removed while their sync was running.
    pub fn new(storage: Storage, window: Duration, configured: AccountFilter) -> Self {
        Self {
            storage,
            window,
            configured,
        }
    }
}

impl SyncSink for StorageSink {
    fn replace_events(&self, account: &AccountId, events: &[Event]) -> Result<()> {
        if !(self.configured)(account) {
            // Removed mid-sync: `remove_account` already cleared its cache.
            return Ok(());
        }
        // The engine hands over only events of the account's included
        // calendars; the account's whole cache is replaced either way.
        let calendars: Vec<CalendarId> = events
            .iter()
            .map(|e| e.calendar_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.storage.replace_events(account, &calendars, events)
    }

    fn record_sync(&self, account: &AccountId, outcome: &SyncOutcome) -> Result<()> {
        if !(self.configured)(account) {
            return Ok(());
        }
        // Storage stamps with its own clock (the attempt is recorded when it
        // ends; `docs/storage.md` has no column for the start time).
        self.storage.record_sync_attempt(account)?;
        match &outcome.result {
            Ok(()) => self.storage.record_sync_success(account),
            Err(e) => self.storage.record_sync_failure(account, e),
        }
    }

    fn cached_events(&self) -> Result<Vec<Event>> {
        let now = self.storage.clock().now();
        self.storage.events_in_window(now, now + self.window)
    }
}

/// Where the sync wiring reports to. Called on the sync thread; must return
/// quickly.
pub trait SyncOutlet: Send + Sync {
    /// Feed an event to the buddy state machine.
    fn buddy_event(&self, event: BuddyEvent);
    /// Tell the reminder service whether a sync runs.
    fn sync_in_progress(&self, running: bool);
    /// An account's sync status changed (account rows, tray).
    fn account_status(&self, status: AccountSyncStatus);
    /// Hand the merged event list to the scheduler.
    fn events_changed(&self, events: Vec<Event>);
    /// Ask an open popover to refetch.
    fn upcoming_changed(&self);
}

/// [`SyncListener`] that forwards to a [`SyncOutlet`].
pub struct Listener {
    storage: Option<Storage>,
    outlet: Arc<dyn SyncOutlet>,
    in_run: AtomicBool,
}

impl Listener {
    /// A listener filling missing last-synced times from `storage`.
    pub fn new(storage: Option<Storage>, outlet: Arc<dyn SyncOutlet>) -> Self {
        Self {
            storage,
            outlet,
            in_run: AtomicBool::new(false),
        }
    }

    fn stored_last_synced(&self, status: &mut AccountSyncStatus) {
        if status.last_synced_at.is_some() {
            return;
        }
        if let Some(storage) = &self.storage {
            match storage.sync_state(&status.account_id) {
                Ok(record) => status.last_synced_at = record.and_then(|r| r.last_success_at),
                Err(e) => log::warn!("cannot read the stored sync state: {e}"),
            }
        }
    }
}

impl SyncListener for Listener {
    fn on_update(&self, update: SyncUpdate) {
        let buddy = update.buddy_event();
        match update {
            SyncUpdate::Started => {
                self.in_run.store(true, Ordering::SeqCst);
                self.outlet.sync_in_progress(true);
            }
            SyncUpdate::Ended => {
                self.in_run.store(false, Ordering::SeqCst);
                self.outlet.sync_in_progress(false);
                self.outlet.upcoming_changed();
            }
            SyncUpdate::AccountStatus(mut status) => {
                log_status(&status);
                self.stored_last_synced(&mut status);
                self.outlet.account_status(status);
            }
            SyncUpdate::EventsChanged(events) => {
                self.outlet.events_changed(events);
                // Inside a run, `Ended` announces it.
                if !self.in_run.load(Ordering::SeqCst) {
                    self.outlet.upcoming_changed();
                }
            }
        }
        if let Some(e) = buddy {
            self.outlet.buddy_event(e);
        }
    }
}

/// Logs the end of an account's sync. Ids, error kinds and provider
/// messages only: never URLs, user names or event contents (spec 9).
fn log_status(status: &AccountSyncStatus) {
    let id = &status.account_id;
    match (status.sync_state, &status.last_error) {
        (SyncState::Ok, _) => log::debug!("account {id} synced"),
        (SyncState::Error, Some(e)) => match status.next_retry_at {
            Some(at) => log::warn!("account {id} sync failed: {e}; retrying at {at}"),
            None => log::warn!("account {id} sync failed: {e}; next try at the next sync"),
        },
        _ => {}
    }
}

/// The running sync: the worker thread and its handle.
pub struct CalendarSync {
    handle: SyncHandle,
    _worker: SyncWorker,
}

impl CalendarSync {
    /// The handle (`sync_now`, settings, statuses).
    pub fn handle(&self) -> &SyncHandle {
        &self.handle
    }
}

/// Builds the engine and starts the sync thread; the first full sync starts
/// immediately. Without `storage` (the database did not open) results are
/// kept in memory, so account status and the tray still work.
pub fn start(
    settings: &Settings,
    factory: Arc<dyn ProviderFactory>,
    storage: Option<Storage>,
    configured: AccountFilter,
    outlet: Arc<dyn SyncOutlet>,
    clock: Arc<dyn Clock>,
) -> std::io::Result<CalendarSync> {
    let config = SyncConfig::default();
    let sink: Arc<dyn SyncSink> = match &storage {
        Some(s) => Arc::new(StorageSink::new(s.clone(), config.window, configured)),
        None => Arc::new(InMemorySink::new()),
    };
    let listener = Arc::new(Listener::new(storage, outlet));
    let engine = SyncEngine::new(settings, factory, sink, listener, clock, config);
    let worker = SyncWorker::spawn(engine)?;
    Ok(CalendarSync {
        handle: worker.handle(),
        _worker: worker,
    })
}

/// The real [`SyncOutlet`].
struct TauriOutlet<R: Runtime>(AppHandle<R>);

impl<R: Runtime> SyncOutlet for TauriOutlet<R> {
    fn buddy_event(&self, event: BuddyEvent) {
        crate::buddy_state::dispatch(&self.0, event);
    }

    fn sync_in_progress(&self, running: bool) {
        crate::reminders::set_sync_in_progress(&self.0, running);
    }

    fn account_status(&self, status: AccountSyncStatus) {
        crate::accounts::set_sync_status(&self.0, status);
    }

    fn events_changed(&self, events: Vec<Event>) {
        crate::reminders::events_changed(&self.0, events);
    }

    fn upcoming_changed(&self) {
        let _ = self
            .0
            .emit_to(BUDDY_WINDOW_LABEL, event::UPCOMING_CHANGED, ());
    }
}

/// Starts calendar sync and keeps it in managed state. Call once during
/// setup, after `app_storage`, `settings_service`, `accounts` and
/// `reminders`. A failure is logged; `sync_now` then does nothing.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let Some(services) = crate::accounts::services(app) else {
        log::error!("calendar sync is off: the account services are not initialised");
        return;
    };
    let storage = match crate::app_storage::storage(app) {
        Ok(s) => Some(s),
        Err(e) => {
            log::error!("calendar sync keeps no cache: {e}");
            None
        }
    };
    let configured: AccountFilter = {
        let app = app.clone();
        Arc::new(move |id| {
            crate::settings_service::current(&app)
                .accounts
                .iter()
                .any(|a| a.id == *id)
        })
    };
    let settings = crate::settings_service::current(app);
    let sync = match start(
        &settings,
        services.providers,
        storage,
        configured,
        Arc::new(TauriOutlet(app.clone())),
        services.clock,
    ) {
        Ok(sync) => sync,
        Err(e) => {
            log::error!("cannot start calendar sync: {e}");
            return;
        }
    };
    let handle = sync.handle().clone();
    crate::settings_service::on_settings_changed(app, move |_old, new| {
        handle.update_settings(new.clone());
    });
    app.manage(sync);
}

/// Requests a full sync now (IPC `sync_now`). Returns immediately.
pub fn sync_now<R: Runtime>(app: &AppHandle<R>) {
    match app.try_state::<CalendarSync>() {
        Some(sync) => {
            log::info!("sync requested");
            sync.handle().sync_now();
        }
        None => log::warn!("sync requested, but calendar sync is not running"),
    }
}
