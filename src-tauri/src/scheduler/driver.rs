//! The thin runtime driver: one thread, one timer.
//!
//! [`SchedulerDriver`] owns a [`Scheduler`] behind a mutex and runs a single
//! background thread that sleeps on a condition variable until the core's
//! [`next_wake`](Scheduler::next_wake) instant, then calls
//! [`tick`](Scheduler::tick). Every change (events, tasks, settings, pause,
//! bubble actions) is applied on the caller's thread and then notifies the
//! condition variable, so the thread recomputes its deadline at once. There
//! is no polling and no fixed-interval tick.
//!
//! **Sleep cap.** The wait is capped at [`SchedulerDriver::DEFAULT_MAX_SLEEP`]
//! (60 s). The condition variable times out on the monotonic clock, which on
//! macOS (and in some Windows configurations) stops while the computer sleeps,
//! and which ignores wall-clock changes. Without the cap, a reminder due
//! during or after a system sleep or a clock change could fire arbitrarily
//! late. With it, every wake recomputes from `clock.now()`, so a reminder
//! fires at most 60 s late after resume. One wake-up a minute that finds
//! nothing to do costs a few microseconds of CPU, which is negligible against
//! the ~0% idle CPU target (spec 10). Embedders that get an OS resume
//! notification can also call [`SchedulerDriver::nudge`].

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};

use super::{BubbleChange, Scheduler, Update};
use crate::domain::ipc::BubbleView;
use crate::domain::{BubbleId, Clock, Event, ReminderInstance, Result, Settings, Task};

/// Receives the scheduler's output. Implemented by the app (task 2.2) to
/// emit Tauri events, drive the animation state and persist the ledger.
///
/// Called with the scheduler's lock held, from the driver thread or from
/// whichever thread called a [`SchedulerDriver`] method. Implementations
/// must return quickly and **must not call back into the
/// `SchedulerDriver`** (that would deadlock); hand work off instead.
pub trait SchedulerSink: Send {
    /// The bubble on screen changed.
    fn bubble_changed(&mut self, change: &BubbleChange);
    /// The ledger changed; `ledger` is the full ledger to persist.
    fn ledger_changed(&mut self, ledger: &[ReminderInstance]);
}

struct State {
    core: Scheduler,
    sink: Box<dyn SchedulerSink>,
    /// Set by every change so the thread recomputes its deadline.
    changed: bool,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    clock: Arc<dyn Clock>,
    max_sleep: StdDuration,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Runs a [`Scheduler`] on a background thread with a single timer.
/// `Send + Sync`, so it can live in Tauri managed state. Dropping it stops
/// the thread.
pub struct SchedulerDriver {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl SchedulerDriver {
    /// Longest the driver thread sleeps before re-checking the clock.
    pub const DEFAULT_MAX_SLEEP: StdDuration = StdDuration::from_secs(60);

    /// Starts the driver thread with the default sleep cap.
    pub fn spawn(
        core: Scheduler,
        clock: Arc<dyn Clock>,
        sink: Box<dyn SchedulerSink>,
    ) -> io::Result<Self> {
        Self::spawn_with_max_sleep(core, clock, sink, Self::DEFAULT_MAX_SLEEP)
    }

