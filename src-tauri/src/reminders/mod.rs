//! Reminder wiring (task 2.2): the scheduler driven by storage and
//! settings, the speech bubble events, the chime and the bubble commands.
//!
//! - [`init`] (at startup) builds the [`Scheduler`](crate::scheduler::Scheduler)
//!   from the persisted ledger, feeds it the cached events, the tasks, the
//!   settings and the pause state, and starts the
//!   [`SchedulerDriver`](crate::scheduler::SchedulerDriver).
//! - The sink maps `BubbleChange::Shown` to `ReminderDue` (cat to `alert`),
//!   `reminder-shown` and the chime (if sound is on); `Updated` to
//!   `reminder-shown` only; `Cleared` to `reminder-cleared` and the cat
//!   leaving `alert`. Ledger changes are saved by a separate worker thread
//!   ([`ledger_writer`]), coalescing bursts.
//! - Commands for the buddy window: `get_current_bubble`, `dismiss_bubble`,
//!   `snooze_bubble`, `get_paused` and `set_paused`. `snooze_all` (command
//!   and cat menu) lives in [`crate::snooze`] and calls [`snooze_all`]; the
//!   bubble's "Mark done" is `set_task_done` in [`crate::upcoming`].
//!
//! # Hooks for other tasks
//!
//! - **Task changes:** every task mutation goes through
//!   `upcoming::tasks_changed`, which calls [`tasks_changed`] (the scheduler
//!   reloads all tasks from storage, so a done task leaves the bubble and a
//!   new or edited due time re-arms its reminder).
//! - **Sync (3.6):** after a sync, call [`events_changed`] with the cached
//!   events. While a sync runs, call [`set_sync_in_progress`] so a bubble
//!   closed mid-sync puts the cat back in `thinking` (see
//!   `state_machine::next_state`).
//! - **Pause:** [`crate::pause::set_paused`] forwards to the scheduler
//!   through [`scheduler_set_paused`].
//!
//! # Chime
//!
//! The chime plays in the buddy webview. The contract has no event for it,
//! and the webview cannot read the settings, so Rust asks the page to play
//! it by evaluating [`CHIME_SCRIPT`] (only for `Shown`, only with sound on).
//! `eval` is host-side script injection and needs no capability.

pub mod ledger_writer;
mod service;

#[cfg(test)]
mod tests;

pub use service::{Outlet, Reminders, EVENT_WINDOW};

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use crate::domain::ipc::{event, BubbleRequest, BubbleView, SetPausedRequest, SnoozeBubbleRequest};
use crate::domain::{BuddyEvent, Clock, Error, Event, Result, SystemClock};
use crate::BUDDY_WINDOW_LABEL;

/// Script the buddy page runs to play the chime (see `src/buddy/chime.ts`).
pub const CHIME_SCRIPT: &str = "window.__aichemistPlayChime?.()";

/// The real [`Outlet`]: buddy state machine plus events to the buddy window.
struct TauriOutlet<R: Runtime>(AppHandle<R>);

impl<R: Runtime> Outlet for TauriOutlet<R> {
    fn buddy_event(&self, event: BuddyEvent) {
        crate::buddy_state::dispatch(&self.0, event);
    }

    fn bubble_shown(&self, view: &BubbleView) {
        let _ = self
            .0
            .emit_to(BUDDY_WINDOW_LABEL, event::REMINDER_SHOWN, view);
    }

    fn bubble_cleared(&self) {
        let _ = self
            .0
            .emit_to(BUDDY_WINDOW_LABEL, event::REMINDER_CLEARED, ());
    }

    fn play_chime(&self) {
        if let Some(window) = self.0.get_webview_window(BUDDY_WINDOW_LABEL) {
            let _ = window.eval(CHIME_SCRIPT);
        }
    }
}

