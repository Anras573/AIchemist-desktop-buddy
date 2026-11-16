//! Tests for the scheduler core, driven by `FakeClock`.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};

use super::{BubbleChange, Scheduler, Update};
use crate::domain::ipc::BubbleView;
use crate::domain::{
    AccountId, BubbleId, CalendarId, Clock, Error, Event, EventId, EventKey, EventTime, FakeClock,
    ReminderInstance, ReminderKey, ReminderOutcome, ReminderStatus, ResponseStatus, Settings, Task,
    TaskId,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 2026-11-16 at `h:m` UTC.
fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, h, m, 0).unwrap()
}

fn mins(m: i64) -> Duration {
    Duration::minutes(m)
}

fn event(id: &str, start: DateTime<Utc>, minutes: i64) -> Event {
    Event {
        id: EventId::new(id),
        account_id: AccountId::new("acc"),
        calendar_id: CalendarId::new("cal"),
        title: format!("meeting {id}"),
        time: EventTime::Timed {
            start,
            end: start + mins(minutes),
        },
        location: Some("Room 1".into()),
        response: ResponseStatus::Accepted,
    }
}

fn all_day(id: &str, day: NaiveDate) -> Event {
    Event {
        time: EventTime::AllDay {
            start: day,
            end: day.succ_opt().unwrap(),
        },
        ..event(id, at(0, 0), 0)
    }
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

fn done(mut t: Task, when: DateTime<Utc>) -> Task {
    t.done = true;
    t.done_at = Some(when);
    t
}

fn mkey(id: &str, lead: u32) -> ReminderKey {
    ReminderKey::Meeting {
        event: EventKey {
            account_id: AccountId::new("acc"),
            calendar_id: CalendarId::new("cal"),
            event_id: EventId::new(id),
        },
        lead_minutes: lead,
    }
}

fn tkey(id: &str) -> ReminderKey {
    ReminderKey::Task {
        task_id: TaskId::new(id),
    }
}

fn quiet_settings(start: (u32, u32), end: (u32, u32)) -> Settings {
    let mut s = Settings::default();
    s.system.quiet_hours.enabled = true;
    s.system.quiet_hours.start = NaiveTime::from_hms_opt(start.0, start.1, 0).unwrap();
    s.system.quiet_hours.end = NaiveTime::from_hms_opt(end.0, end.1, 0).unwrap();
    s
}

/// A scheduler plus its fake clock.
struct H {
    clock: FakeClock,
    s: Scheduler,
}

impl H {
    fn new(now: DateTime<Utc>) -> Self {
        Self::with(now, Settings::default(), Vec::new())
    }

    fn with(now: DateTime<Utc>, settings: Settings, ledger: Vec<ReminderInstance>) -> Self {
        Self::with_clock(FakeClock::new(now), settings, ledger)
    }

    fn with_clock(clock: FakeClock, settings: Settings, ledger: Vec<ReminderInstance>) -> Self {
        let s = Scheduler::new(ledger, &settings, &clock);
        Self { clock, s }
    }

    /// Jumps the clock to `t` and ticks.
    fn tick_at(&mut self, t: DateTime<Utc>) -> Update {
        self.clock.set(t);
        self.s.tick(&self.clock)
    }

    fn events(&mut self, events: Vec<Event>) -> Update {
        self.s.set_events(events, &self.clock)
    }

    fn tasks(&mut self, tasks: Vec<Task>) -> Update {
        self.s.set_tasks(tasks, &self.clock)
    }

    fn settings(&mut self, settings: &Settings) -> Update {
        self.s.set_settings(settings, &self.clock)
    }

    fn pause(&mut self, paused: bool) -> Update {
        self.s.set_paused(paused, &self.clock)
    }

    fn dismiss(&mut self, id: BubbleId) -> Update {
        self.s.dismiss(id, &self.clock).unwrap()
    }

    fn snooze(&mut self, id: BubbleId, minutes: u32) -> Update {
        self.s.snooze(id, minutes, &self.clock).unwrap()
    }

    fn next_wake(&self) -> Option<DateTime<Utc>> {
        self.s.next_wake(&self.clock)
    }

    fn status(&self, key: &ReminderKey) -> Option<ReminderStatus> {
        self.s
            .ledger()
            .into_iter()
            .find(|i| &i.key == key)
            .map(|i| i.status)
    }

    fn outcome(&self, key: &ReminderKey) -> Option<ReminderOutcome> {
        match self.status(key)? {
            ReminderStatus::Handled { outcome, .. } => Some(outcome),
            _ => None,
        }
    }

    /// Dismisses the bubble on screen; panics if there is none.
    fn dismiss_current(&mut self) -> Update {
        let id = self.s.current_bubble().expect("a bubble is shown").id;
        self.dismiss(id)
    }
}

/// The view of a `Shown` change; panics otherwise.
fn shown(update: &Update) -> BubbleView {
    match &update.bubble {
        Some(BubbleChange::Shown(v)) => v.clone(),
        other => panic!("expected a new bubble, got {other:?}"),
    }
}

/// The view of an `Updated` change; panics otherwise.
fn updated(update: &Update) -> BubbleView {
    match &update.bubble {
        Some(BubbleChange::Updated(v)) => v.clone(),
        other => panic!("expected a bubble update, got {other:?}"),
    }
}

fn keys(view: &BubbleView) -> Vec<ReminderKey> {
    view.items.iter().map(|i| i.key.clone()).collect()
}

fn is_cleared(update: &Update) -> bool {
    update.bubble == Some(BubbleChange::Cleared)
}

// ---------------------------------------------------------------------------
// Meeting reminders
// ---------------------------------------------------------------------------

#[test]
fn each_meeting_lead_time_fires_once() {
    let mut h = H::new(at(9, 0));
    let u = h.events(vec![event("a", at(10, 0), 30)]);
    assert!(u.bubble.is_none());
    assert!(u.ledger_changed, "new reminders are persisted");
    assert_eq!(h.next_wake(), Some(at(9, 50)));

    for (lead, due) in [(10, at(9, 50)), (5, at(9, 55)), (1, at(9, 59))] {
        assert!(h.tick_at(due - Duration::seconds(1)).bubble.is_none());
        let view = shown(&h.tick_at(due));
        assert_eq!(keys(&view), vec![mkey("a", lead)]);
        let item = &view.items[0];
        assert_eq!(item.title, "meeting a");
        assert_eq!(item.at, at(10, 0));
        assert_eq!(item.location.as_deref(), Some("Room 1"));
        assert!(!item.all_day);
        assert_eq!(view.queued, 0);
        assert!(!view.replay);
        assert_eq!(view.snooze_options_minutes, vec![5]);

        // Ticking again at the same instant does not re-fire.
        assert!(h.tick_at(due).bubble.is_none());
        assert!(is_cleared(&h.dismiss(view.id)));
        assert_eq!(
            h.outcome(&mkey("a", lead)),
            Some(ReminderOutcome::Dismissed)
        );
        assert!(h.tick_at(due + Duration::seconds(30)).bubble.is_none());
    }
    assert_eq!(h.next_wake(), None);
    assert!(h.tick_at(at(10, 30)).bubble.is_none());
}

#[test]
fn lead_time_zero_fires_at_start() {
    let mut s = Settings::default();
    s.reminders.lead_times_minutes = vec![0];
    let mut h = H::with(at(9, 0), s, Vec::new());
    h.events(vec![event("a", at(10, 0), 30)]);
    assert_eq!(h.next_wake(), Some(at(10, 0)));
    assert_eq!(keys(&shown(&h.tick_at(at(10, 0)))), vec![mkey("a", 0)]);
}

#[test]
fn no_retroactive_firing_at_launch() {
    // App launches at 09:30. Meeting "old" started at 09:00; meeting "soon"
    // starts at 09:33, so only its 1 min reminder is still ahead.
    let mut h = H::new(at(9, 30));
    let u = h.events(vec![
        event("old", at(9, 0), 60),
        event("soon", at(9, 33), 30),
    ]);
    assert!(u.bubble.is_none(), "nothing fires retroactively");
    for lead in [10, 5, 1] {
        assert_eq!(
            h.outcome(&mkey("old", lead)),
            Some(ReminderOutcome::Skipped)
        );
    }
    assert_eq!(h.outcome(&mkey("soon", 10)), Some(ReminderOutcome::Skipped));
    assert_eq!(h.outcome(&mkey("soon", 5)), Some(ReminderOutcome::Skipped));
    assert_eq!(h.status(&mkey("soon", 1)), Some(ReminderStatus::Pending));
    assert_eq!(h.next_wake(), Some(at(9, 32)));
    assert_eq!(keys(&shown(&h.tick_at(at(9, 32)))), vec![mkey("soon", 1)]);
}

#[test]
fn persisted_meeting_reminders_missed_while_closed_are_skipped() {
    let ledger = vec![
        ReminderInstance {
            key: mkey("a", 10),
            due_at: at(9, 50),
            status: ReminderStatus::Pending,
        },
        ReminderInstance {
            key: mkey("a", 5),
            due_at: at(9, 55),
            status: ReminderStatus::Pending,
        },
        ReminderInstance {
            key: tkey("t"),
            due_at: at(9, 0),
            status: ReminderStatus::Pending,
        },
    ];
    // Relaunched at 09:52: the 09:50 meeting reminder was missed.
    let mut h = H::with(at(9, 52), Settings::default(), ledger);
    assert_eq!(h.outcome(&mkey("a", 10)), Some(ReminderOutcome::Skipped));
    assert_eq!(h.status(&mkey("a", 5)), Some(ReminderStatus::Pending));

    let u = h.events(vec![event("a", at(10, 0), 30)]);
    assert!(u.ledger_changed, "the launch rule's change is reported");
    assert!(u.bubble.is_none());
    // The task that fell due while closed fires once now.
    let view = shown(&h.tasks(vec![task("t", at(9, 0))]));
    assert_eq!(keys(&view), vec![tkey("t")]);
    h.dismiss(view.id);
    assert_eq!(h.next_wake(), Some(at(9, 55)));
}

#[test]
fn a_meeting_appearing_late_fires_only_lead_times_still_ahead() {
    let mut h = H::new(at(9, 57));
    // Added by a sync 3 minutes before it starts.
    assert!(h.events(vec![event("a", at(10, 0), 30)]).bubble.is_none());
    assert_eq!(h.outcome(&mkey("a", 10)), Some(ReminderOutcome::Skipped));
    assert_eq!(h.outcome(&mkey("a", 5)), Some(ReminderOutcome::Skipped));
    assert_eq!(h.next_wake(), Some(at(9, 59)));
    assert_eq!(keys(&shown(&h.tick_at(at(9, 59)))), vec![mkey("a", 1)]);

    // Added 30 s before it starts: every lead time is past, nothing fires.
    let mut h = H::new(at(10, 0) - Duration::seconds(30));
    assert!(h.events(vec![event("b", at(10, 0), 30)]).bubble.is_none());
    assert_eq!(h.next_wake(), None);
}

#[test]
fn a_moved_meeting_is_re_armed() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    let view = shown(&h.tick_at(at(9, 50)));
    h.dismiss(view.id);
    // Moved to 11:00 by a sync.
    h.events(vec![event("a", at(11, 0), 30)]);
    assert_eq!(h.status(&mkey("a", 10)), Some(ReminderStatus::Pending));
    assert_eq!(h.next_wake(), Some(at(10, 50)));
    assert_eq!(keys(&shown(&h.tick_at(at(10, 50)))), vec![mkey("a", 10)]);
}

