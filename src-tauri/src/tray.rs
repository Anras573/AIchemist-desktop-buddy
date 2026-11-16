//! Tray (Windows) / menu bar (macOS) icon and its menu (spec 4.2).
//!
//! Menu: Open Settings, Upcoming, Pause reminders (check item), Quit.
//! Everything is handled in Rust; the webviews need no permission for it.
//!
//! Hooks for later tasks:
//! - [`open_settings`]: stub until task 2.4 creates the settings window.
//! - [`set_error`]: error indicator (task 3.5), shown as a disabled first
//!   menu item and in the tooltip.
//! - [`sync_paused`]: keeps the check mark in sync; called by
//!   [`crate::pause::set_paused`].

use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::domain::ipc::{event, PopoverView};
use crate::BUDDY_WINDOW_LABEL;

/// Tray icon id.
pub const TRAY_ID: &str = "main";
/// Tooltip without an error.
pub const TOOLTIP: &str = "AIchemist Buddy";

const ID_OPEN_SETTINGS: &str = "open-settings";
const ID_UPCOMING: &str = "upcoming";
const ID_PAUSE: &str = "pause";
const ID_QUIT: &str = "quit";
const ID_STATUS: &str = "status";

/// Managed state: the menu items that change at runtime.
struct TrayState<R: Runtime> {
    tray: TrayIcon<R>,
    menu: Menu<R>,
    pause: CheckMenuItem<R>,
    status: MenuItem<R>,
    status_separator: PredefinedMenuItem<R>,
    /// Whether the status item and its separator are in the menu.
    error_shown: Mutex<bool>,
}

/// Creates the tray icon and menu. Call once during setup.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let pause = CheckMenuItem::with_id(
        app,
        ID_PAUSE,
        "Pause reminders",
        true,
        crate::pause::is_paused(app),
        None::<&str>,
    )?;
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, ID_OPEN_SETTINGS, "Open Settings", true, None::<&str>)?,
            &MenuItem::with_id(app, ID_UPCOMING, "Upcoming", true, None::<&str>)?,
            &pause,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, ID_QUIT, "Quit", true, None::<&str>)?,
        ],
    )?;
    // Not in the menu until `set_error` shows it.
    let status = MenuItem::with_id(app, ID_STATUS, "", false, None::<&str>)?;
    let status_separator = PredefinedMenuItem::separator(app)?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(true)
        // A template image is drawn by macOS in the menu bar's colours (the
        // cat-head silhouette); ignored elsewhere.
        .icon_as_template(true)
        .on_menu_event(on_menu_event);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;

    app.manage(TrayState {
        tray,
        menu,
        pause,
        status,
        status_separator,
        error_shown: Mutex::new(false),
    });
    Ok(())
}

fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        ID_OPEN_SETTINGS => open_settings(app),
        ID_UPCOMING => {
            let _ = app.emit_to(
                BUDDY_WINDOW_LABEL,
                event::OPEN_POPOVER,
                PopoverView::Upcoming,
            );
        }
        ID_PAUSE => {
            crate::pause::set_paused(app, !crate::pause::is_paused(app));
        }
        ID_QUIT => app.exit(0),
        _ => {}
    }
}

/// Opens the settings window. Stub: task 2.4 creates the window on demand
/// (and destroys it on close, spec 4.5).
pub fn open_settings<R: Runtime>(_app: &AppHandle<R>) {
    eprintln!("open_settings: the settings window is not implemented yet (task 2.4)");
}

/// Sets the check mark of "Pause reminders".
pub fn sync_paused<R: Runtime>(app: &AppHandle<R>, paused: bool) {
    if let Some(state) = app.try_state::<TrayState<R>>() {
        let _ = state.pause.set_checked(paused);
    }
}

/// Shows (`Some`) or clears (`None`) the error indicator: a disabled first
/// menu item with the text, and a second tooltip line.
///
/// `message` must be short and must not contain secrets or event/task
/// contents (spec 9), e.g. "Sync failed for Work (network)".
pub fn set_error<R: Runtime>(app: &AppHandle<R>, message: Option<&str>) {
    let Some(state) = app.try_state::<TrayState<R>>() else {
        return;
    };
    let mut shown = state.error_shown.lock().unwrap_or_else(|e| e.into_inner());
    match message {
        Some(text) => {
            let _ = state.status.set_text(text);
            let _ = state.tray.set_tooltip(Some(format!("{TOOLTIP}\n{text}")));
            if !*shown {
                let _ = state
                    .menu
                    .insert_items(&[&state.status, &state.status_separator], 0);
                *shown = true;
            }
        }
        None => {
            let _ = state.tray.set_tooltip(Some(TOOLTIP));
            if *shown {
                let _ = state.menu.remove(&state.status);
                let _ = state.menu.remove(&state.status_separator);
                *shown = false;
            }
        }
    }
}
