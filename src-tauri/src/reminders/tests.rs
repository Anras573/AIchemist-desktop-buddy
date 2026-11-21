use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::ledger_writer::LedgerWriter;
use super::service::test_support;
use super::{Outlet, Reminders};
use crate::domain::ipc::BubbleView;
use crate::domain::{
    BubbleId, BuddyEvent, Clock, FakeClock, ReminderInstance, ReminderKey, ReminderOutcome,
    ReminderStatus, Settings, TaskId,
};
use crate::scheduler::BubbleChange;
use crate::storage::Storage;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Buddy(BuddyEvent),
    Shown(BubbleId),
    Cleared,
    Chime,
}

#[derive(Default)]
struct FakeOutlet(Mutex<Vec<Call>>);

impl FakeOutlet {
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
    fn push(&self, call: Call) {
        self.0.lock().unwrap().push(call);
    }
}

impl Outlet for FakeOutlet {
    fn buddy_event(&self, event: BuddyEvent) {
        self.push(Call::Buddy(event));
    }
    fn bubble_shown(&self, view: &BubbleView) {
        self.push(Call::Shown(view.id));
    }
    fn bubble_cleared(&self) {
        self.push(Call::Cleared);
    }
    fn play_chime(&self) {
        self.push(Call::Chime);
    }
}

fn view(id: u64) -> BubbleView {
    BubbleView {
        id: BubbleId(id),
        items: Vec::new(),
        queued: 0,
        replay: false,
        snooze_options_minutes: vec![5],
    }
}

fn noop_writer() -> Arc<LedgerWriter> {
    Arc::new(LedgerWriter::spawn(|_| {}).unwrap())
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
}

// ----- Sink mapping -----

#[test]
fn shown_alerts_emits_and_chimes_when_sound_is_on() {
    let outlet = Arc::new(FakeOutlet::default());
    let mut sink = test_support::sink(outlet.clone(), noop_writer(), true, false, None);
    sink.bubble_changed(&BubbleChange::Shown(view(1)));
    assert_eq!(
        outlet.take(),
        [
            Call::Buddy(BuddyEvent::ReminderDue),
            Call::Shown(BubbleId(1)),
            Call::Chime
        ]
    );
}

#[test]
fn shown_is_silent_when_sound_is_off() {
    let outlet = Arc::new(FakeOutlet::default());
    let mut sink = test_support::sink(outlet.clone(), noop_writer(), false, false, None);
    sink.bubble_changed(&BubbleChange::Shown(view(1)));
    assert_eq!(
        outlet.take(),
        [
            Call::Buddy(BuddyEvent::ReminderDue),
            Call::Shown(BubbleId(1))
        ]
    );
}

#[test]
fn updated_re_emits_without_chime_or_state_change() {
    let outlet = Arc::new(FakeOutlet::default());
    let mut sink = test_support::sink(outlet.clone(), noop_writer(), true, false, None);
    sink.bubble_changed(&BubbleChange::Updated(view(2)));
    assert_eq!(outlet.take(), [Call::Shown(BubbleId(2))]);
}

#[test]
fn cleared_emits_and_leaves_alert() {
    let outlet = Arc::new(FakeOutlet::default());
    let mut sink = test_support::sink(outlet.clone(), noop_writer(), true, false, None);
    sink.bubble_changed(&BubbleChange::Cleared);
    assert_eq!(
        outlet.take(),
        [Call::Cleared, Call::Buddy(BuddyEvent::BubbleDismissed)]
    );

    let mut sink = test_support::sink(
        outlet.clone(),
        noop_writer(),
        true,
        false,
        Some(BuddyEvent::BubbleSnoozed),
    );
    sink.bubble_changed(&BubbleChange::Cleared);
    assert_eq!(
        outlet.take(),
        [Call::Cleared, Call::Buddy(BuddyEvent::BubbleSnoozed)]
    );
}

#[test]
fn cleared_during_a_sync_returns_to_thinking() {
    let outlet = Arc::new(FakeOutlet::default());
    let mut sink = test_support::sink(outlet.clone(), noop_writer(), true, true, None);
    sink.bubble_changed(&BubbleChange::Cleared);
    assert_eq!(
        outlet.take(),
        [
            Call::Cleared,
            Call::Buddy(BuddyEvent::BubbleDismissed),
            Call::Buddy(BuddyEvent::SyncStarted)
        ]
    );
}