#[test]
fn a_meeting_removed_or_declined_leaves_the_screen() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    shown(&h.tick_at(at(9, 50)));
    // Cancelled a minute later: gone from the cache.
    h.clock.set(at(9, 51));
    assert!(is_cleared(&h.events(vec![])));
    assert_eq!(h.next_wake(), None);
    // Past entries for it are pruned, future ones kept.
    assert_eq!(h.status(&mkey("a", 10)), None);
    assert_eq!(h.status(&mkey("a", 5)), Some(ReminderStatus::Pending));

    // Declined while shown.
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    shown(&h.tick_at(at(9, 50)));
    let mut declined = event("a", at(10, 0), 30);
    declined.response = ResponseStatus::Declined;
    assert!(is_cleared(&h.events(vec![declined])));
}

#[test]
fn a_title_change_updates_the_bubble_in_place() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    let first = shown(&h.tick_at(at(9, 50)));
    let mut renamed = event("a", at(10, 0), 30);
    renamed.title = "renamed".into();
    let view = updated(&h.events(vec![renamed]));
    assert_eq!(view.id, first.id);
    assert_eq!(view.items[0].title, "renamed");
}

#[test]
fn waking_from_sleep_shows_one_reminder_per_meeting() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    // The computer slept from 09:40 to 09:58: the 10 and 5 min reminders
    // are both due at once, only the most recent one is shown.
    let view = shown(&h.tick_at(at(9, 58)));
    assert_eq!(keys(&view), vec![mkey("a", 5)]);
    assert_eq!(view.queued, 0);
    assert_eq!(h.outcome(&mkey("a", 10)), Some(ReminderOutcome::Skipped));
}

