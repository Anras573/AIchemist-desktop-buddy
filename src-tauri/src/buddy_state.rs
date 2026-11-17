//! The buddy's animation state at runtime (spec 5, task 2.1).
//!
//! Holds the current [`BuddyState`], feeds every [`BuddyEvent`] through the
//! pure [`next_state`], and emits `buddy-state-changed` to the buddy window
//! when the state changes. Other modules call [`dispatch`]:
//!
//! - reminders (2.2): `ReminderDue`, `BubbleDismissed`, `BubbleSnoozed`;
//! - sync: `SyncStarted`, `SyncEnded`;
//! - the buddy page, through the `report_interaction` command:
//!   `UserInteracted`.
//!
//! # Idle-to-sleep timer
//!
//! While the cat is `Idle`, it falls asleep (`IdleTimeout`) once the
//! configured minutes have passed since the last *user activity*. User
//! activity is `UserInteracted` (hover or click on the cat) and leaving
//! `Alert` (the user dismissed or snoozed the bubble). Entering `Idle` from
//! `Thinking` does **not** reset the timer: syncs run every few minutes and
//! would otherwise keep the cat awake forever. So if the deadline passed
//! while a sync ran, the cat falls asleep as soon as the sync ends.
//!
//! One timer thread waits on a condition variable with a timeout up to the
//! deadline, or without a timeout when the cat is not `Idle`. It wakes only
//! when the deadline is reached or the state/deadline changes; no polling.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::domain::ipc::event;
use crate::domain::{AppearanceSettings, BuddyEvent, BuddyState, SystemSettings};
use crate::state_machine::next_state;
use crate::BUDDY_WINDOW_LABEL;

/// Pure state holder: no threads, no I/O, time passed in. Unit-testable.
#[derive(Debug, Clone)]
pub struct Core {
    state: BuddyState,
    idle_after: Duration,
    last_activity: Instant,
}

impl Core {
    /// Starts in `Idle` with the idle timer starting at `now`.
    pub fn new(now: Instant, idle_after: Duration) -> Self {
        Self {
            state: BuddyState::default(),
            idle_after,
            last_activity: now,
        }
    }

    /// The current state.
    pub fn state(&self) -> BuddyState {
        self.state
    }

    /// Applies `event` at `now`. Returns the new state if it changed.
    pub fn apply(&mut self, event: BuddyEvent, now: Instant) -> Option<BuddyState> {
        if event == BuddyEvent::UserInteracted {
            self.last_activity = now;
        }
        let next = next_state(self.state, event);
        if next == self.state {
            return None;
        }
        if self.state == BuddyState::Alert {
            // Leaving alert means the user dismissed or snoozed the bubble.
            self.last_activity = now;
        }
        self.state = next;
        Some(next)
    }

    /// Changes the idle timeout. Measured from the last activity, so a
    /// shorter value may make the cat fall asleep right away.
    pub fn set_idle_after(&mut self, idle_after: Duration) {
        self.idle_after = idle_after;
    }

    /// When the cat should fall asleep, if it is `Idle`; `None` otherwise.
    pub fn sleep_deadline(&self) -> Option<Instant> {
        (self.state == BuddyState::Idle).then(|| {
            self.last_activity
                .checked_add(self.idle_after)
                // Absurd durations: never in practice; treat as "far away".
                .unwrap_or(self.last_activity + Duration::from_secs(86_400 * 365))
        })
    }
}

type OnChange = Box<dyn Fn(BuddyState) + Send + Sync>;

struct Guarded {
    core: Core,
    stopped: bool,
}

struct Shared {
    guarded: Mutex<Guarded>,
    wake: Condvar,
    on_change: OnChange,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Guarded> {
        // A poisoned lock only means a panic elsewhere; the data is valid.
        self.guarded.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The state plus its idle timer thread. Dropping it stops the thread.
pub struct StateRuntime {
    shared: Arc<Shared>,
}

impl StateRuntime {
    /// Starts in `Idle` and spawns the timer thread. `on_change` is called
    /// with every new state, in order, while the state lock is held (so it
    /// must not call back into this runtime).
    pub fn start(
        idle_after: Duration,
        on_change: impl Fn(BuddyState) + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            guarded: Mutex::new(Guarded {
                core: Core::new(Instant::now(), idle_after),
                stopped: false,
            }),
            wake: Condvar::new(),
            on_change: Box::new(on_change),
        });
        let timer = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("buddy-idle-timer".into())
            .spawn(move || run_timer(&timer))
            .expect("failed to spawn the idle timer thread");
        Self { shared }
    }

    /// The current state.
    pub fn state(&self) -> BuddyState {
        self.shared.lock().core.state()
    }