#[test]
fn ledger_changes_reach_the_writer() {
    let (tx, rx) = mpsc::channel();
    let writer = Arc::new(LedgerWriter::spawn(move |l| tx.send(l.len()).unwrap()).unwrap());
    let outlet = Arc::new(FakeOutlet::default());
    let mut sink = test_support::sink(outlet, writer.clone(), true, false, None);
    let inst = ReminderInstance {
        key: ReminderKey::Task {
            task_id: TaskId::new("t"),
        },
        due_at: t0(),
        status: ReminderStatus::Pending,
    };
    sink.ledger_changed(&[inst.clone(), inst]);
    writer.flush();
    assert_eq!(rx.try_recv(), Ok(2));
}

// ----- Ledger writer -----

#[test]
fn writer_coalesces_bursts_and_saves_the_latest() {
    let (started_tx, started_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let saved = Arc::new(Mutex::new(Vec::new()));
    let writer = LedgerWriter::spawn({
        let saved = Arc::clone(&saved);
        move |l: &[ReminderInstance]| {
            started_tx.send(()).unwrap();
            gate_rx.recv().unwrap();
            saved.lock().unwrap().push(l.len());
        }
    })
    .unwrap();
    let snapshot = |n: usize| {
        (0..n)
            .map(|i| ReminderInstance {
                key: ReminderKey::Task {
                    task_id: TaskId::new(format!("t{i}")),
                },
                due_at: t0(),
                status: ReminderStatus::Pending,
            })
            .collect::<Vec<_>>()
    };

    writer.submit(snapshot(1));
    started_rx
        .recv_timeout(StdDuration::from_secs(5))
        .expect("first save starts");
    // A burst while the first save is blocked: only the last one is saved.
    writer.submit(snapshot(2));
    writer.submit(snapshot(3));
    writer.submit(snapshot(4));
    gate_tx.send(()).unwrap();
    started_rx
        .recv_timeout(StdDuration::from_secs(5))
        .expect("second save starts");
    gate_tx.send(()).unwrap();
    writer.flush();
    assert_eq!(*saved.lock().unwrap(), [1, 4]);
    assert!(started_rx.try_recv().is_err(), "no third save");
}

#[test]
fn writer_shutdown_saves_what_is_pending() {
    let saved = Arc::new(Mutex::new(Vec::new()));
    let writer = LedgerWriter::spawn({
        let saved = Arc::clone(&saved);
        move |l: &[ReminderInstance]| saved.lock().unwrap().push(l.len())
    })
    .unwrap();
    writer.submit(Vec::new());
    writer.shutdown();
    writer.shutdown();
    assert_eq!(*saved.lock().unwrap(), [0]);
}

// ----- Service end to end (fake clock, in-memory storage) -----

struct Fixture {
    clock: Arc<FakeClock>,
    storage: Storage,
    outlet: Arc<FakeOutlet>,
}

impl Fixture {
    fn new() -> Self {
        let clock = Arc::new(FakeClock::new(t0()));
        let storage = Storage::open_in_memory(clock.clone()).unwrap();
        Self {
            clock,
            storage,
            outlet: Arc::new(FakeOutlet::default()),
        }
    }

    fn start(&self, paused: bool) -> Reminders {
        let clock: Arc<dyn Clock> = self.clock.clone();
        Reminders::start(
            self.storage.clone(),
            &Settings::default(),
            clock,
            paused,
            self.outlet.clone(),
        )
        .unwrap()
    }

    fn task_status(&self, id: &TaskId) -> ReminderStatus {
        self.storage
            .load_reminders()
            .unwrap()
            .into_iter()
            .find(|r| {
                r.key
                    == ReminderKey::Task {
                        task_id: id.clone(),
                    }
            })
            .expect("task reminder persisted")
            .status
    }
}

fn shown_id(calls: &[Call]) -> BubbleId {
    calls
        .iter()
        .find_map(|c| match c {
            Call::Shown(id) => Some(*id),
            _ => None,
        })
        .expect("a bubble was shown")
}

#[test]
fn a_due_task_fires_at_startup_and_dismiss_persists() {
    let fx = Fixture::new();
    let task = fx
        .storage
        .create_task("Water plants", t0() - Duration::minutes(1))
        .unwrap();
    let r = fx.start(false);
    let calls = fx.outlet.take();
    assert_eq!(calls[0], Call::Buddy(BuddyEvent::ReminderDue));
    assert!(calls.contains(&Call::Chime));
    let id = shown_id(&calls);
    assert_eq!(r.current_bubble().unwrap().id, id);

    assert!(r.dismiss(BubbleId(id.0 + 100)).is_err(), "stale id");
    r.dismiss(id).unwrap();
    assert_eq!(
        fx.outlet.take(),
        [Call::Cleared, Call::Buddy(BuddyEvent::BubbleDismissed)]
    );
    r.flush();
    assert!(matches!(
        fx.task_status(&task.id),
        ReminderStatus::Handled {
            outcome: ReminderOutcome::Dismissed,
            ..
        }
    ));

    // Restart: the dismissed reminder does not fire again.
    drop(r);
    let r = fx.start(false);
    assert!(r.current_bubble().is_none());
    assert!(!fx.outlet.take().contains(&Call::Chime));
}

#[test]
fn snooze_leaves_alert_as_snoozed_and_refires() {
    let fx = Fixture::new();
    fx.storage.create_task("Stretch", t0()).unwrap();
    let r = fx.start(false);
    let id = shown_id(&fx.outlet.take());
    r.snooze(id, 5).unwrap();
    assert_eq!(
        fx.outlet.take(),
        [Call::Cleared, Call::Buddy(BuddyEvent::BubbleSnoozed)]
    );
    fx.clock.advance(Duration::minutes(5));
    r.nudge();
    let calls = fx.outlet.take();
    assert_eq!(calls[0], Call::Buddy(BuddyEvent::ReminderDue));
    assert!(calls.contains(&Call::Chime));
}

#[test]
fn set_paused_reaches_the_driver_and_replays_on_resume() {
    let fx = Fixture::new();
    fx.storage.create_task("Call back", t0()).unwrap();
    let r = fx.start(false);
    shown_id(&fx.outlet.take());

    r.set_paused(true);
    assert!(r.is_paused());
    assert_eq!(
        fx.outlet.take(),
        [Call::Cleared, Call::Buddy(BuddyEvent::BubbleDismissed)]
    );

    r.set_paused(false);
    assert!(!r.is_paused());
    let calls = fx.outlet.take();
    shown_id(&calls);
    assert!(r.current_bubble().unwrap().replay);
}

#[test]
fn starting_paused_holds_reminders_back() {
    let fx = Fixture::new();
    fx.storage.create_task("Later", t0()).unwrap();
    let r = fx.start(true);
    assert!(r.is_paused());
    assert!(r.current_bubble().is_none());
}

#[test]
fn mark_done_updates_storage_and_handles_the_reminder() {
    let fx = Fixture::new();
    let task = fx.storage.create_task("Pay bill", t0()).unwrap();
    let r = fx.start(false);
    shown_id(&fx.outlet.take());

    // The `set_task_done` path: storage update, then `tasks_changed`.
    let done = fx.storage.set_task_done(&task.id, true).unwrap();
    assert!(done.done);
    r.tasks_changed().unwrap();
    assert_eq!(
        fx.outlet.take(),
        [Call::Cleared, Call::Buddy(BuddyEvent::BubbleDismissed)]
    );
    r.flush();
    assert!(matches!(
        fx.task_status(&task.id),
        ReminderStatus::Handled {
            outcome: ReminderOutcome::MarkedDone,
            ..
        }
    ));
}

#[test]
fn tasks_changed_picks_up_new_tasks() {
    let fx = Fixture::new();
    let r = fx.start(false);
    assert!(fx.outlet.take().is_empty());
    fx.storage.create_task("New", t0()).unwrap();
    r.tasks_changed().unwrap();
    shown_id(&fx.outlet.take());
}

#[test]
fn buddy_capability_grants_the_reminder_commands() {
    let buddy: serde_json::Value =
        serde_json::from_str(include_str!("../../capabilities/buddy.json")).unwrap();
    let perms: Vec<&str> = buddy["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    for cmd in [
        "get_current_bubble",
        "dismiss_bubble",
        "snooze_bubble",
        "set_task_done",
    ] {
        let spec = crate::domain::ipc::COMMANDS
            .iter()
            .find(|c| c.name == cmd)
            .expect("command is in the contract");
        assert_eq!(spec.windows, [crate::domain::ipc::Window::Buddy]);
        let perm = format!("allow-{}", cmd.replace('_', "-"));
        assert!(perms.contains(&perm.as_str()), "buddy lacks {perm}");
    }
}