#[test]
fn a_meeting_in_progress_still_fires_but_an_ended_one_does_not() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30), event("b", at(10, 0), 60)]);
    // Slept through until 10:45: "a" ended at 10:30, "b" runs until 11:00.
    let view = shown(&h.tick_at(at(10, 45)));
    assert_eq!(keys(&view), vec![mkey("b", 1)]);
    assert_eq!(view.queued, 0);
    for lead in [10, 5, 1] {
        assert_eq!(h.outcome(&mkey("a", lead)), Some(ReminderOutcome::Skipped));
    }
}

#[test]
fn skip_all_day_toggle() {
    let day = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
    // Local time zone UTC+2: the all-day event starts at 22:00 UTC on the 16th.
    let clock = FakeClock::with_offset(at(12, 0), FixedOffset::east_opt(2 * 3600).unwrap());
    let mut h = H::with_clock(clock, Settings::default(), Vec::new());
    h.events(vec![all_day("holiday", day)]);
    assert_eq!(h.next_wake(), None, "skipped by default");
    assert!(h.s.ledger().is_empty());

    let mut s = Settings::default();
    s.reminders.skip_all_day = false;
    h.settings(&s);
    assert_eq!(h.next_wake(), Some(at(21, 50)));
    let view = shown(&h.tick_at(at(21, 50)));
    assert!(view.items[0].all_day);
    assert_eq!(view.items[0].at, at(22, 0));
}