    /// Applies `event`; calls `on_change` if the state changed.
    pub fn dispatch(&self, event: BuddyEvent) {
        let mut g = self.shared.lock();
        if let Some(state) = g.core.apply(event, Instant::now()) {
            (self.shared.on_change)(state);
        }
        // The deadline may have moved (interaction, state change).
        self.shared.wake.notify_one();
    }

    /// Changes the idle timeout.
    pub fn set_idle_after(&self, idle_after: Duration) {
        self.shared.lock().core.set_idle_after(idle_after);
        self.shared.wake.notify_one();
    }
}

impl Drop for StateRuntime {
    fn drop(&mut self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_one();
    }
}

fn run_timer(shared: &Shared) {
    let mut g = shared.lock();
    loop {
        if g.stopped {
            return;
        }
        let Some(deadline) = g.core.sleep_deadline() else {
            g = shared.wake.wait(g).unwrap_or_else(|e| e.into_inner());
            continue;
        };
        let now = Instant::now();
        if now >= deadline {
            if let Some(state) = g.core.apply(BuddyEvent::IdleTimeout, now) {
                (shared.on_change)(state);
            }
        } else {
            g = shared
                .wake
                .wait_timeout(g, deadline - now)
                .map(|(g, _)| g)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }
}

fn minutes(m: u32) -> Duration {
    Duration::from_secs(u64::from(m.max(1)) * 60)
}

/// Initial idle timeout: the settings default. Debug builds accept
/// `AICHEMIST_SLEEP_AFTER_IDLE_SECS` to test sleeping without waiting.
fn initial_idle_after() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("AICHEMIST_SLEEP_AFTER_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_secs(secs.max(1));
    }
    minutes(SystemSettings::default().sleep_after_idle_minutes)
}

/// Registers the state runtime and starts the idle timer. Call once during
/// setup, before the buddy page can call the commands.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let handle = app.clone();
    app.manage(StateRuntime::start(initial_idle_after(), move |state| {
        let _ = handle.emit_to(BUDDY_WINDOW_LABEL, event::BUDDY_STATE_CHANGED, state);
    }));
}

/// Feeds `event` to the state machine and emits `buddy-state-changed` if the
/// state changed. Safe to call from any thread.
pub fn dispatch<R: Runtime>(app: &AppHandle<R>, event: BuddyEvent) {
    if let Some(rt) = app.try_state::<StateRuntime>() {
        rt.dispatch(event);
    }
}

/// The current state (`Idle` before [`init`]).
pub fn current<R: Runtime>(app: &AppHandle<R>) -> BuddyState {
    app.try_state::<StateRuntime>()
        .map(|rt| rt.state())
        .unwrap_or_default()
}

/// Applies `settings.system.sleepAfterIdleMinutes` (validated, 1-240; values
/// below 1 are treated as 1). For settings loading and `update_settings`.
pub fn set_sleep_after_idle_minutes<R: Runtime>(app: &AppHandle<R>, minutes_: u32) {
    if let Some(rt) = app.try_state::<StateRuntime>() {
        rt.set_idle_after(minutes(minutes_));
    }
}

/// IPC `get_buddy_state` (buddy window).
#[tauri::command]
pub fn get_buddy_state<R: Runtime>(app: AppHandle<R>) -> BuddyState {
    current(&app)
}

/// IPC `report_interaction` (buddy window): the user hovered over or
/// clicked the cat. Wakes it and resets the idle timer.
#[tauri::command]
pub fn report_interaction<R: Runtime>(app: AppHandle<R>) {
    dispatch(&app, BuddyEvent::UserInteracted);
}

