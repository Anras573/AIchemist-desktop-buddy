//! The "Pause reminders" flag (spec 4.2, 7.5).
//!
//! In memory only (a restart resumes reminders). [`set_paused`] forwards to
//! the scheduler (`SchedulerDriver::set_paused`, through
//! [`crate::reminders::scheduler_set_paused`]); the `get_paused` /
//! `set_paused` commands in [`crate::reminders`] use [`is_paused`] and
//! [`set_paused`].

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::domain::ipc::event;
use crate::BUDDY_WINDOW_LABEL;

/// Managed state: whether reminders are paused.
#[derive(Default)]
pub struct Paused(AtomicBool);

/// Registers the state. Call once during setup, before [`set_paused`].
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    app.manage(Paused::default());
}

/// Whether reminders are paused.
pub fn is_paused<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<Paused>()
        .is_some_and(|p| p.0.load(Ordering::SeqCst))
}

/// Sets the flag. If it changed: updates the tray check item and emits
/// `paused-changed` to the buddy window. Returns whether it changed.
/// Single entry point for the tray, the cat menu and the `set_paused`
/// command, so all of them stay in sync.
pub fn set_paused<R: Runtime>(app: &AppHandle<R>, paused: bool) -> bool {
    let Some(state) = app.try_state::<Paused>() else {
        return false;
    };
    if state.0.swap(paused, Ordering::SeqCst) == paused {
        // Still re-sync the tray: a check item toggles itself when clicked.
        crate::tray::sync_paused(app, paused);
        return false;
    }
    crate::tray::sync_paused(app, paused);
    // Pausing clears the bubble; resuming replays what fell due (spec 7.5).
    crate::reminders::scheduler_set_paused(app, paused);
    let _ = app.emit_to(BUDDY_WINDOW_LABEL, event::PAUSED_CHANGED, paused);
    true
}