#[test]
fn skip_declined_toggle() {
    let mut declined = event("a", at(10, 0), 30);
    declined.response = ResponseStatus::Declined;
    let mut h = H::new(at(9, 0));
    h.events(vec![declined]);
    assert_eq!(h.next_wake(), None, "skipped by default");

    let mut s = Settings::default();
    s.reminders.skip_declined = false;
    h.settings(&s);
    assert_eq!(h.next_wake(), Some(at(9, 50)));
    assert_eq!(keys(&shown(&h.tick_at(at(9, 50)))), vec![mkey("a", 10)]);
}

#[test]
fn changing_lead_times_takes_effect_immediately() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    shown(&h.tick_at(at(9, 50)));

    // Switch to [15, 3] at 09:50 while the 10 min reminder is on screen.
    let mut s = Settings::default();
    s.reminders.lead_times_minutes = vec![15, 3];
    let u = h.settings(&s);
    assert!(is_cleared(&u), "the removed lead time leaves the screen");
    // 15 min (09:45) is already past at first sight: skipped.
    assert_eq!(h.outcome(&mkey("a", 15)), Some(ReminderOutcome::Skipped));
    assert_eq!(h.next_wake(), Some(at(9, 57)));
    assert_eq!(keys(&shown(&h.tick_at(at(9, 57)))), vec![mkey("a", 3)]);
}

// ---------------------------------------------------------------------------
// Snooze and dismiss
// ---------------------------------------------------------------------------

#[test]
fn snooze_re_fires_after_the_delay() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("t", at(10, 0))]);
    let view = shown(&h.tick_at(at(10, 0)));
    assert!(is_cleared(&h.snooze(view.id, 7)));
    assert_eq!(
        h.status(&tkey("t")),
        Some(ReminderStatus::Snoozed { until: at(10, 7) })
    );
    assert_eq!(h.next_wake(), Some(at(10, 7)));
    assert!(h.tick_at(at(10, 6)).bubble.is_none());
    let again = shown(&h.tick_at(at(10, 7)));
    assert_eq!(keys(&again), vec![tkey("t")]);
    assert_ne!(again.id, view.id, "a re-fire is a new bubble");
    assert_eq!(h.status(&tkey("t")), Some(ReminderStatus::Pending));
    h.dismiss(again.id);
    assert_eq!(h.next_wake(), None);
}

#[test]
fn a_snoozed_meeting_reminder_collapses_into_the_next_lead_time() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    let view = shown(&h.tick_at(at(9, 50)));
    h.snooze(view.id, 5);
    // 09:55: the snoozed 10 min reminder and the 5 min one are both due.
    let view = shown(&h.tick_at(at(9, 55)));
    assert_eq!(keys(&view), vec![mkey("a", 5)]);
    assert_eq!(view.queued, 0);
    assert_eq!(h.outcome(&mkey("a", 10)), Some(ReminderOutcome::Skipped));
}

#[test]
fn a_snoozed_meeting_reminder_is_dropped_once_the_meeting_ended() {
    let mut s = Settings::default();
    s.reminders.lead_times_minutes = vec![1];
    let mut h = H::with(at(9, 0), s, Vec::new());
    h.events(vec![event("a", at(10, 0), 30)]);
    let view = shown(&h.tick_at(at(9, 59)));
    h.snooze(view.id, 60);
    assert!(h.tick_at(at(10, 59)).bubble.is_none());
    assert_eq!(h.outcome(&mkey("a", 1)), Some(ReminderOutcome::Skipped));
}

#[test]
fn a_snooze_survives_the_meeting_briefly_leaving_the_cache() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    let view = shown(&h.tick_at(at(9, 50)));
    h.snooze(view.id, 2);
    h.clock.set(at(9, 51));
    h.events(vec![]);
    h.events(vec![event("a", at(10, 0), 30)]);
    assert_eq!(
        h.status(&mkey("a", 10)),
        Some(ReminderStatus::Snoozed { until: at(9, 52) })
    );
    assert_eq!(keys(&shown(&h.tick_at(at(9, 52)))), vec![mkey("a", 10)]);
}

#[test]
fn ledger_parts_are_not_pruned_before_their_input_is_loaded() {
    let ledger = vec![ReminderInstance {
        key: tkey("t"),
        due_at: at(8, 0),
        status: ReminderStatus::Handled {
            at: at(8, 0),
            outcome: ReminderOutcome::Dismissed,
        },
    }];
    let mut h = H::with(at(9, 0), Settings::default(), ledger);
    // Events arrive before tasks at startup.
    h.events(vec![]);
    assert_eq!(h.outcome(&tkey("t")), Some(ReminderOutcome::Dismissed));
    assert!(h.tasks(vec![task("t", at(8, 0))]).bubble.is_none());
}

