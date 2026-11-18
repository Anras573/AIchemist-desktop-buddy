//! The native right-click menu on the cat (spec 4.3) and the
//! `show_cat_menu` / `open_settings` commands.
//!
//! Menu: Upcoming, Add task, Snooze all, Settings, Quit. The buddy page
//! calls `show_cat_menu` on right-click; the menu pops up at the cursor.
//! Item ids carry a `cat-` prefix: menu events reach every menu handler of
//! the app (including the tray's), so they must not clash with tray ids.

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow};

use crate::domain::ipc::{event, PopoverView};
use crate::BUDDY_WINDOW_LABEL;

const ID_UPCOMING: &str = "cat-upcoming";
const ID_ADD_TASK: &str = "cat-add-task";
const ID_SNOOZE_ALL: &str = "cat-snooze-all";
const ID_SETTINGS: &str = "cat-settings";
const ID_QUIT: &str = "cat-quit";

/// Managed state: the menu, built once.
struct CatMenu<R: Runtime>(Menu<R>);

/// Builds the menu and registers its event handler. Call once during setup.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, ID_UPCOMING, "Upcoming", true, None::<&str>)?,
            &MenuItem::with_id(app, ID_ADD_TASK, "Add task", true, None::<&str>)?,
            &MenuItem::with_id(app, ID_SNOOZE_ALL, "Snooze all", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, ID_SETTINGS, "Settings", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, ID_QUIT, "Quit", true, None::<&str>)?,
        ],
    )?;
    app.manage(CatMenu(menu));
    app.on_menu_event(on_menu_event);
    Ok(())
}

fn open_popover<R: Runtime>(app: &AppHandle<R>, view: PopoverView) {
    let _ = app.emit_to(BUDDY_WINDOW_LABEL, event::OPEN_POPOVER, view);
}

fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        ID_UPCOMING => open_popover(app, PopoverView::Upcoming),
        ID_ADD_TASK => open_popover(app, PopoverView::AddTask),
        ID_SNOOZE_ALL => {
            let app = app.clone();
            // The scheduler call (2.2) must not run on the main thread.
            tauri::async_runtime::spawn_blocking(move || {
                let settings = crate::settings_service::current(&app);
                let minutes = crate::snooze::default_minutes(&settings.reminders);
                if let Err(e) = crate::snooze::snooze_all_for(&app, minutes) {
                    log::warn!("snooze all failed: {e}");
                }
            });
        }
        ID_SETTINGS => crate::tray::open_settings(app),
        ID_QUIT => app.exit(0),
        _ => {}
    }
}

/// IPC `show_cat_menu`: pops the menu up at the cursor. Returns at once; the
/// menu runs from the event loop, not inside the webview's IPC callback
/// (a nested menu loop there can stall WebView2).
#[tauri::command]
pub fn show_cat_menu<R: Runtime>(window: WebviewWindow<R>) -> Result<(), String> {
    if window.label() != BUDDY_WINDOW_LABEL {
        return Err("only the buddy window has a cat menu".into());
    }
    let app = window.app_handle().clone();
    app.clone()
        .run_on_main_thread(move || {
            if let Some(menu) = app.try_state::<CatMenu<R>>() {
                if let Err(e) = window.popup_menu(&menu.0) {
                    log::warn!("showing the cat menu failed: {e}");
                }
            }
        })
        .map_err(|e| e.to_string())
}

/// IPC `open_settings`: opens (or focuses) the settings window. Async:
/// creating a window from a sync command can deadlock on Windows.
#[tauri::command]
pub async fn open_settings<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    crate::tray::open_settings(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::domain::ipc::{command, Window, COMMANDS};

    #[test]
    fn buddy_capability_grants_the_popover_commands_and_no_settings_ones() {
        let cap: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/buddy.json")).unwrap();
        let granted: Vec<&str> = cap["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p.as_str())
            .collect();
        let perm = |name: &str| format!("allow-{}", name.replace('_', "-"));
        for name in [
            command::LIST_UPCOMING,
            command::LIST_TASKS,
            command::CREATE_TASK,
            command::UPDATE_TASK,
            command::SET_TASK_DONE,
            command::DELETE_TASK,
            command::SNOOZE_ALL,
            command::SHOW_CAT_MENU,
            command::OPEN_SETTINGS,
        ] {
            let spec = COMMANDS.iter().find(|c| c.name == name).unwrap();
            assert_eq!(spec.windows, &[Window::Buddy]);
            assert!(granted.contains(&perm(name).as_str()), "{name} not granted");
        }
        for spec in COMMANDS
            .iter()
            .filter(|c| !c.windows.contains(&Window::Buddy))
        {
            assert!(
                !granted.contains(&perm(spec.name).as_str()),
                "buddy must not get {}",
                spec.name
            );
        }
    }
}
