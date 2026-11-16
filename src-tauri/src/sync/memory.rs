//! In-memory [`SyncSink`] for tests (and until storage is wired up).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use super::{SyncOutcome, SyncSink};
use crate::domain::{AccountId, Error, Event, Result};

#[derive(Debug, Default)]
struct State {
    events: BTreeMap<AccountId, Vec<Event>>,
    outcomes: BTreeMap<AccountId, SyncOutcome>,
    replace_count: usize,
    write_failure: Option<Error>,
}

/// A [`SyncSink`] that keeps everything in memory.
#[derive(Debug, Default)]
pub struct InMemorySink {
    state: Mutex<State>,
}

impl InMemorySink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the cache of `account`, as if loaded from disk at startup.
    pub fn seed(&self, account: &AccountId, events: Vec<Event>) {
        self.state().events.insert(account.clone(), events);
    }

    /// The cached events of `account`.
    pub fn events_of(&self, account: &AccountId) -> Vec<Event> {
        self.state()
            .events
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    /// The last recorded outcome for `account`.
    pub fn last_outcome(&self, account: &AccountId) -> Option<SyncOutcome> {
        self.state().outcomes.get(account).cloned()
    }

    /// Number of successful `replace_events` calls.
    pub fn replace_count(&self) -> usize {
        self.state().replace_count
    }

    /// Makes `replace_events` fail with `error` (`None` to succeed again).
    pub fn fail_writes(&self, error: Option<Error>) {
        self.state().write_failure = error;
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SyncSink for InMemorySink {
    fn replace_events(&self, account: &AccountId, events: &[Event]) -> Result<()> {
        let mut state = self.state();
        if let Some(e) = &state.write_failure {
            return Err(e.clone());
        }
        state.events.insert(account.clone(), events.to_vec());
        state.replace_count += 1;
        Ok(())
    }

    fn record_sync(&self, account: &AccountId, outcome: &SyncOutcome) -> Result<()> {
        self.state()
            .outcomes
            .insert(account.clone(), outcome.clone());
        Ok(())
    }

    fn cached_events(&self) -> Result<Vec<Event>> {
        Ok(self.state().events.values().flatten().cloned().collect())
    }
}