#[test]
fn bubble_actions_validate_their_input() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("t", at(9, 0))]);
    let view = h.s.current_bubble().unwrap();
    let stale = BubbleId(view.id.0 + 1);
    assert!(matches!(
        h.s.dismiss(stale, &h.clock),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        h.s.snooze(stale, 5, &h.clock),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        h.s.snooze(view.id, 0, &h.clock),
        Err(Error::InvalidInput { .. })
    ));
    assert!(matches!(
        h.s.snooze_all(121, &h.clock),
        Err(Error::InvalidInput { .. })
    ));
    // Failed actions changed nothing.
    assert_eq!(h.s.current_bubble(), Some(view.clone()));
    h.dismiss(view.id);
    assert!(matches!(
        h.s.dismiss(view.id, &h.clock),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn simultaneous_reminders_queue_with_a_count() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("a", at(10, 0)), task("b", at(10, 0))]);
    h.events(vec![event("m", at(10, 10), 30)]);
    // Task a, task b and the 10 min reminder for "m" are due at 10:00.
    let first = shown(&h.tick_at(at(10, 0)));
    assert_eq!(first.queued, 2);
    let second = shown(&h.dismiss(first.id));
    assert_eq!(second.queued, 1);
    let third = shown(&h.snooze(second.id, 5));
    assert_eq!(third.queued, 0);
    let mut all = [keys(&first), keys(&second), keys(&third)].concat();
    all.sort();
    assert_eq!(all, vec![mkey("m", 10), tkey("a"), tkey("b")]);
    assert!(is_cleared(&h.dismiss(third.id)));
}

#[test]
fn a_new_reminder_behind_the_bubble_bumps_the_count() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("a", at(10, 0)), task("b", at(10, 1))]);
    let first = shown(&h.tick_at(at(10, 0)));
    assert_eq!(first.queued, 0);
    let view = updated(&h.tick_at(at(10, 1)));
    assert_eq!(view.id, first.id);
    assert_eq!(view.queued, 1);
}

#[test]
fn snooze_all_covers_the_bubble_and_the_queue() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![
        task("a", at(10, 0)),
        task("b", at(10, 0)),
        task("c", at(10, 0)),
    ]);
    assert_eq!(shown(&h.tick_at(at(10, 0))).queued, 2);
    let u = h.s.snooze_all(5, &h.clock).unwrap();
    assert!(is_cleared(&u));
    assert!(u.ledger_changed);
    for id in ["a", "b", "c"] {
        assert_eq!(
            h.status(&tkey(id)),
            Some(ReminderStatus::Snoozed { until: at(10, 5) })
        );
    }
    assert_eq!(h.next_wake(), Some(at(10, 5)));
    assert_eq!(shown(&h.tick_at(at(10, 5))).queued, 2);
    // Nothing to snooze is a no-op.
    let mut idle = H::new(at(9, 0));
    assert_eq!(
        idle.s.snooze_all(5, &idle.clock).unwrap(),
        Update::default()
    );
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

#[test]
fn marking_a_task_done_handles_its_reminder() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("t", at(10, 0))]);
    shown(&h.tick_at(at(10, 0)));
    // The bubble's "Mark done" goes through set_task_done -> set_tasks.
    let u = h.tasks(vec![done(task("t", at(10, 0)), at(10, 1))]);
    assert!(is_cleared(&u));
    assert!(u.ledger_changed);
    assert_eq!(h.outcome(&tkey("t")), Some(ReminderOutcome::MarkedDone));
    assert_eq!(h.next_wake(), None);
    assert!(h.tick_at(at(12, 0)).bubble.is_none());
}

#[test]
fn a_done_task_never_fires() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![done(task("t", at(10, 0)), at(9, 0))]);
    assert_eq!(h.next_wake(), None);
    assert!(h.tick_at(at(10, 0)).bubble.is_none());
    assert!(h.s.ledger().is_empty());

    // Marked done before it fell due.
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("t", at(10, 0))]);
    h.tasks(vec![done(task("t", at(10, 0)), at(9, 30))]);
    assert_eq!(h.outcome(&tkey("t")), Some(ReminderOutcome::MarkedDone));
    assert!(h.tick_at(at(10, 0)).bubble.is_none());
}

#[test]
fn an_overdue_task_fires_once_and_never_re_nags() {
    let mut h = H::new(at(9, 0));
    let view = shown(&h.tasks(vec![task("t", at(8, 0))]));
    assert_eq!(keys(&view), vec![tkey("t")]);
    h.dismiss(view.id);
    assert_eq!(h.next_wake(), None);
    assert!(h.tick_at(at(12, 0)).bubble.is_none());
    assert!(h.tasks(vec![task("t", at(8, 0))]).bubble.is_none());

    // It also survives a restart with the persisted ledger.
    let mut h = H::with(at(13, 0), Settings::default(), h.s.ledger());
    assert!(h.tasks(vec![task("t", at(8, 0))]).bubble.is_none());
}

