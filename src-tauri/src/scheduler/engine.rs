//! The pure scheduler core: no threads, no I/O, no hidden time source.
//!
//! [`Scheduler`] owns the reminder ledger and the runtime-only bubble queue.
//! Every operation takes a [`Clock`], reads `now` from it exactly once, applies
//! the change, re-evaluates everything (reconcile the ledger against the
//! current events, tasks and settings, collect what fell due, handle pause and
//! quiet hours, promote the next bubble) and returns an [`Update`] describing
//! what the caller must emit or persist. The same inputs always give the same
//! outputs, so tests drive it with [`FakeClock`](crate::domain::FakeClock).
//!
//! See the module docs of [`crate::scheduler`] for the rules.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Duration, NaiveDate, Utc};

use crate::domain::ipc::{BubbleItem, BubbleView};
use crate::domain::{
    BubbleId, Clock, Error, Event, QuietHours, ReminderInstance, ReminderKey, ReminderOutcome,
    ReminderSettings, ReminderStatus, Result, Settings, Task,
};

/// How the bubble on screen changed in one scheduler operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BubbleChange {
    /// A new bubble (new [`BubbleId`]) replaces whatever was shown. This is
    /// the moment a reminder fires: put the cat in `alert`, play the chime
    /// and emit `reminder-shown`.
    Shown(BubbleView),
    /// The bubble on screen kept its id but its content changed: an item was
    /// removed (e.g. its task was marked done in the popover), a title changed
    /// after a sync, or the "+N" queue count changed. Re-emit
    /// `reminder-shown`; no chime.
    Updated(BubbleView),
    /// No bubble is shown any more (everything handled, or paused). Emit
    /// `reminder-cleared`.
    Cleared,
}

/// What one scheduler operation changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Update {
    /// The bubble change to emit, if any.
    pub bubble: Option<BubbleChange>,
    /// Whether the ledger changed and should be persisted
    /// (see [`Scheduler::ledger`]).
    pub ledger_changed: bool,
}

/// What a reminder points at, resolved from the current events and tasks.
#[derive(Debug, Clone)]
struct Source {
    title: String,
    /// Meeting start, or task due time.
    at: DateTime<Utc>,
    /// Meeting end; `None` for tasks.
    end: Option<DateTime<Utc>>,
    all_day: bool,
    location: Option<String>,
}

/// Reminders shown (or to be shown) together in one bubble.
#[derive(Debug, Clone)]
struct Batch {
    keys: Vec<ReminderKey>,
    replay: bool,
}

/// The pure scheduler core. Every operation takes a [`Clock`], reads `now`
/// once, re-evaluates everything and returns an [`Update`]. See
/// [`crate::scheduler`] for the rules.
#[derive(Debug)]
pub struct Scheduler {
    // ----- Inputs -----
    /// `None` until the first [`Scheduler::set_events`]: until then the
    /// meeting part of the ledger is left alone (not pruned).
    events: Option<Vec<Event>>,
    /// `None` until the first [`Scheduler::set_tasks`]: until then the task
    /// part of the ledger is left alone (not pruned).
    tasks: Option<Vec<Task>>,
    reminders: ReminderSettings,
    quiet_hours: QuietHours,
    paused: bool,

    // ----- Persisted state -----
    ledger: BTreeMap<ReminderKey, ReminderInstance>,
    /// Set when [`Scheduler::new`] changed the ledger, so the first operation
    /// reports `ledger_changed`.
    ledger_dirty: bool,

    // ----- Derived on every evaluation -----
    /// Every reminder that may fire right now (its event passes the filters
    /// and its lead time is configured, or its task exists and is open).
    sources: BTreeMap<ReminderKey, Source>,

    // ----- Runtime-only state (never persisted) -----
    /// The bubble on screen.
    current: Option<(BubbleId, Batch)>,
    /// Bubbles waiting behind the current one.
    queue: VecDeque<Batch>,
    /// Reminders that fell due while suppressed (paused or quiet hours),
    /// replayed as one batch when suppression ends.
    backlog: Vec<ReminderKey>,
    /// Whether reminders were suppressed at the last evaluation.
    suppressed: bool,
    last_bubble_id: u64,
    /// The view returned by the last evaluation, to diff against.
    last_view: Option<BubbleView>,
}

