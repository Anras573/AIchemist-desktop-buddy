//! The reminder service without Tauri: the scheduler driver, its sink and
//! the ledger writer, reporting through an [`Outlet`] so tests can use a
//! fake.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Duration;

use super::ledger_writer::LedgerWriter;
use crate::domain::ipc::BubbleView;
use crate::domain::{BubbleId, BuddyEvent, Clock, Event, ReminderInstance, Result, Settings};
use crate::scheduler::{BubbleChange, Scheduler, SchedulerDriver, SchedulerSink};
use crate::storage::Storage;

/// How far ahead cached events are loaded into the scheduler at startup.
/// The sync engine (3.6) replaces them through
/// [`Reminders::events_changed`] after each sync.
pub const EVENT_WINDOW: Duration = Duration::days(7);

/// Where the reminder service reports to: the buddy state machine and the
/// buddy window. Called from the scheduler sink, with the scheduler's lock
/// held, so implementations must return quickly and must not call back into
/// [`Reminders`].
pub trait Outlet: Send + Sync {
    /// Feed an event to the buddy state machine.
    fn buddy_event(&self, event: BuddyEvent);
    /// Emit `reminder-shown` with `view`.
    fn bubble_shown(&self, view: &BubbleView);
    /// Emit `reminder-cleared`.
    fn bubble_cleared(&self);
    /// Play the reminder chime once.
    fn play_chime(&self);
}

/// Flags shared between [`Reminders`] and its sink.
#[derive(Default)]
struct Flags {
    sound_enabled: AtomicBool,
    /// Hook for the sync engine (3.6); see [`Reminders::set_sync_in_progress`].
    sync_in_progress: AtomicBool,
    /// Why the bubble is about to be cleared, set around a user action.
    clear_reason: Mutex<Option<BuddyEvent>>,
}

impl Flags {
    fn set_clear_reason(&self, reason: Option<BuddyEvent>) {
        *self.clear_reason.lock().unwrap_or_else(|e| e.into_inner()) = reason;
    }

