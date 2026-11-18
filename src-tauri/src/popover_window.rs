//! Window plumbing for the upcoming popover (task 2.3).
//!
//! The popover ([`crate::placement::POPOVER_WIDTH`] x
//! [`crate::placement::POPOVER_HEIGHT`]) does not fit the 240 x 120 bubble
//! area, so the buddy page asks for a larger window while it is open and
//! for the normal size again when it closes ([`set_popover_open`]). The
//! window grows away from its corner, so the cat keeps its place on screen
//! ([`crate::placement::place_grown`]). The flag is read by
//! [`crate::buddy_window::reposition`], so display changes while the
//! popover is open keep the grown size.
//!
//! Like `set_hit_regions`, this is buddy-window plumbing rather than part
//! of the IPC contract in `domain::ipc`.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

use crate::BUDDY_WINDOW_LABEL;

/// Managed state: whether the popover is open.
#[derive(Default)]
struct PopoverOpen(AtomicBool);

/// Registers the state. Call once during setup.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    app.manage(PopoverOpen::default());
}

/// Whether the popover is open (the window is grown).
pub fn is_open<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<PopoverOpen>()
        .is_some_and(|s| s.0.load(Ordering::SeqCst))
}

/// Request for [`set_popover_open`].
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPopoverOpenRequest {
    /// Open (grow the window) or closed (restore it).
    pub open: bool,
}

/// IPC: the popover opened or closed; resizes and re-places the buddy
/// window. Resolves once the new frame is applied, so the page can lay out
/// for it.
#[tauri::command]
pub async fn set_popover_open<R: Runtime>(
    window: WebviewWindow<R>,
    request: SetPopoverOpenRequest,
) -> Result<(), String> {
    if window.label() != BUDDY_WINDOW_LABEL {
        return Err("only the buddy window has a popover".into());
    }
    let Some(state) = window.try_state::<PopoverOpen>() else {
        return Err("popover state is not initialised".into());
    };
    if state.0.swap(request.open, Ordering::SeqCst) == request.open {
        return Ok(());
    }
    crate::buddy_window::reposition(&window).map_err(|e| e.to_string())
}
