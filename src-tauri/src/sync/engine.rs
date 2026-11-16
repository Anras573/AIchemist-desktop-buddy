//! Single-threaded sync logic, driven by [`SyncEngine::tick`].

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

use super::{
    merge_events, AccountSyncStatus, ProviderFactory, SyncListener, SyncOutcome, SyncSink,
    SyncUpdate,
};
use crate::domain::ipc::SyncState;
use crate::domain::{
    AccountConfig, CalendarProvider, Clock, Error, Event, ReminderSettings, Result, Settings,
};

/// Internal sync parameters (not user settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncConfig {
    /// Length of the fetched window `[now, now + window)`. Default 7 days
    /// (spec 6.5).
    pub window: Duration,
    /// First retry delay after a retryable error; doubles with each further
    /// failure, capped at the sync interval. Default 30 s.
    pub initial_backoff: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            window: Duration::days(7),
            initial_backoff: Duration::seconds(30),
        }
    }
}

/// What woke the engine, collected by the worker since the last tick.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Triggers {
    /// The user asked for a sync (`sync_now`).
    pub sync_now: bool,
    /// The system resumed from sleep (or the wall clock jumped).
    pub resumed: bool,
    /// New settings to apply before deciding what to run.
    pub settings: Option<Settings>,
}

/// Result of [`SyncEngine::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickOutcome {
    /// Whether this tick ran a full sync (all accounts).
    pub full_sync: bool,
    /// When to call `tick` again at the latest: the next regular sync or the
    /// earliest scheduled retry.
    pub next_wake: DateTime<Utc>,
}

struct AccountEntry {
    config: AccountConfig,
    provider: Option<Arc<dyn CalendarProvider>>,
    status: AccountSyncStatus,
    /// Consecutive retryable failures (drives the backoff).
    failures: u32,
}

impl AccountEntry {
    fn new(config: AccountConfig) -> Self {
        Self {
            status: AccountSyncStatus::new(config.id.clone()),
            config,
            provider: None,
            failures: 0,
        }
    }
}

/// The sync logic, without threads: [`tick`](Self::tick) decides what is
/// due at `clock.now()`, runs it synchronously (blocking provider calls) and
/// returns the next deadline. [`SyncWorker`](super::SyncWorker) calls it on
/// its thread; tests call it directly with a fake clock.
pub struct SyncEngine {
    factory: Arc<dyn ProviderFactory>,
    sink: Arc<dyn SyncSink>,
    listener: Arc<dyn SyncListener>,
    clock: Arc<dyn Clock>,
    config: SyncConfig,
    interval: Duration,
    accounts: Vec<AccountEntry>,
    last_full: Option<DateTime<Utc>>,
    next_full: Option<DateTime<Utc>>,
    last_emitted: Option<Vec<Event>>,
    statuses: Arc<Mutex<Vec<AccountSyncStatus>>>,
}

impl SyncEngine {
    /// Creates an engine for `settings`. Nothing runs until the first
    /// [`tick`](Self::tick), which always runs a full sync.
    pub fn new(
        settings: &Settings,
        factory: Arc<dyn ProviderFactory>,
        sink: Arc<dyn SyncSink>,
        listener: Arc<dyn SyncListener>,
        clock: Arc<dyn Clock>,
        config: SyncConfig,
    ) -> Self {
        let mut engine = Self {
            factory,
            sink,
            listener,
            clock,
            config,
            interval: Duration::minutes(10),
            accounts: Vec::new(),
            last_full: None,
            next_full: None,
            last_emitted: None,
            statuses: Arc::default(),
        };
        engine.apply_settings(settings);
        engine
    }

    /// The engine's clock.
    pub fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// A shared snapshot of every account's status, kept up to date by the
    /// engine, in settings order.
    pub fn statuses(&self) -> Arc<Mutex<Vec<AccountSyncStatus>>> {
        Arc::clone(&self.statuses)
    }