impl Scheduler {
    /// Creates a scheduler at app launch from the persisted `ledger`.
    ///
    /// Applies the launch rule (spec 7.1): a meeting reminder whose fire time
    /// passed while the app was not running is marked
    /// [`Skipped`](ReminderOutcome::Skipped) and never fires. Task reminders
    /// are kept, so a task that fell due while the app was closed fires once
    /// at launch.
    ///
    /// Nothing is evaluated yet: feed events, tasks and pause state with the
    /// `set_*` methods (or call [`tick`](Self::tick)). The first operation
    /// reports `ledger_changed` if the launch rule changed anything.
    pub fn new(
        ledger: impl IntoIterator<Item = ReminderInstance>,
        settings: &Settings,
        clock: &dyn Clock,
    ) -> Self {
        let now = clock.now();
        let mut ledger_dirty = false;
        let ledger = ledger
            .into_iter()
            .map(|mut inst| {
                let missed = inst.next_fire_at().is_some_and(|t| t < now);
                if missed && matches!(inst.key, ReminderKey::Meeting { .. }) {
                    inst.status = skipped(now);
                    ledger_dirty = true;
                }
                (inst.key.clone(), inst)
            })
            .collect();
        Self {
            events: None,
            tasks: None,
            reminders: settings.reminders.clone(),
            quiet_hours: settings.system.quiet_hours,
            paused: false,
            ledger,
            ledger_dirty,
            sources: BTreeMap::new(),
            current: None,
            queue: VecDeque::new(),
            backlog: Vec::new(),
            suppressed: false,
            last_bubble_id: 0,
            last_view: None,
        }
    }

    // ------------------------------------------------------------------
    // Operations
    // ------------------------------------------------------------------

    /// Re-evaluates at `clock.now()`: fires what is due. The driver calls
    /// this when its timer expires.
    pub fn tick(&mut self, clock: &dyn Clock) -> Update {
        self.run(clock, |_, _| {})
    }

    /// Replaces the calendar events (the whole cache, after a sync or at
    /// launch).
    pub fn set_events(&mut self, events: Vec<Event>, clock: &dyn Clock) -> Update {
        self.run(clock, |s, _| s.events = Some(events))
    }

    /// Replaces the tasks (all of them, open and done) after any task change.
    /// Marking a task done (from the popover or the bubble's "Mark done")
    /// reaches the scheduler this way and handles its reminder with outcome
    /// [`MarkedDone`](ReminderOutcome::MarkedDone).
    pub fn set_tasks(&mut self, tasks: Vec<Task>, clock: &dyn Clock) -> Update {
        self.run(clock, |s, _| s.tasks = Some(tasks))
    }

    /// Applies new settings (lead times, filters, snooze options, quiet
    /// hours). Other sections are ignored.
    pub fn set_settings(&mut self, settings: &Settings, clock: &dyn Clock) -> Update {
        self.run(clock, |s, _| {
            s.reminders = settings.reminders.clone();
            s.quiet_hours = settings.system.quiet_hours;
        })
    }

    /// Pauses or resumes reminders (spec 7.5). Resuming replays what fell
    /// due while paused as one batch.
    pub fn set_paused(&mut self, paused: bool, clock: &dyn Clock) -> Update {
        self.run(clock, |s, _| s.paused = paused)
    }

    /// Dismisses every reminder in bubble `id`: each is handled with outcome
    /// [`Dismissed`](ReminderOutcome::Dismissed). Fails with `notFound` if
    /// `id` is not the bubble on screen (a stale click).
    pub fn dismiss(&mut self, id: BubbleId, clock: &dyn Clock) -> Result<Update> {
        self.check_current(id)?;
        Ok(self.run(clock, |s, now| {
            for key in s.current.take().map(|(_, b)| b.keys).unwrap_or_default() {
                s.set_status(
                    &key,
                    ReminderStatus::Handled {
                        at: now,
                        outcome: ReminderOutcome::Dismissed,
                    },
                );
            }
        }))
    }

    /// Snoozes every reminder in bubble `id` for `minutes`; each fires again
    /// after the delay. Fails with `notFound` for a stale id and
    /// `invalidInput` for a duration outside
    /// [`ReminderSettings::SNOOZE_RANGE`].
    pub fn snooze(&mut self, id: BubbleId, minutes: u32, clock: &dyn Clock) -> Result<Update> {
        check_snooze(minutes)?;
        self.check_current(id)?;
        Ok(self.run(clock, |s, now| {
            let keys = s.current.take().map(|(_, b)| b.keys).unwrap_or_default();
            s.snooze_keys(keys, now, minutes);
        }))
    }