/// Starts the reminder service and puts it in managed state. Call once
/// during setup, after `pause::init`, `app_storage::init`,
/// `buddy_state::init` and `settings_service::init`. A failure (no
/// database, no thread) is logged; the commands then fail with `internal`.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let storage = match crate::app_storage::storage(app) {
        Ok(storage) => storage,
        Err(e) => {
            log::error!("reminders are off: {e}");
            return;
        }
    };
    let settings = crate::settings_service::current(app);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let outlet: Arc<dyn Outlet> = Arc::new(TauriOutlet(app.clone()));
    let paused = crate::pause::is_paused(app);
    match Reminders::start(storage, &settings, clock, paused, outlet) {
        Ok(reminders) => {
            app.manage(reminders);
        }
        Err(e) => {
            log::error!("cannot start the reminder scheduler: {e}");
            return;
        }
    }
    let handle = app.clone();
    crate::settings_service::on_settings_changed(app, move |_old, new| {
        if let Some(r) = handle.try_state::<Reminders>() {
            r.set_settings(new);
        }
    });
}

/// Stops the scheduler and writes the last ledger to disk. Call on exit.
pub fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(r) = app.try_state::<Reminders>() {
        r.shutdown();
    }
}

/// Forwards the pause flag to the scheduler (called by
/// [`crate::pause::set_paused`]).
pub fn scheduler_set_paused<R: Runtime>(app: &AppHandle<R>, paused: bool) {
    if let Some(r) = app.try_state::<Reminders>() {
        r.set_paused(paused);
    }
}

/// Reloads every task from storage into the scheduler. Call after any task
/// mutation. Blocks briefly on storage: not on the main thread.
pub fn tasks_changed<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    reminders(app)?.tasks_changed()
}

/// Snoozes the bubble, the queue and anything held back by pause for
/// `minutes` (for `snooze::snooze_all_for`). The cat leaves `alert`.
pub fn snooze_all<R: Runtime>(app: &AppHandle<R>, minutes: u32) -> Result<()> {
    reminders(app)?.snooze_all(minutes)
}

/// Hands the scheduler the current calendar events (the whole cache), e.g.
/// after a sync (3.6).
pub fn events_changed<R: Runtime>(app: &AppHandle<R>, events: Vec<Event>) {
    if let Some(r) = app.try_state::<Reminders>() {
        r.events_changed(events);
    }
}

/// Tells the reminder service whether a sync is running (3.6), so a bubble
/// closed during a sync returns the cat to `thinking`.
pub fn set_sync_in_progress<R: Runtime>(app: &AppHandle<R>, in_progress: bool) {
    if let Some(r) = app.try_state::<Reminders>() {
        r.set_sync_in_progress(in_progress);
    }
}

fn reminders<R: Runtime>(app: &AppHandle<R>) -> Result<State<'_, Reminders>> {
    app.try_state::<Reminders>()
        .ok_or_else(|| Error::internal("the reminder scheduler is not running"))
}

/// IPC `get_current_bubble` (buddy window).
#[tauri::command]
pub async fn get_current_bubble(app: AppHandle) -> Result<Option<BubbleView>> {
    Ok(reminders(&app)?.current_bubble())
}

/// IPC `dismiss_bubble` (buddy window).
#[tauri::command]
pub async fn dismiss_bubble(app: AppHandle, request: BubbleRequest) -> Result<()> {
    reminders(&app)?.dismiss(request.bubble_id)
}

/// IPC `snooze_bubble` (buddy window).
#[tauri::command]
pub async fn snooze_bubble(app: AppHandle, request: SnoozeBubbleRequest) -> Result<()> {
    reminders(&app)?.snooze(request.bubble_id, request.minutes)
}

/// IPC `get_paused` (buddy window).
#[tauri::command]
pub fn get_paused(app: AppHandle) -> bool {
    crate::pause::is_paused(&app)
}

/// IPC `set_paused` (buddy window). Same path as the tray toggle. Returns
/// the new value.
#[tauri::command]
pub async fn set_paused(app: AppHandle, request: SetPausedRequest) -> bool {
    crate::pause::set_paused(&app, request.paused);
    crate::pause::is_paused(&app)
}
