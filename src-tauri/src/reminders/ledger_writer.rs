//! Persists the reminder ledger off the scheduler lock.
//!
//! The scheduler sink runs with the scheduler's lock held, so it must not
//! wait on SQLite. [`LedgerWriter::submit`] only stores the latest snapshot
//! and wakes one worker thread, which saves it. Snapshots that arrive while
//! a save is running replace each other: a burst of changes costs at most
//! two writes (the one in flight and the latest), and the last snapshot is
//! always the one that ends up on disk.

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use crate::domain::ReminderInstance;

type SaveFn = Box<dyn FnMut(&[ReminderInstance]) + Send>;

#[derive(Default)]
struct State {
    /// The latest snapshot not yet handed to the save function.
    pending: Option<Vec<ReminderInstance>>,
    /// Whether the worker is saving right now.
    busy: bool,
    shutdown: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    /// Signals the worker (new snapshot, shutdown) and flushers (idle).
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One worker thread that saves the most recent ledger snapshot.
/// Dropping it flushes and stops the thread.
pub struct LedgerWriter {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl LedgerWriter {
    /// Starts the worker. `save` is called on the worker thread with each
    /// snapshot to persist; it handles (logs) its own errors.
    pub fn spawn(save: impl FnMut(&[ReminderInstance]) + Send + 'static) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let save: SaveFn = Box::new(save);
        let thread = thread::Builder::new()
            .name("ledger-writer".into())
            .spawn(move || run(&worker, save))?;
        Ok(Self {
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Queues `ledger` for saving, replacing any snapshot not yet saved.
    /// Never blocks on I/O.
    pub fn submit(&self, ledger: Vec<ReminderInstance>) {
        self.shared.lock().pending = Some(ledger);
        self.shared.changed.notify_all();
    }

    /// Blocks until every submitted snapshot has been saved.
    pub fn flush(&self) {
        let state = self.shared.lock();
        let _idle = self
            .shared
            .changed
            .wait_while(state, |s| (s.pending.is_some() || s.busy) && !s.shutdown)
            .unwrap_or_else(|e| e.into_inner());
    }

    /// Saves what is pending, then stops and joins the worker. Idempotent.
    pub fn shutdown(&self) {
        self.shared.lock().shutdown = true;
        self.shared.changed.notify_all();
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for LedgerWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(shared: &Shared, mut save: SaveFn) {
    let mut state = shared.lock();
    loop {
        if let Some(ledger) = state.pending.take() {
            state.busy = true;
            drop(state);
            save(&ledger);
            state = shared.lock();
            state.busy = false;
            shared.changed.notify_all();
            continue;
        }
        if state.shutdown {
            return;
        }
        state = shared
            .changed
            .wait_while(state, |s| s.pending.is_none() && !s.shutdown)
            .unwrap_or_else(|e| e.into_inner());
    }
}