    fn clear_reason(&self) -> Option<BuddyEvent> {
        *self.clear_reason.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The [`SchedulerSink`]: maps bubble changes to outlet calls and hands the
/// ledger to the writer thread. Never blocks and never calls the driver.
struct Sink {
    outlet: Arc<dyn Outlet>,
    flags: Arc<Flags>,
    writer: Arc<LedgerWriter>,
}

impl SchedulerSink for Sink {
    fn bubble_changed(&mut self, change: &BubbleChange) {
        match change {
            BubbleChange::Shown(view) => {
                self.outlet.buddy_event(BuddyEvent::ReminderDue);
                self.outlet.bubble_shown(view);
                if self.flags.sound_enabled.load(Ordering::SeqCst) {
                    self.outlet.play_chime();
                }
            }
            BubbleChange::Updated(view) => self.outlet.bubble_shown(view),
            BubbleChange::Cleared => {
                self.outlet.bubble_cleared();
                // Leaving `alert` happens here, not in the commands: when a
                // dismissed bubble had another queued behind it the scheduler
                // shows that one instead (`Shown`, the cat stays alert), and
                // doing it under the scheduler lock keeps the order right if
                // a reminder fires at the same moment. A bubble cleared
                // without a user action (paused, task marked done in the
                // popover, meeting removed by a sync) counts as dismissed.
                let reason = self
                    .flags
                    .clear_reason()
                    .unwrap_or(BuddyEvent::BubbleDismissed);
                self.outlet.buddy_event(reason);
                // Leaving `alert` always lands in `idle` (state_machine.rs).
                // If a sync is still running, put the cat back in `thinking`.
                if self.flags.sync_in_progress.load(Ordering::SeqCst) {
                    self.outlet.buddy_event(BuddyEvent::SyncStarted);
                }
            }
        }
    }

    fn ledger_changed(&mut self, ledger: &[ReminderInstance]) {
        self.writer.submit(ledger.to_vec());
    }
}

/// The running reminder service: one [`SchedulerDriver`] fed from storage
/// and settings, plus the ledger writer.
pub struct Reminders {
    driver: SchedulerDriver,
    storage: Storage,
    flags: Arc<Flags>,
    writer: Arc<LedgerWriter>,
}

impl Reminders {
    /// Builds the scheduler from the persisted ledger, starts the driver and
    /// feeds it the cached events of the next [`EVENT_WINDOW`], the tasks
    /// and the pause state. Reminders that are already due fire during this
    /// call (through `outlet`).
    pub fn start(
        storage: Storage,
        settings: &Settings,
        clock: Arc<dyn Clock>,
        paused: bool,
        outlet: Arc<dyn Outlet>,
    ) -> io::Result<Self> {
        let ledger = storage.load_reminders().unwrap_or_else(|e| {
            log::error!("cannot load the reminder ledger: {e}");
            Vec::new()
        });
        let core = Scheduler::new(ledger, settings, &*clock);
        let writer = Arc::new(LedgerWriter::spawn({
            let storage = storage.clone();
            move |ledger| {
                if let Err(e) = storage.save_reminders(ledger) {
                    log::error!("cannot save the reminder ledger: {e}");
                }
            }
        })?);
        let flags = Arc::new(Flags::default());
        flags
            .sound_enabled
            .store(settings.reminders.sound_enabled, Ordering::SeqCst);
        let sink = Sink {
            outlet,
            flags: Arc::clone(&flags),
            writer: Arc::clone(&writer),
        };
        let now = clock.now();
        let driver = SchedulerDriver::spawn(core, clock, Box::new(sink))?;
        let reminders = Self {
            driver,
            storage,
            flags,
            writer,
        };
        if paused {
            reminders.driver.set_paused(true);
        }
        match reminders.storage.events_in_window(now, now + EVENT_WINDOW) {
            Ok(events) => reminders.driver.set_events(events),
            Err(e) => log::error!("cannot load cached events: {e}"),
        }
        if let Err(e) = reminders.tasks_changed() {
            log::error!("cannot load tasks: {e}");
        }
        Ok(reminders)
    }

    /// The bubble on screen, if any.
    pub fn current_bubble(&self) -> Option<BubbleView> {
        self.driver.current_bubble()
    }

    /// Dismisses bubble `id` (a stale id fails with `notFound`).
    pub fn dismiss(&self, id: BubbleId) -> Result<()> {
        self.user_action(BuddyEvent::BubbleDismissed, || self.driver.dismiss(id))
    }

    /// Snoozes bubble `id` for `minutes`.
    pub fn snooze(&self, id: BubbleId, minutes: u32) -> Result<()> {
        self.user_action(BuddyEvent::BubbleSnoozed, || {
            self.driver.snooze(id, minutes)
        })
    }

    /// Snoozes the bubble, the queue and anything held back.
    pub fn snooze_all(&self, minutes: u32) -> Result<()> {
        self.user_action(BuddyEvent::BubbleSnoozed, || {
            self.driver.snooze_all(minutes)
        })
    }

    /// Pauses or resumes reminders in the scheduler.
    pub fn set_paused(&self, paused: bool) {
        self.driver.set_paused(paused);
    }

    /// Whether the scheduler is paused.
    pub fn is_paused(&self) -> bool {
        self.driver.is_paused()
    }

    /// Applies new settings (lead times, snooze options, filters, quiet
    /// hours, sound).
    pub fn set_settings(&self, settings: &Settings) {
        self.flags
            .sound_enabled
            .store(settings.reminders.sound_enabled, Ordering::SeqCst);
        self.driver.set_settings(settings);
    }

    /// Reloads every task from storage into the scheduler. Call after any
    /// task change (create, edit, done/undone, delete). Blocks on storage.
    pub fn tasks_changed(&self) -> Result<()> {
        let tasks = self.storage.list_tasks()?;
        self.driver.set_tasks(tasks);
        Ok(())
    }

    /// Replaces the calendar events the scheduler sees (the whole cache).
    pub fn events_changed(&self, events: Vec<Event>) {
        self.driver.set_events(events);
    }

    /// Hook for the sync engine (3.6): set while a sync runs. When a bubble
    /// closes during a sync, the cat goes back to `thinking` instead of
    /// staying `idle`. The sync engine still dispatches `SyncStarted` and
    /// `SyncEnded` itself.
    pub fn set_sync_in_progress(&self, in_progress: bool) {
        self.flags
            .sync_in_progress
            .store(in_progress, Ordering::SeqCst);
    }

    /// Re-evaluates now (e.g. after resume from sleep).
    pub fn nudge(&self) {
        self.driver.nudge();
    }

    /// Blocks until the ledger writes queued so far are on disk.
    pub fn flush(&self) {
        self.writer.flush();
    }

    /// Stops the scheduler and saves the last ledger. Idempotent.
    pub fn shutdown(&self) {
        self.driver.shutdown();
        self.writer.shutdown();
    }

    fn user_action(&self, reason: BuddyEvent, op: impl FnOnce() -> Result<()>) -> Result<()> {
        self.flags.set_clear_reason(Some(reason));
        let result = op();
        self.flags.set_clear_reason(None);
        result
    }
}

impl Drop for Reminders {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// Builds a sink around a fake outlet and a writer, for mapping tests.
    pub fn sink(
        outlet: Arc<dyn Outlet>,
        writer: Arc<LedgerWriter>,
        sound: bool,
        sync_in_progress: bool,
        clear_reason: Option<BuddyEvent>,
    ) -> Box<dyn SchedulerSink> {
        let flags = Arc::new(Flags::default());
        flags.sound_enabled.store(sound, Ordering::SeqCst);
        flags
            .sync_in_progress
            .store(sync_in_progress, Ordering::SeqCst);
        flags.set_clear_reason(clear_reason);
        Box::new(Sink {
            outlet,
            flags,
            writer,
        })
    }
}