    /// The current sync interval (from settings, clamped to 5..=15 min).
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Applies `triggers`, runs a full sync if one is due (first tick,
    /// interval elapsed, `sync_now`, resume, relevant account change),
    /// otherwise retries the accounts whose backoff expired. Blocks while
    /// providers fetch.
    pub fn tick(&mut self, triggers: Triggers) -> TickOutcome {
        let mut full = triggers.sync_now || triggers.resumed;
        if let Some(settings) = &triggers.settings {
            full |= self.apply_settings(settings);
        }
        let now = self.clock.now();
        if self.next_full.is_none_or(|t| now >= t) {
            full = true;
        }
        if full {
            self.last_full = Some(now);
            self.next_full = Some(now + self.interval);
        }
        self.run(full, now);
        TickOutcome {
            full_sync: full,
            next_wake: self.next_wake(),
        }
    }

    /// The next regular sync or the earliest retry, whichever is first.
    /// Before the first tick: now.
    pub fn next_wake(&self) -> DateTime<Utc> {
        let Some(next_full) = self.next_full else {
            return self.clock.now();
        };
        self.accounts
            .iter()
            .filter_map(|a| a.status.next_retry_at)
            .fold(next_full, DateTime::min)
    }

    /// Applies new settings. Returns whether the accounts changed in a way
    /// that needs a full sync.
    fn apply_settings(&mut self, settings: &Settings) -> bool {
        let range = ReminderSettings::SYNC_INTERVAL_RANGE;
        let minutes = settings
            .reminders
            .sync_interval_minutes
            .clamp(*range.start(), *range.end());
        let interval = Duration::minutes(i64::from(minutes));
        if interval != self.interval {
            self.interval = interval;
            // Re-plan the next regular sync from the last one.
            self.next_full = self.last_full.map(|t| t + interval);
        }

        let mut old = std::mem::take(&mut self.accounts);
        let mut changed = old.len() != settings.accounts.len();
        for config in &settings.accounts {
            let entry = match old.iter().position(|e| e.config.id == config.id) {
                Some(i) => {
                    let mut entry = old.swap_remove(i);
                    if entry.config.connection != config.connection {
                        entry.provider = None;
                    }
                    if entry.config.connection != config.connection
                        || entry.config.included_calendars != config.included_calendars
                    {
                        changed = true;
                        entry.failures = 0;
                        entry.status.next_retry_at = None;
                    }
                    entry.config = config.clone();
                    entry
                }
                None => {
                    changed = true;
                    AccountEntry::new(config.clone())
                }
            };
            self.accounts.push(entry);
        }
        self.publish_statuses();
        changed
    }

    fn run(&mut self, full: bool, now: DateTime<Utc>) {
        let selected: Vec<usize> = (0..self.accounts.len())
            .filter(|&i| {
                full || self.accounts[i]
                    .status
                    .next_retry_at
                    .is_some_and(|t| t <= now)
            })
            .collect();
        if !selected.is_empty() {
            self.listener.on_update(SyncUpdate::Started);
        }
        for &i in &selected {
            self.sync_account(i, now);
        }
        if full || !selected.is_empty() {
            self.emit_events_if_changed();
        }
        if !selected.is_empty() {
            self.listener.on_update(SyncUpdate::Ended);
        }
    }

    fn sync_account(&mut self, i: usize, now: DateTime<Utc>) {
        let id = self.accounts[i].config.id.clone();
        if self.accounts[i].config.included_calendars.is_empty() {
            // Nothing to fetch: drop any stale cache and report "never synced".
            let _ = self.sink.replace_events(&id, &[]);
            let entry = &mut self.accounts[i];
            entry.failures = 0;
            entry.status = AccountSyncStatus::new(id);
            self.emit_status(i);
            return;
        }

        self.accounts[i].status.sync_state = SyncState::Syncing;
        self.emit_status(i);

        let result = self.fetch_and_store(i, now);
        let finished = self.clock.now();
        let entry = &mut self.accounts[i];
        match &result {
            Ok(()) => {
                entry.failures = 0;
                entry.status.sync_state = SyncState::Ok;
                entry.status.last_synced_at = Some(finished);
                entry.status.last_error = None;
                entry.status.next_retry_at = None;
            }
            Err(error) => {
                entry.status.sync_state = SyncState::Error;
                entry.status.last_error = Some(error.clone());
                if error.is_retryable() {
                    entry.failures = entry.failures.saturating_add(1);
                    entry.status.next_retry_at = Some(
                        finished
                            + backoff_delay(
                                entry.failures,
                                self.config.initial_backoff,
                                self.interval,
                            ),
                    );
                } else {
                    // Wait for the next full sync, with a fresh provider
                    // (credentials may have been fixed by then).
                    entry.failures = 0;
                    entry.status.next_retry_at = None;
                    entry.provider = None;
                }
            }
        }
        let outcome = SyncOutcome {
            attempted_at: now,
            result,
        };
        // A failure to record the outcome must not fail the sync.
        let _ = self.sink.record_sync(&id, &outcome);
        self.emit_status(i);
    }

    /// Fetches the account's included calendars and replaces its cache. On
    /// any error the cache is left as it was.
    fn fetch_and_store(&mut self, i: usize, now: DateTime<Utc>) -> Result<()> {
        let entry = &mut self.accounts[i];
        let provider = match &entry.provider {
            Some(p) => Arc::clone(p),
            None => {
                let p = self.factory.create(&entry.config)?;
                entry.provider = Some(Arc::clone(&p));
                p
            }
        };
        if provider.account_id() != &entry.config.id {
            return Err(Error::internal("provider serves a different account"));
        }
        let calendars = &entry.config.included_calendars;
        let mut events = provider.fetch_events(calendars, now, now + self.config.window)?;
        // Defensive: keep only what belongs to this account and its
        // included calendars.
        events.retain(|e| e.account_id == entry.config.id && calendars.contains(&e.calendar_id));
        self.sink.replace_events(&entry.config.id, &events)
    }

    fn emit_events_if_changed(&mut self) {
        // If the cache cannot be read, keep the last list we reported.
        let Ok(cached) = self.sink.cached_events() else {
            return;
        };
        let configs: Vec<AccountConfig> = self.accounts.iter().map(|a| a.config.clone()).collect();
        let merged = merge_events(cached, &configs, self.clock.as_ref());
        if self.last_emitted.as_ref() != Some(&merged) {
            self.last_emitted = Some(merged.clone());
            self.listener.on_update(SyncUpdate::EventsChanged(merged));
        }
    }

    fn emit_status(&self, i: usize) {
        self.publish_statuses();
        self.listener
            .on_update(SyncUpdate::AccountStatus(self.accounts[i].status.clone()));
    }

    fn publish_statuses(&self) {
        let snapshot = self.accounts.iter().map(|a| a.status.clone()).collect();
        *self.statuses.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }
}

/// Delay before retry number `failures` (1-based): `initial * 2^(failures-1)`,
/// capped at `cap`.
pub(super) fn backoff_delay(failures: u32, initial: Duration, cap: Duration) -> Duration {
    let factor = 1i32 << failures.saturating_sub(1).min(20);
    (initial * factor).min(cap)
}