    /// Snoozes everything waiting for the user: the bubble on screen, the
    /// bubbles queued behind it, and reminders held back by pause or quiet
    /// hours. A no-op when there is nothing. Fails with `invalidInput` for a
    /// duration outside [`ReminderSettings::SNOOZE_RANGE`].
    pub fn snooze_all(&mut self, minutes: u32, clock: &dyn Clock) -> Result<Update> {
        check_snooze(minutes)?;
        Ok(self.run(clock, |s, now| {
            let mut keys = Vec::new();
            if let Some((_, batch)) = s.current.take() {
                keys.extend(batch.keys);
            }
            for batch in s.queue.drain(..) {
                keys.extend(batch.keys);
            }
            keys.append(&mut s.backlog);
            s.snooze_keys(keys, now, minutes);
        }))
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    /// The single instant at which the scheduler next needs to run: the
    /// earliest fire time of a reminder that is not already shown, queued or
    /// held back, and (if quiet hours are enabled) the next quiet-hours
    /// boundary. `None` when nothing is scheduled.
    ///
    /// Right after an operation this is always later than that operation's
    /// `now`. If the clock has moved on since, it may be in the past; the
    /// caller should then [`tick`](Self::tick) immediately.
    pub fn next_wake(&self, clock: &dyn Clock) -> Option<DateTime<Utc>> {
        let reminders = self
            .ledger
            .values()
            .filter(|inst| self.sources.contains_key(&inst.key) && !self.is_active(&inst.key))
            .filter_map(ReminderInstance::next_fire_at)
            .min();
        let quiet = if self.quiet_hours.enabled {
            next_quiet_boundary(&self.quiet_hours, clock.now(), clock)
        } else {
            None
        };
        match (reminders, quiet) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The bubble on screen, if any (for `get_current_bubble`).
    pub fn current_bubble(&self) -> Option<BubbleView> {
        self.last_view.clone()
    }

    /// The whole ledger, sorted by key, for persistence. Shown and queued
    /// reminders are `Pending` here (that state is runtime-only).
    pub fn ledger(&self) -> Vec<ReminderInstance> {
        self.ledger.values().cloned().collect()
    }

    /// Whether the user paused reminders.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Whether bubbles were held back (paused or quiet hours) at the last
    /// evaluation.
    pub fn is_suppressed(&self) -> bool {
        self.suppressed
    }

    // ------------------------------------------------------------------
    // Evaluation
    // ------------------------------------------------------------------

    /// Runs `op` at a single `now`, then re-evaluates, and reports what
    /// changed.
    fn run(&mut self, clock: &dyn Clock, op: impl FnOnce(&mut Self, DateTime<Utc>)) -> Update {
        let now = clock.now();
        let before = self.ledger.clone();
        op(self, now);
        self.evaluate(now, clock);
        let ledger_changed = std::mem::take(&mut self.ledger_dirty) || self.ledger != before;
        Update {
            bubble: self.diff_view(),
            ledger_changed,
        }
    }

    fn evaluate(&mut self, now: DateTime<Utc>, clock: &dyn Clock) {
        self.reconcile(now, clock);
        self.drop_ended_meetings(now);

        let local_time = clock.utc_to_local(now).time();
        let quiet = self.quiet_hours.enabled && self.quiet_hours.contains(local_time);
        let suppressed = self.paused || quiet;

        // Suppression starts: hide everything and hold it back for replay.
        if suppressed && !self.suppressed {
            if let Some((_, batch)) = self.current.take() {
                self.backlog.extend(batch.keys);
            }
            for batch in self.queue.drain(..) {
                self.backlog.extend(batch.keys);
            }
        }

        // Collect what fell due. Anything collected at the evaluation that
        // ends suppression fell due while suppressed, so it joins the replay.
        let to_backlog = suppressed || self.suppressed;
        for key in self.newly_due(now) {
            if let Some(inst) = self.ledger.get_mut(&key) {
                // A re-fired snoozed reminder is pending again while shown.
                inst.status = ReminderStatus::Pending;
            }
            self.enqueue(key, to_backlog, now);
        }

        // Suppression ended: replay the backlog as one batch, in front.
        if !suppressed && !self.backlog.is_empty() {
            self.replay(now);
        }

        self.suppressed = suppressed;
        if !suppressed && self.current.is_none() {
            if let Some(batch) = self.queue.pop_front() {
                self.last_bubble_id += 1;
                self.current = Some((BubbleId(self.last_bubble_id), batch));
            }
        }
    }

    /// Brings the ledger in line with the current events, tasks and
    /// settings, and recomputes [`Self::sources`].
    fn reconcile(&mut self, now: DateTime<Utc>, clock: &dyn Clock) {
        let mut sources = BTreeMap::new();

        // Meetings: one reminder per (event, lead time).
        let leads: BTreeSet<u32> = self.reminders.lead_times_minutes.iter().copied().collect();
        let events = self.events.take();
        for event in events.iter().flatten() {
            if (self.reminders.skip_all_day && event.is_all_day())
                || (self.reminders.skip_declined && event.is_declined())
            {
                continue;
            }
            let start = event.time.start_utc(clock);
            let source = Source {
                title: event.title.clone(),
                at: start,
                end: Some(event.time.end_utc(clock)),
                all_day: event.is_all_day(),
                location: event.location.clone(),
            };
            for &lead in &leads {
                let key = ReminderKey::Meeting {
                    event: event.key(),
                    lead_minutes: lead,
                };
                let due = start - Duration::minutes(i64::from(lead));
                match self.ledger.get(&key) {
                    Some(inst) if inst.due_at == due => {}
                    existing => {
                        // First sight, or the meeting moved: arm it unless
                        // its due time has already passed.
                        if existing.is_some() {
                            self.deactivate(&key);
                        }
                        let status = if due >= now {
                            ReminderStatus::Pending
                        } else {
                            skipped(now)
                        };
                        self.ledger.insert(
                            key.clone(),
                            ReminderInstance {
                                key: key.clone(),
                                due_at: due,
                                status,
                            },
                        );
                    }
                }
                sources.insert(key, source.clone());
            }
        }
        self.events = events;

        // Tasks: one reminder per open task, at its due time.
        let tasks = self.tasks.take();
        for task in tasks.iter().flatten() {
            let key = ReminderKey::Task {
                task_id: task.id.clone(),
            };
            if task.done {
                if let Some(inst) = self.ledger.get_mut(&key) {
                    if !matches!(inst.status, ReminderStatus::Handled { .. }) {
                        inst.status = ReminderStatus::Handled {
                            at: now,
                            outcome: ReminderOutcome::MarkedDone,
                        };
                    }
                }
                continue;
            }
            let rearm = match self.ledger.get(&key) {
                None => true,
                // Editing the due time re-arms (contract gap decision 11).
                Some(inst) if inst.due_at != task.due_at => true,
                // Marked undone before it fell due: arm it again. An overdue
                // task marked undone does not re-nag.
                Some(inst) => {
                    matches!(
                        inst.status,
                        ReminderStatus::Handled {
                            outcome: ReminderOutcome::MarkedDone,
                            ..
                        }
                    ) && task.due_at > now
                }
            };
            if rearm {
                self.deactivate(&key);
                self.ledger.insert(
                    key.clone(),
                    ReminderInstance {
                        key: key.clone(),
                        due_at: task.due_at,
                        status: ReminderStatus::Pending,
                    },
                );
            }
            sources.insert(
                key,
                Source {
                    title: task.title.clone(),
                    at: task.due_at,
                    end: None,
                    all_day: false,
                    location: None,
                },
            );
        }

        // Prune: deleted tasks, and meeting reminders that no longer apply
        // (event gone or filtered out, or lead time removed) and would not
        // fire in the future anyway. Each part only once its input is loaded.
        let task_ids: Option<BTreeSet<_>> =
            tasks.as_ref().map(|t| t.iter().map(|t| &t.id).collect());
        let events_loaded = self.events.is_some();
        self.ledger.retain(|key, inst| match key {
            ReminderKey::Task { task_id } => {
                task_ids.as_ref().is_none_or(|ids| ids.contains(task_id))
            }
            ReminderKey::Meeting { .. } => {
                !events_loaded
                    || sources.contains_key(key)
                    || inst.next_fire_at().is_some_and(|t| t >= now)
            }
        });
        self.tasks = tasks;
        self.sources = sources;

        // Whatever no longer applies leaves the screen and the queues.
        let stale: Vec<ReminderKey> = self
            .active_keys()
            .filter(|key| !self.sources.contains_key(key) || !self.ledger.contains_key(key))
            .cloned()
            .collect();
        for key in stale {
            self.deactivate(&key);
        }
    }

    /// Meeting reminders still waiting (queued or held back) when their
    /// meeting has ended are skipped. The bubble on screen is left alone.
    fn drop_ended_meetings(&mut self, now: DateTime<Utc>) {
        let ended: Vec<ReminderKey> = self
            .waiting_keys()
            .filter(|key| self.has_ended(key, now))
            .cloned()
            .collect();
        for key in ended {
            self.deactivate(&key);
            self.set_status(&key, skipped(now));
        }
    }

    /// Reminders whose fire time has come that are not shown, queued or held
    /// back yet, in firing order. Meeting reminders whose meeting has ended
    /// meanwhile (e.g. the computer slept through it) are skipped instead.
    fn newly_due(&mut self, now: DateTime<Utc>) -> Vec<ReminderKey> {
        let mut due: Vec<(DateTime<Utc>, DateTime<Utc>, ReminderKey)> = self
            .ledger
            .values()
            .filter(|inst| !self.is_active(&inst.key))
            .filter_map(|inst| {
                let fire = inst.next_fire_at().filter(|t| *t <= now)?;
                let source = self.sources.get(&inst.key)?;
                Some((fire, source.at, inst.key.clone()))
            })
            .collect();
        due.sort();
        let mut keys = Vec::with_capacity(due.len());
        for (_, _, key) in due {
            if self.has_ended(&key, now) {
                self.set_status(&key, skipped(now));
            } else {
                keys.push(key);
            }
        }
        keys
    }

    /// Adds a newly due reminder to the queue (one bubble each) or the
    /// backlog. At most one reminder per meeting waits at a time: the one
    /// with the smallest lead time wins and the others are skipped, so a
    /// meeting whose 10 and 5 min reminders both fell due (e.g. after
    /// sleep, or while paused) is shown once.
    fn enqueue(&mut self, key: ReminderKey, to_backlog: bool, now: DateTime<Utc>) {
        if let ReminderKey::Meeting {
            event,
            lead_minutes,
        } = &key
        {
            let rivals: Vec<(ReminderKey, u32)> = self
                .waiting_keys()
                .filter_map(|k| match k {
                    ReminderKey::Meeting {
                        event: e,
                        lead_minutes: l,
                    } if e == event => Some((k.clone(), *l)),
                    _ => None,
                })
                .collect();
            for (rival, rival_lead) in rivals {
                if rival_lead < *lead_minutes {
                    self.set_status(&key, skipped(now));
                    return;
                }
                self.deactivate(&rival);
                self.set_status(&rival, skipped(now));
            }
        }
        if to_backlog {
            self.backlog.push(key);
        } else {
            self.queue.push_back(Batch {
                keys: vec![key],
                replay: false,
            });
        }
    }

    /// Turns the backlog into one replay bubble at the front of the queue.
    /// Tasks always replay; meetings only if they have not started yet.
    fn replay(&mut self, now: DateTime<Utc>) {
        let mut keys = std::mem::take(&mut self.backlog);
        let started: Vec<ReminderKey> = keys
            .iter()
            .filter(|key| {
                matches!(key, ReminderKey::Meeting { .. })
                    && self.sources.get(*key).is_some_and(|s| s.at <= now)
            })
            .cloned()
            .collect();
        for key in &started {
            self.set_status(key, skipped(now));
        }
        keys.retain(|key| !started.contains(key));
        keys.sort_by_key(|key| (self.sources.get(key).map(|s| s.at), key.clone()));
        if !keys.is_empty() {
            self.queue.push_front(Batch { keys, replay: true });
        }
    }

    fn diff_view(&mut self) -> Option<BubbleChange> {
        let view = self.build_view();
        let change = match (&self.last_view, &view) {
            (None, None) => None,
            (Some(_), None) => Some(BubbleChange::Cleared),
            (None, Some(v)) => Some(BubbleChange::Shown(v.clone())),
            (Some(old), Some(v)) if old.id != v.id => Some(BubbleChange::Shown(v.clone())),
            (Some(old), Some(v)) if old != v => Some(BubbleChange::Updated(v.clone())),
            _ => None,
        };
        self.last_view = view;
        change
    }

    fn build_view(&self) -> Option<BubbleView> {
        let (id, batch) = self.current.as_ref()?;
        let items: Vec<BubbleItem> = batch
            .keys
            .iter()
            .filter_map(|key| {
                let s = self.sources.get(key)?;
                Some(BubbleItem {
                    key: key.clone(),
                    title: s.title.clone(),
                    at: s.at,
                    all_day: s.all_day,
                    location: s.location.clone(),
                })
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        Some(BubbleView {
            id: *id,
            items,
            queued: u32::try_from(self.queue.len()).unwrap_or(u32::MAX),
            replay: batch.replay,
            snooze_options_minutes: self.reminders.snooze_durations_minutes.clone(),
        })
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn check_current(&self, id: BubbleId) -> Result<()> {
        match &self.current {
            Some((current, _)) if *current == id => Ok(()),
            _ => Err(Error::not_found("no such bubble on screen")),
        }
    }

    fn snooze_keys(&mut self, keys: Vec<ReminderKey>, now: DateTime<Utc>, minutes: u32) {
        let until = now + Duration::minutes(i64::from(minutes));
        for key in keys {
            self.set_status(&key, ReminderStatus::Snoozed { until });
        }
    }

    fn set_status(&mut self, key: &ReminderKey, status: ReminderStatus) {
        if let Some(inst) = self.ledger.get_mut(key) {
            inst.status = status;
        }
    }

    fn has_ended(&self, key: &ReminderKey, now: DateTime<Utc>) -> bool {
        self.sources
            .get(key)
            .and_then(|s| s.end)
            .is_some_and(|end| end <= now)
    }

    /// Keys queued or held back (not the one on screen).
    fn waiting_keys(&self) -> impl Iterator<Item = &ReminderKey> {
        self.queue
            .iter()
            .flat_map(|b| b.keys.iter())
            .chain(self.backlog.iter())
    }

    /// Keys on screen, queued or held back.
    fn active_keys(&self) -> impl Iterator<Item = &ReminderKey> {
        self.current
            .iter()
            .flat_map(|(_, b)| b.keys.iter())
            .chain(self.waiting_keys())
    }

    fn is_active(&self, key: &ReminderKey) -> bool {
        self.active_keys().any(|k| k == key)
    }

    /// Removes `key` from the screen, the queue and the backlog. Its ledger
    /// status is left as it is.
    fn deactivate(&mut self, key: &ReminderKey) {
        if let Some((_, batch)) = &mut self.current {
            batch.keys.retain(|k| k != key);
            if batch.keys.is_empty() {
                self.current = None;
            }
        }
        for batch in &mut self.queue {
            batch.keys.retain(|k| k != key);
        }
        self.queue.retain(|b| !b.keys.is_empty());
        self.backlog.retain(|k| k != key);
    }
}

fn skipped(now: DateTime<Utc>) -> ReminderStatus {
    ReminderStatus::Handled {
        at: now,
        outcome: ReminderOutcome::Skipped,
    }
}

fn check_snooze(minutes: u32) -> Result<()> {
    if ReminderSettings::SNOOZE_RANGE.contains(&minutes) {
        Ok(())
    } else {
        Err(Error::invalid_input(format!(
            "snooze must be between {} and {} minutes",
            ReminderSettings::SNOOZE_RANGE.start(),
            ReminderSettings::SNOOZE_RANGE.end()
        )))
    }
}

/// The next instant after `now` at which quiet hours start or end, in the
/// clock's local time zone. Looks at yesterday, today and tomorrow (local),
/// which always contains the next boundary of a daily window.
fn next_quiet_boundary(
    qh: &QuietHours,
    now: DateTime<Utc>,
    clock: &dyn Clock,
) -> Option<DateTime<Utc>> {
    let today = clock.utc_to_local(now).date();
    [today.pred_opt(), Some(today), today.succ_opt()]
        .into_iter()
        .flatten()
        .flat_map(|day: NaiveDate| [day.and_time(qh.start), day.and_time(qh.end)])
        .map(|local| clock.local_to_utc(local))
        .filter(|t| *t > now)
        .min()
}
