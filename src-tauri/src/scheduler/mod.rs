//! The reminder scheduler (spec 7.4): one component that owns all reminder
//! timing.
//!
//! - [`Scheduler`] (in `engine`) is the pure, deterministic core. Feed it
//!   events, tasks, settings, pause state and bubble actions; each call takes
//!   a [`Clock`](crate::domain::Clock) and returns an [`Update`] (bubble to
//!   show, update or clear; whether the ledger must be persisted).
//!   [`Scheduler::next_wake`] is the single instant at which anything next
//!   becomes due.
//! - [`SchedulerDriver`] (in `driver`) runs the core on one thread with one
//!   timer and reports through a [`SchedulerSink`], so it does not depend on
//!   Tauri.
//!
//! The ledger goes in and out as plain [`ReminderInstance`](crate::domain::ReminderInstance)
//! values ([`Scheduler::new`], [`Scheduler::ledger`]); storage is wired up
//! elsewhere.
//!
//! # Rules
//!
//! **Meeting reminders.** One reminder per (event occurrence, configured lead
//! time), due at `start - lead`. All-day and declined events are skipped when
//! the corresponding setting is on. Each fires at most once unless snoozed.
//!
//! - *First sight:* when a (event, lead) pair first appears (new event in
//!   the cache, or a lead time added in settings) and its due time has
//!   already passed, it is marked `Skipped` and never fires. Only lead times
//!   still ahead fire. A meeting added 3 min before its start therefore gets
//!   only its 1 min reminder; one added 30 s before its start gets none.
//! - *Launch:* in [`Scheduler::new`], persisted meeting reminders whose fire
//!   time passed while the app was not running are marked `Skipped`.
//! - *Moved meeting:* if an event's start changes, its reminders are re-armed
//!   at the new times (with the first-sight rule).
//! - *One per meeting at a time:* if several lead times of the same meeting
//!   are waiting at once (after sleep, while paused, or a snoozed 10 min
//!   reminder re-firing together with the 5 min one), only the smallest lead
//!   is kept and the rest are `Skipped`.
//! - *Ended meetings:* a meeting reminder that would fire, or is still
//!   queued, after its meeting has ended is `Skipped`. A meeting in progress
//!   still fires (e.g. on wake from sleep).
//! - A meeting that disappears from the cache, becomes declined, or loses its
//!   lead time leaves the screen and queue; past entries for it are pruned.
//!
//! **Task reminders.** One per open task, at `due_at`.
//!
//! - An overdue task that has not fired yet (created overdue, or due while
//!   the app was closed) fires once immediately; after that it never
//!   re-nags.
//! - Done tasks never fire; a task marked done while pending or on screen is
//!   handled with outcome `MarkedDone` and leaves the screen.
//! - Editing `due_at` re-arms the reminder (contract gap decision 11).
//! - Marking a task undone re-arms it only if its due time is still ahead.
//! - Deleted tasks are pruned from the ledger.
//!
//! **Bubbles.** One bubble at a time. Reminders falling due together are
//! queued one per bubble, in fire-time order; `queued` is the "+N" badge.
//! Dismiss handles every item in the bubble (`Dismissed`); snooze re-fires
//! each item after the delay; snooze all covers the bubble, the queue and
//! anything held back by pause or quiet hours. Bubble actions name the
//! bubble id, so a stale click fails with `notFound`.
//!
//! **Pause and quiet hours** (treated the same). While suppressed nothing is
//! shown: the bubble on screen and the queue are taken down and held back,
//! and reminders that fall due are collected. When suppression ends they are
//! replayed as one bubble with `replay: true`: tasks always, meetings only if
//! they have not started yet (the rest are `Skipped`). Quiet hours are
//! evaluated in local time through the clock and may wrap midnight; their
//! start and end are wake-up points. A wake from sleep that skipped the whole
//! quiet window fires normally.

mod driver;
mod engine;

pub use driver::{SchedulerDriver, SchedulerSink};
pub use engine::{BubbleChange, Scheduler, Update};

#[cfg(test)]
mod tests;
