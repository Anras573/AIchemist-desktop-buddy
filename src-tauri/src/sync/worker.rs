//! The sync thread.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration as StdDuration, Instant};

use chrono::Duration;

use super::{AccountSyncStatus, SyncEngine, Triggers};
use crate::domain::ipc::SyncState;
use crate::domain::Settings;

/// Longest single wait. Bounds how late a sync can be after the machine
/// wakes up: on macOS (and Linux) timed waits do not count time asleep, so
/// an unbounded wait could oversleep by up to a whole interval. Waking once
/// a minute to compare clocks costs nothing measurable.
const MAX_WAIT: StdDuration = StdDuration::from_secs(60);

/// A difference between elapsed wall-clock and monotonic time larger than
/// this means the machine slept (or the clock was changed): treat it as a
/// resume.
const JUMP_TOLERANCE: Duration = Duration::seconds(60);

#[derive(Default)]
struct Control {
    sync_now: bool,
    resumed: bool,
    settings: Option<Settings>,
    shutdown: bool,
}

impl Control {
    fn pending(&self) -> bool {
        self.sync_now || self.resumed || self.settings.is_some()
    }
}

struct Shared {
    control: Mutex<Control>,
    wake: Condvar,
    statuses: Arc<Mutex<Vec<AccountSyncStatus>>>,
}

impl Shared {
    fn control(&self) -> MutexGuard<'_, Control> {
        self.control.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn update(&self, f: impl FnOnce(&mut Control)) {
        f(&mut self.control());
        self.wake.notify_one();
    }
}

/// Runs a [`SyncEngine`] on a dedicated thread (`"calendar-sync"`), so the
/// blocking providers never run on the main thread. Syncs once at start.
/// Stops and joins the thread when dropped (after a sync in progress ends).
pub struct SyncWorker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SyncWorker {
    /// Starts the thread. The first full sync begins immediately.
    pub fn spawn(engine: SyncEngine) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            control: Mutex::default(),
            wake: Condvar::new(),
            statuses: engine.statuses(),
        });
        let thread = std::thread::Builder::new()
            .name("calendar-sync".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || run(&shared, engine)
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// A cloneable handle for IPC commands, the settings store and power
    /// notifications.
    pub fn handle(&self) -> SyncHandle {
        SyncHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Stops the thread and waits for it (after the sync in progress, if
    /// any).
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shared.update(|c| c.shutdown = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SyncWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Controls a running [`SyncWorker`]. All methods return immediately.
#[derive(Clone)]
pub struct SyncHandle {
    shared: Arc<Shared>,
}

impl SyncHandle {
    /// Requests a full sync (IPC `sync_now`). Syncs never overlap: requests
    /// made while a sync is running coalesce into one follow-up sync.
    pub fn sync_now(&self) {
        self.shared.update(|c| c.sync_now = true);
    }

    /// Tells the engine the system resumed from sleep: runs a full sync.
    /// The worker also detects wake-ups itself from wall-clock jumps.
    pub fn notify_resumed(&self) {
        self.shared.update(|c| c.resumed = true);
    }

    /// Applies new settings live (sync interval, accounts, included
    /// calendars). Only the latest settings are kept if several arrive
    /// before the worker picks them up.
    pub fn update_settings(&self, settings: Settings) {
        self.shared.update(|c| c.settings = Some(settings));
    }

    /// The current status of every account, in settings order.
    pub fn account_statuses(&self) -> Vec<AccountSyncStatus> {
        self.shared
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether any account's last sync failed (tray error indicator).
    pub fn has_errors(&self) -> bool {
        self.account_statuses()
            .iter()
            .any(|s| s.sync_state == SyncState::Error)
    }
}

fn run(shared: &Shared, mut engine: SyncEngine) {
    let clock = engine.clock();
    let mut next_wake = engine.next_wake();
    loop {
        let triggers = {
            let mut control = shared.control();
            loop {
                if control.shutdown {
                    return;
                }
                let now = clock.now();
                if control.pending() || now >= next_wake {
                    break;
                }
                let wait = (next_wake - now)
                    .to_std()
                    .unwrap_or(StdDuration::ZERO)
                    .min(MAX_WAIT);
                let started = Instant::now();
                control = shared
                    .wake
                    .wait_timeout(control, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
                if clock_jumped(clock.now() - now, started.elapsed()) {
                    control.resumed = true;
                }
            }
            Triggers {
                sync_now: std::mem::take(&mut control.sync_now),
                resumed: std::mem::take(&mut control.resumed),
                settings: control.settings.take(),
            }
        };
        next_wake = engine.tick(triggers).next_wake;
    }
}

/// Whether the wall clock moved differently from the monotonic clock by
/// more than [`JUMP_TOLERANCE`] over one wait: the machine slept, or the
/// user changed the time.
pub(super) fn clock_jumped(wall: Duration, monotonic: StdDuration) -> bool {
    let Ok(monotonic) = Duration::from_std(monotonic) else {
        return true;
    };
    wall.checked_sub(&monotonic)
        .is_none_or(|diff| diff.abs() > JUMP_TOLERANCE)
}