/// IPC `get_appearance` (buddy window).
#[tauri::command]
pub fn get_appearance<R: Runtime>(app: AppHandle<R>) -> AppearanceSettings {
    crate::buddy_window::appearance(&app)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use BuddyEvent as E;
    use BuddyState as S;

    const TEN_MIN: Duration = Duration::from_secs(600);

    #[test]
    fn dispatch_sequence_reports_only_changes() {
        let t0 = Instant::now();
        let mut core = Core::new(t0, TEN_MIN);
        let seq = [
            (E::SyncStarted, Some(S::Thinking)),
            (E::SyncStarted, None),
            (E::ReminderDue, Some(S::Alert)),
            (E::SyncEnded, None),
            (E::UserInteracted, None),
            (E::BubbleSnoozed, Some(S::Idle)),
            (E::IdleTimeout, Some(S::Sleeping)),
            (E::SyncStarted, None),
            (E::UserInteracted, Some(S::Idle)),
            (E::BubbleDismissed, None),
        ];
        for (event, expected) in seq {
            assert_eq!(core.apply(event, t0), expected, "{event:?}");
        }
        assert_eq!(core.state(), S::Idle);
    }

    #[test]
    fn deadline_only_while_idle() {
        let t0 = Instant::now();
        let mut core = Core::new(t0, TEN_MIN);
        assert_eq!(core.sleep_deadline(), Some(t0 + TEN_MIN));
        core.apply(E::SyncStarted, t0);
        assert_eq!(core.sleep_deadline(), None);
        core.apply(E::SyncEnded, t0);
        core.apply(E::IdleTimeout, t0);
        assert_eq!(core.sleep_deadline(), None);
    }

    #[test]
    fn interaction_resets_the_deadline() {
        let t0 = Instant::now();
        let mut core = Core::new(t0, TEN_MIN);
        let t1 = t0 + Duration::from_secs(300);
        core.apply(E::UserInteracted, t1);
        assert_eq!(core.sleep_deadline(), Some(t1 + TEN_MIN));
    }

    #[test]
    fn sync_does_not_reset_the_deadline() {
        let t0 = Instant::now();
        let mut core = Core::new(t0, TEN_MIN);
        core.apply(E::SyncStarted, t0 + Duration::from_secs(60));
        core.apply(E::SyncEnded, t0 + Duration::from_secs(120));
        assert_eq!(core.sleep_deadline(), Some(t0 + TEN_MIN));
    }

    #[test]
    fn leaving_alert_resets_the_deadline() {
        let t0 = Instant::now();
        let mut core = Core::new(t0, TEN_MIN);
        core.apply(E::ReminderDue, t0);
        let t1 = t0 + Duration::from_secs(900);
        core.apply(E::BubbleDismissed, t1);
        assert_eq!(core.sleep_deadline(), Some(t1 + TEN_MIN));
    }

    #[test]
    fn set_idle_after_moves_the_deadline() {
        let t0 = Instant::now();
        let mut core = Core::new(t0, TEN_MIN);
        core.set_idle_after(Duration::from_secs(60));
        assert_eq!(core.sleep_deadline(), Some(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn buddy_capability_grants_the_commands_to_the_buddy_window_only() {
        use crate::domain::ipc::{command, Window, COMMANDS};

        let cap: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/buddy.json")).unwrap();
        let granted: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p.as_str())
            .collect();
        for name in [
            command::GET_BUDDY_STATE,
            command::REPORT_INTERACTION,
            command::GET_APPEARANCE,
        ] {
            let spec = COMMANDS.iter().find(|c| c.name == name).unwrap();
            assert_eq!(spec.windows, &[Window::Buddy]);
            let perm = format!("allow-{}", name.replace('_', "-"));
            assert!(granted.contains(&perm.as_str()), "{perm} not granted");
        }
        assert_eq!(cap["windows"], serde_json::json!(["buddy"]));
    }

    fn runtime(idle_after: Duration) -> (StateRuntime, mpsc::Receiver<BuddyState>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let rt = StateRuntime::start(idle_after, move |s| {
            let _ = tx.lock().unwrap().send(s);
        });
        (rt, rx)
    }

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn timer_puts_the_cat_to_sleep_and_interaction_wakes_it() {
        let (rt, rx) = runtime(Duration::from_millis(50));
        assert_eq!(rx.recv_timeout(WAIT), Ok(S::Sleeping));
        assert_eq!(rt.state(), S::Sleeping);
        rt.dispatch(E::UserInteracted);
        assert_eq!(rx.recv_timeout(WAIT), Ok(S::Idle));
        // And falls asleep again.
        assert_eq!(rx.recv_timeout(WAIT), Ok(S::Sleeping));
    }

    #[test]
    fn interaction_postpones_sleep() {
        let (rt, rx) = runtime(Duration::from_millis(300));
        let start = Instant::now();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(100));
            rt.dispatch(E::UserInteracted);
        }
        assert_eq!(rx.recv_timeout(WAIT), Ok(S::Sleeping));
        // At least 400 ms of interactions plus the 300 ms timeout.
        assert!(start.elapsed() >= Duration::from_millis(700));
    }

    #[test]
    fn no_timeout_while_not_idle() {
        let (rt, rx) = runtime(Duration::from_millis(50));
        rt.dispatch(E::ReminderDue);
        assert_eq!(rx.recv_timeout(WAIT), Ok(S::Alert));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert_eq!(rt.state(), S::Alert);
    }

    #[test]
    fn shortening_the_timeout_takes_effect() {
        let (rt, rx) = runtime(TEN_MIN);
        rt.set_idle_after(Duration::from_millis(50));
        assert_eq!(rx.recv_timeout(WAIT), Ok(S::Sleeping));
    }
}