#[test]
fn editing_the_due_time_re_arms_the_reminder() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("t", at(10, 0))]);
    let view = shown(&h.tick_at(at(10, 0)));
    h.dismiss(view.id);
    assert_eq!(h.next_wake(), None);

    h.tasks(vec![task("t", at(11, 0))]);
    assert_eq!(h.status(&tkey("t")), Some(ReminderStatus::Pending));
    assert_eq!(h.next_wake(), Some(at(11, 0)));
    assert_eq!(keys(&shown(&h.tick_at(at(11, 0)))), vec![tkey("t")]);

    // Editing while on screen takes it down until the new time.
    assert!(is_cleared(&h.tasks(vec![task("t", at(12, 0))])));
    assert_eq!(h.next_wake(), Some(at(12, 0)));
    // Editing a snoozed task re-arms it at the new time.
    let view = shown(&h.tick_at(at(12, 0)));
    h.snooze(view.id, 30);
    h.tasks(vec![task("t", at(12, 10))]);
    assert_eq!(h.next_wake(), Some(at(12, 10)));
}

#[test]
fn marking_a_task_undone_re_arms_it_only_if_still_ahead() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![done(task("t", at(10, 0)), at(9, 0))]);
    h.tasks(vec![task("t", at(10, 0))]);
    h.tasks(vec![done(task("t", at(10, 0)), at(9, 0))]);
    h.tasks(vec![task("t", at(10, 0))]);
    assert_eq!(h.next_wake(), Some(at(10, 0)));

    // Marked done before it fired, undone once overdue: no nag.
    h.tasks(vec![done(task("t", at(10, 0)), at(9, 0))]);
    h.clock.set(at(11, 0));
    assert!(h.tasks(vec![task("t", at(10, 0))]).bubble.is_none());
    assert_eq!(h.outcome(&tkey("t")), Some(ReminderOutcome::MarkedDone));
}

#[test]
fn a_deleted_task_leaves_the_screen_and_the_ledger() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("t", at(9, 0))]);
    assert!(h.s.current_bubble().is_some());
    assert!(is_cleared(&h.tasks(vec![])));
    assert!(h.s.ledger().is_empty());
}

#[test]
fn removing_one_item_of_a_replay_bubble_updates_it_in_place() {
    let mut h = H::new(at(9, 0));
    h.pause(true);
    h.tasks(vec![task("a", at(9, 5)), task("b", at(9, 6))]);
    h.tick_at(at(9, 10));
    let view = shown(&h.pause(false));
    assert_eq!(keys(&view), vec![tkey("a"), tkey("b")]);
    let after = updated(&h.tasks(vec![
        done(task("a", at(9, 5)), at(9, 11)),
        task("b", at(9, 6)),
    ]));
    assert_eq!(after.id, view.id);
    assert_eq!(keys(&after), vec![tkey("b")]);
}

// ---------------------------------------------------------------------------
// Pause
// ---------------------------------------------------------------------------

#[test]
fn pause_collects_and_replays_one_batch() {
    let mut h = H::new(at(9, 0));
    h.events(vec![
        // Started during the pause: its reminders are not replayed.
        event("started", at(9, 25), 60),
        // Not started yet at unpause: replayed once (smallest lead).
        event("later", at(9, 40), 30),
    ]);
    h.tasks(vec![task("t1", at(9, 10)), task("t2", at(9, 20))]);
    assert!(h.pause(true).bubble.is_none());
    assert!(h.s.is_paused());

    // Nothing is shown while paused, but due reminders are collected.
    for m in [10, 15, 20, 24, 30, 35] {
        assert!(h.tick_at(at(9, m)).bubble.is_none(), "09:{m}");
    }
    h.clock.set(at(9, 36));
    let view = shown(&h.pause(false));
    assert!(view.replay);
    assert_eq!(view.queued, 0);
    assert_eq!(keys(&view), vec![tkey("t1"), tkey("t2"), mkey("later", 5)]);
    for lead in [10, 5, 1] {
        assert_eq!(
            h.outcome(&mkey("started", lead)),
            Some(ReminderOutcome::Skipped)
        );
    }
    assert_eq!(
        h.outcome(&mkey("later", 10)),
        Some(ReminderOutcome::Skipped)
    );
    assert_eq!(h.status(&mkey("later", 5)), Some(ReminderStatus::Pending));

    // Later reminders queue behind the replay bubble as usual.
    assert_eq!(h.next_wake(), Some(at(9, 39)));
    assert_eq!(updated(&h.tick_at(at(9, 39))).queued, 1);
    // Dismissing the batch handles every item.
    let next = shown(&h.dismiss(view.id));
    assert!(!next.replay);
    assert_eq!(keys(&next), vec![mkey("later", 1)]);
    for key in [tkey("t1"), tkey("t2"), mkey("later", 5)] {
        assert_eq!(h.outcome(&key), Some(ReminderOutcome::Dismissed));
    }
}