    /// Starts the driver thread with a custom sleep cap (tests use a short
    /// one to simulate a wall-clock jump).
    pub fn spawn_with_max_sleep(
        core: Scheduler,
        clock: Arc<dyn Clock>,
        sink: Box<dyn SchedulerSink>,
        max_sleep: StdDuration,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                core,
                sink,
                changed: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
            clock,
            max_sleep,
        });
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("scheduler".into())
            .spawn(move || run(&thread_shared))?;
        Ok(Self {
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// See [`Scheduler::set_events`].
    pub fn set_events(&self, events: Vec<Event>) {
        self.update(|core, clock| core.set_events(events, clock));
    }

    /// See [`Scheduler::set_tasks`].
    pub fn set_tasks(&self, tasks: Vec<Task>) {
        self.update(|core, clock| core.set_tasks(tasks, clock));
    }

    /// See [`Scheduler::set_settings`].
    pub fn set_settings(&self, settings: &Settings) {
        self.update(|core, clock| core.set_settings(settings, clock));
    }

    /// See [`Scheduler::set_paused`].
    pub fn set_paused(&self, paused: bool) {
        self.update(|core, clock| core.set_paused(paused, clock));
    }

    /// See [`Scheduler::dismiss`].
    pub fn dismiss(&self, id: BubbleId) -> Result<()> {
        self.apply(|core, clock| core.dismiss(id, clock))
    }

    /// See [`Scheduler::snooze`].
    pub fn snooze(&self, id: BubbleId, minutes: u32) -> Result<()> {
        self.apply(|core, clock| core.snooze(id, minutes, clock))
    }

    /// See [`Scheduler::snooze_all`].
    pub fn snooze_all(&self, minutes: u32) -> Result<()> {
        self.apply(|core, clock| core.snooze_all(minutes, clock))
    }

    /// Re-evaluates now, e.g. after an OS resume-from-sleep notification.
    pub fn nudge(&self) {
        self.update(|core, clock| core.tick(clock));
    }

    /// The bubble on screen, if any.
    pub fn current_bubble(&self) -> Option<BubbleView> {
        self.shared.lock().core.current_bubble()
    }

    /// Whether reminders are paused.
    pub fn is_paused(&self) -> bool {
        self.shared.lock().core.is_paused()
    }

    /// The full ledger.
    pub fn ledger(&self) -> Vec<ReminderInstance> {
        self.shared.lock().core.ledger()
    }

    /// When the driver thread will next wake for a reminder or quiet-hours
    /// boundary (before the sleep cap), if anything is scheduled.
    pub fn next_wake(&self) -> Option<DateTime<Utc>> {
        self.shared.lock().core.next_wake(&*self.shared.clock)
    }

    /// Stops and joins the driver thread. Idempotent; also done on drop.
    pub fn shutdown(&self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(thread) = thread {
            // A panic on the driver thread has already been reported.
            let _ = thread.join();
        }
    }

    /// [`apply`](Self::apply) for operations that cannot fail.
    fn update(&self, op: impl FnOnce(&mut Scheduler, &dyn Clock) -> Update) {
        // The closure never returns an error, so neither can `apply`.
        let _ = self.apply(|core, clock| Ok(op(core, clock)));
    }

    /// Applies an operation to the core under the lock, delivers its update
    /// to the sink and wakes the thread to recompute its deadline.
    fn apply(&self, op: impl FnOnce(&mut Scheduler, &dyn Clock) -> Result<Update>) -> Result<()> {
        let mut state = self.shared.lock();
        let update = op(&mut state.core, &*self.shared.clock)?;
        deliver(&mut state, &update);
        state.changed = true;
        drop(state);
        self.shared.wake.notify_all();
        Ok(())
    }
}

impl Drop for SchedulerDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn deliver(state: &mut State, update: &Update) {
    if let Some(change) = &update.bubble {
        state.sink.bubble_changed(change);
    }
    if update.ledger_changed {
        let ledger = state.core.ledger();
        state.sink.ledger_changed(&ledger);
    }
}

/// The driver thread: tick, compute the single next wake-up, sleep until
/// then or until a change arrives, repeat.
fn run(shared: &Shared) {
    let clock = &*shared.clock;
    let mut state = shared.lock();
    while !state.shutdown {
        let update = state.core.tick(clock);
        deliver(&mut state, &update);

        let now = clock.now();
        let timeout = match state.core.next_wake(clock) {
            Some(at) if at <= now => continue,
            Some(at) => (at - now)
                .to_std()
                .unwrap_or(StdDuration::ZERO)
                .min(shared.max_sleep),
            None => shared.max_sleep,
        };
        state.changed = false;
        state = shared
            .wake
            .wait_timeout_while(state, timeout, |s| !s.changed && !s.shutdown)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FakeClock, SystemClock, TaskId};
    use chrono::{Duration, TimeZone};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Instant;

    struct ChannelSink(Sender<BubbleChange>);

    impl SchedulerSink for ChannelSink {
        fn bubble_changed(&mut self, change: &BubbleChange) {
            let _ = self.0.send(change.clone());
        }
        fn ledger_changed(&mut self, _: &[ReminderInstance]) {}
    }

    fn task(id: &str, due: DateTime<Utc>) -> Task {
        Task {
            id: TaskId::new(id),
            title: format!("task {id}"),
            due_at: due,
            done: false,
            done_at: None,
            created_at: due - Duration::days(1),
        }
    }

    fn start(
        clock: Arc<dyn Clock>,
        max_sleep: StdDuration,
    ) -> (SchedulerDriver, Receiver<BubbleChange>) {
        let (tx, rx) = mpsc::channel();
        let core = Scheduler::new(Vec::new(), &Settings::default(), &*clock);
        let driver = SchedulerDriver::spawn_with_max_sleep(
            core,
            clock,
            Box::new(ChannelSink(tx)),
            max_sleep,
        )
        .unwrap();
        (driver, rx)
    }

    const PATIENCE: StdDuration = StdDuration::from_secs(10);

    #[test]
    fn wakes_at_the_due_time_on_the_real_clock() {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        // Cap well above the delay, so only the computed deadline can wake it.
        let (driver, rx) = start(Arc::clone(&clock), StdDuration::from_secs(3600));
        let started = Instant::now();
        driver.set_tasks(vec![task("1", clock.now() + Duration::milliseconds(300))]);
        assert!(driver.current_bubble().is_none(), "not due yet");

        let change = rx.recv_timeout(PATIENCE).expect("the reminder fires");
        let elapsed = started.elapsed();
        assert!(matches!(change, BubbleChange::Shown(_)));
        assert!(
            elapsed >= StdDuration::from_millis(250),
            "fired early: {elapsed:?}"
        );
        assert!(
            elapsed < StdDuration::from_secs(5),
            "fired late: {elapsed:?}"
        );
    }

    #[test]
    fn a_change_wakes_the_thread_immediately() {
        let t0 = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        let fake = Arc::new(FakeClock::new(t0));
        let (driver, rx) = start(fake.clone(), StdDuration::from_secs(3600));

        driver.set_tasks(vec![task("1", t0 + Duration::minutes(5))]);
        assert_eq!(driver.next_wake(), Some(t0 + Duration::minutes(5)));
        assert!(rx.try_recv().is_err());

        // A task that is already due fires synchronously with the change.
        driver.set_tasks(vec![
            task("1", t0 + Duration::minutes(5)),
            task("2", t0 - Duration::minutes(1)),
        ]);
        assert!(matches!(rx.try_recv(), Ok(BubbleChange::Shown(_))));

        // Simulated resume from sleep: the clock jumps, nudge re-evaluates.
        fake.advance(Duration::minutes(10));
        driver.nudge();
        let change = rx.try_recv().expect("queue count update");
        let BubbleChange::Updated(view) = change else {
            panic!("expected an update, got {change:?}");
        };
        assert_eq!(view.queued, 1);
    }

    #[test]
    fn the_sleep_cap_recovers_from_a_wall_clock_jump() {
        let t0 = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        let fake = Arc::new(FakeClock::new(t0));
        let (driver, rx) = start(fake.clone(), StdDuration::from_millis(20));
        driver.set_tasks(vec![task("1", t0 + Duration::hours(2))]);
        assert!(rx.try_recv().is_err());

        // Nobody tells the driver; only the capped sleep notices the jump.
        fake.advance(Duration::hours(3));
        let change = rx.recv_timeout(PATIENCE).expect("fires after the jump");
        assert!(matches!(change, BubbleChange::Shown(_)));
    }

    #[test]
    fn bubble_actions_go_through_the_driver() {
        let t0 = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        let fake = Arc::new(FakeClock::new(t0));
        let (driver, rx) = start(fake.clone(), StdDuration::from_secs(3600));
        driver.set_tasks(vec![task("1", t0)]);
        let Ok(BubbleChange::Shown(view)) = rx.try_recv() else {
            panic!("expected a bubble");
        };
        assert!(driver.dismiss(BubbleId(999)).is_err());
        driver.snooze(view.id, 5).unwrap();
        assert_eq!(rx.try_recv(), Ok(BubbleChange::Cleared));
        assert_eq!(driver.next_wake(), Some(t0 + Duration::minutes(5)));
        driver.set_paused(true);
        assert!(driver.is_paused());
        assert!(driver.snooze_all(0).is_err());
    }

    #[test]
    fn shutdown_is_prompt_and_idempotent() {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let (driver, _rx) = start(clock, StdDuration::from_secs(3600));
        let started = Instant::now();
        driver.shutdown();
        driver.shutdown();
        assert!(started.elapsed() < StdDuration::from_secs(5));
    }
}