#[test]
fn pausing_takes_the_bubble_down_and_replays_it() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("a", at(9, 0)), task("b", at(9, 0))]);
    assert!(h.s.current_bubble().is_some());
    assert!(is_cleared(&h.pause(true)));
    assert_eq!(h.s.current_bubble(), None);
    assert_eq!(h.next_wake(), None, "held-back reminders need no timer");
    h.clock.set(at(9, 5));
    let view = shown(&h.pause(false));
    assert!(view.replay);
    assert_eq!(keys(&view), vec![tkey("a"), tkey("b")]);
}

#[test]
fn unpausing_with_nothing_missed_shows_nothing() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("a", at(10, 0))]);
    h.pause(true);
    h.tick_at(at(9, 30));
    assert!(h.pause(false).bubble.is_none());
    assert_eq!(h.next_wake(), Some(at(10, 0)));
}

#[test]
fn snooze_all_while_paused_covers_held_back_reminders() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("a", at(9, 5))]);
    h.pause(true);
    h.tick_at(at(9, 5));
    h.s.snooze_all(10, &h.clock).unwrap();
    assert!(h.pause(false).bubble.is_none());
    assert_eq!(h.next_wake(), Some(at(9, 15)));
}

// ---------------------------------------------------------------------------
// Quiet hours
// ---------------------------------------------------------------------------

#[test]
fn quiet_hours_spanning_midnight_hold_back_and_replay() {
    let mut h = H::with(at(21, 0), quiet_settings((22, 0), (7, 0)), Vec::new());
    let next_morning = |h: u32, m: u32| at(h, m) + Duration::days(1);
    h.tasks(vec![
        task("late", at(23, 0)),
        task("night", next_morning(3, 0)),
    ]);
    // Meeting at 02:00 starts during quiet hours: not replayed.
    // Meeting at 07:30 has not started at 07:00: replayed.
    h.events(vec![
        event("night", next_morning(2, 0), 30),
        event("morning", next_morning(7, 30), 30),
    ]);
    // The quiet-hours start is a wake-up point.
    assert_eq!(h.next_wake(), Some(at(22, 0)));
    assert!(h.tick_at(at(22, 0)).bubble.is_none());
    assert!(h.s.is_suppressed());
    assert_eq!(h.next_wake(), Some(at(23, 0)));
    assert!(h.tick_at(at(23, 0)).bubble.is_none());
    for t in [next_morning(1, 50), next_morning(1, 59), next_morning(3, 0)] {
        assert!(h.tick_at(t).bubble.is_none());
    }
    // Wakes again at the end of quiet hours (07:00 local).
    assert_eq!(h.next_wake(), Some(next_morning(7, 0)));
    let view = shown(&h.tick_at(next_morning(7, 0)));
    assert!(view.replay);
    assert_eq!(keys(&view), vec![tkey("late"), tkey("night")]);
    assert_eq!(h.outcome(&mkey("night", 1)), Some(ReminderOutcome::Skipped));
    assert!(!h.s.is_suppressed());
    // The 07:30 meeting fires normally after quiet hours.
    assert_eq!(h.next_wake(), Some(next_morning(7, 20)));
}

#[test]
fn quiet_hours_use_the_clocks_local_time() {
    // UTC+1: quiet hours 22:00-07:00 local are 21:00-06:00 UTC.
    let plus_one = FixedOffset::east_opt(3600).unwrap();
    let clock = FakeClock::with_offset(at(20, 0), plus_one);
    let mut h = H::with_clock(clock, quiet_settings((22, 0), (7, 0)), Vec::new());
    h.tasks(vec![task("t", at(21, 30))]);
    assert_eq!(h.next_wake(), Some(at(21, 0)));
    assert!(
        h.tick_at(at(21, 30)).bubble.is_none(),
        "22:30 local is quiet"
    );
    let morning = at(6, 0) + Duration::days(1);
    assert_eq!(h.next_wake(), Some(morning));
    assert!(shown(&h.tick_at(morning)).replay);

    // UTC-5: the same window is 03:00-12:00 UTC, so 21:30 UTC is not quiet.
    let minus_five = FixedOffset::west_opt(5 * 3600).unwrap();
    let clock = FakeClock::with_offset(at(20, 0), minus_five);
    let mut h = H::with_clock(clock, quiet_settings((22, 0), (7, 0)), Vec::new());
    h.tasks(vec![task("t", at(21, 30))]);
    assert!(!shown(&h.tick_at(at(21, 30))).replay);
}

#[test]
fn quiet_hours_follow_a_utc_offset_change() {
    // The window is recomputed from local time on every evaluation, so a DST
    // switch (modelled as the local offset changing from +2 to +1) moves the
    // boundaries with it instead of keeping stale instants.
    let s = quiet_settings((22, 0), (7, 0));
    let summer = FakeClock::with_offset(at(19, 0), FixedOffset::east_opt(2 * 3600).unwrap());
    let winter = FakeClock::with_offset(at(19, 0), FixedOffset::east_opt(3600).unwrap());
    let mut sched = Scheduler::new(Vec::new(), &s, &summer);
    sched.tick(&summer);
    assert_eq!(sched.next_wake(&summer), Some(at(20, 0)));
    sched.tick(&winter);
    assert_eq!(sched.next_wake(&winter), Some(at(21, 0)));
    // 20:30 UTC is 22:30 in summer time (quiet) but 21:30 in winter time.
    summer.set(at(20, 30));
    winter.set(at(20, 30));
    sched.tick(&summer);
    assert!(sched.is_suppressed());
    sched.tick(&winter);
    assert!(!sched.is_suppressed());
}

#[test]
fn quiet_hours_within_one_day() {
    let mut h = H::with(at(11, 0), quiet_settings((12, 0), (13, 0)), Vec::new());
    h.tasks(vec![task("lunch", at(12, 30))]);
    assert_eq!(h.next_wake(), Some(at(12, 0)));
    h.tick_at(at(12, 0));
    assert_eq!(h.next_wake(), Some(at(12, 30)));
    assert!(h.tick_at(at(12, 30)).bubble.is_none());
    assert_eq!(h.next_wake(), Some(at(13, 0)));
    assert!(shown(&h.tick_at(at(13, 0))).replay);
}

#[test]
fn quiet_hours_and_pause_combine() {
    let mut h = H::with(at(21, 0), quiet_settings((22, 0), (7, 0)), Vec::new());
    h.tasks(vec![task("t", at(23, 0))]);
    h.pause(true);
    h.tick_at(at(23, 0));
    // Quiet hours end but still paused: nothing yet.
    let morning = at(7, 0) + Duration::days(1);
    assert!(h.tick_at(morning).bubble.is_none());
    assert!(shown(&h.pause(false)).replay);
}

#[test]
fn disabled_quiet_hours_add_no_wake_ups() {
    let mut s = quiet_settings((22, 0), (7, 0));
    s.system.quiet_hours.enabled = false;
    let mut h = H::with(at(21, 0), s, Vec::new());
    assert_eq!(h.next_wake(), None);
    h.tasks(vec![task("t", at(23, 0))]);
    assert!(!shown(&h.tick_at(at(23, 0))).replay);
}

// ---------------------------------------------------------------------------
// Next wake and bookkeeping
// ---------------------------------------------------------------------------

#[test]
fn next_wake_is_the_earliest_pending_fire_time() {
    let mut h = H::new(at(9, 0));
    assert_eq!(h.next_wake(), None);
    h.tasks(vec![task("t", at(9, 52))]);
    assert_eq!(h.next_wake(), Some(at(9, 52)));
    h.events(vec![event("a", at(10, 0), 30)]);
    assert_eq!(h.next_wake(), Some(at(9, 50)));
    // Shown reminders do not count; the next one does.
    shown(&h.tick_at(at(9, 50)));
    assert_eq!(h.next_wake(), Some(at(9, 52)));
    h.tick_at(at(9, 52));
    assert_eq!(h.next_wake(), Some(at(9, 55)));
}

#[test]
fn next_wake_is_always_after_the_last_evaluation() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(9, 30), 30), event("b", at(9, 45), 30)]);
    h.tasks(vec![task("t", at(9, 25)), task("u", at(9, 40))]);
    let mut t = at(9, 0);
    while t < at(11, 0) {
        h.tick_at(t);
        if let Some(wake) = h.next_wake() {
            assert!(wake > t, "next wake {wake} not after {t}");
        }
        t += mins(1);
    }
}

#[test]
fn quiet_ticks_report_no_changes() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    assert_eq!(h.tick_at(at(9, 10)), Update::default());
    let u = h.tick_at(at(9, 50));
    assert!(u.bubble.is_some());
    assert!(
        !u.ledger_changed,
        "showing a reminder keeps it pending (runtime-only state)"
    );
}

#[test]
fn the_ledger_round_trips_across_a_restart() {
    let mut h = H::new(at(9, 0));
    h.events(vec![event("a", at(10, 0), 30)]);
    h.tasks(vec![task("t", at(9, 50))]);
    let first = shown(&h.tick_at(at(9, 50)));
    h.dismiss(first.id);
    h.dismiss_current();
    let ledger = h.s.ledger();

    // Restart at 09:52: nothing handled fires again; the 5 min one is ahead.
    let mut h = H::with(at(9, 52), Settings::default(), ledger);
    assert!(h.events(vec![event("a", at(10, 0), 30)]).bubble.is_none());
    assert!(h.tasks(vec![task("t", at(9, 50))]).bubble.is_none());
    assert_eq!(h.next_wake(), Some(at(9, 55)));
}

#[test]
fn update_reports_the_bubble_view_for_get_current_bubble() {
    let mut h = H::new(at(9, 0));
    h.tasks(vec![task("t", at(9, 0))]);
    let view = h.s.current_bubble().unwrap();
    assert_eq!(view.id, BubbleId(1));
    let clock: &dyn Clock = &h.clock;
    assert_eq!(h.s.next_wake(clock), None);
}
