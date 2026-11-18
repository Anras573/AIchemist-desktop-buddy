//! AIchemist Desktop Buddy core.
//!
//! [`domain`] holds the frozen shared contracts (types, traits, IPC contract).
//! The scheduler, providers, storage and state machine are added by later
//! tasks and build against it.

pub mod caldav;
#[deny(missing_docs)]
pub mod domain;
pub mod graph;
pub mod ical;
pub mod scheduler;
pub mod secrets;
pub mod state_machine;
pub mod storage;
pub mod sync;

use tauri::Manager;

pub mod accounts;
pub mod already_running;
pub mod app_storage;
pub mod buddy_state;
pub mod buddy_window;
pub mod cat_menu;
pub mod click_through;
pub mod logger;
pub mod pause;
pub mod placement;
pub mod popover_window;
pub mod reminders;
pub mod settings_service;
pub mod settings_window;
pub mod snooze;
pub mod tray;
pub mod upcoming;

/// Label of the buddy window declared in `tauri.conf.json`.
pub const BUDDY_WINDOW_LABEL: &str = "buddy";

/// Builds and runs the Tauri application. Blocks until the app exits.
pub fn run() {
    tauri::Builder::default()
        // Must be the first plugin: a second launch exits here, before it
        // creates any window (spec 4: one buddy).
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // The second process exits; tell the user where the buddy is.
            already_running::notify(app);
        }))
        .invoke_handler(tauri::generate_handler![
            click_through::set_hit_regions,
            buddy_state::get_buddy_state,
            buddy_state::report_interaction,
            buddy_state::get_appearance,
            settings_service::get_settings,
            settings_service::update_settings,
            settings_window::list_monitors,
            upcoming::list_upcoming,
            upcoming::list_tasks,
            upcoming::create_task,
            upcoming::update_task,
            upcoming::set_task_done,
            upcoming::delete_task,
            snooze::snooze_all,
            cat_menu::show_cat_menu,
            cat_menu::open_settings,
            popover_window::set_popover_open,
            accounts::list_accounts,
            accounts::add_account,
            accounts::update_account,
            accounts::remove_account,
            accounts::list_account_calendars,
            accounts::test_connection,
            accounts::sign_in_account,
            accounts::sync_now,
            reminders::get_current_bubble,
            reminders::dismiss_bubble,
            reminders::snooze_bubble,
            reminders::get_paused,
            reminders::set_paused,
        ])
        .setup(|app| {
            if let Ok(dir) = app.path().app_log_dir() {
                logger::init(&dir);
            }
            pause::init(app.handle());
            app_storage::init(app.handle());
            popover_window::init(app.handle());
            // Before the settings service, which applies the idle timeout.
            buddy_state::init(app.handle());
            let settings = settings_service::init(app.handle());
            accounts::init(app.handle());
            // After pause, app_storage, buddy_state and settings: it reads
            // all four.
            reminders::init(app.handle());
            if let Some(buddy) = app.get_webview_window(BUDDY_WINDOW_LABEL) {
                click_through::init(app.handle(), &buddy)?;
                buddy_window::init(app.handle(), &buddy, settings.appearance)?;
            }
            tray::init(app.handle())?;
            cat_menu::init(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Save the last reminder ledger before the process ends.
                reminders::shutdown(app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::BUDDY_WINDOW_LABEL;

    #[test]
    fn config_declares_only_the_buddy_window() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json is valid JSON");
        let windows = conf["app"]["windows"]
            .as_array()
            .expect("app.windows is an array");

        assert_eq!(
            windows.len(),
            1,
            "only the buddy window may exist at startup"
        );
        assert_eq!(windows[0]["label"], BUDDY_WINDOW_LABEL);
    }

    #[test]
    fn config_buddy_window_matches_the_default_placement() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json is valid JSON");
        let buddy = &conf["app"]["windows"][0];
        let (w, h) = crate::placement::window_size_css(
            crate::domain::AppearanceSettings::default().cat_scale,
        );

        // Same size as placement computes, so nothing resizes on start.
        assert_eq!(buddy["width"], w);
        assert_eq!(buddy["height"], h);
        // Hidden until placed, and showing it must not steal focus.
        assert_eq!(buddy["visible"], false);
        assert_eq!(buddy["focus"], false);
        // Tuned for memory (docs/perf.md); must not change by accident.
        assert_eq!(
            buddy["additionalBrowserArgs"],
            concat!(
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection ",
                "--autoplay-policy=no-user-gesture-required --disable-gpu ",
                "--disable-software-rasterizer --in-process-gpu ",
                "--enable-features=NetworkServiceInProcess,NetworkServiceInProcess2",
            )
        );
    }

    #[test]
    fn settings_commands_are_granted_only_to_the_settings_window() {
        let buddy: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/buddy.json")).unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/settings.json")).unwrap();

        assert_eq!(settings["windows"], serde_json::json!(["settings"]));
        assert_eq!(
            settings["permissions"],
            serde_json::json!([
                "allow-get-settings",
                "allow-update-settings",
                "allow-list-monitors",
                "allow-list-accounts",
                "allow-add-account",
                "allow-update-account",
                "allow-remove-account",
                "allow-list-account-calendars",
                "allow-test-connection",
                "allow-sign-in-account",
                "allow-sync-now",
                "core:event:allow-listen"
            ])
        );
        for perm in buddy["permissions"].as_array().unwrap() {
            let perm = perm.as_str().unwrap();
            if perm == "allow-open-settings" {
                // The buddy's cat menu opens the window (domain::ipc).
                continue;
            }
            assert!(
                !perm.contains("settings")
                    && !perm.contains("monitors")
                    && !perm.contains("account")
                    && !perm.contains("sync")
                    && !perm.contains("connection"),
                "buddy must not get {perm}"
            );
        }
    }

    #[test]
    fn config_sets_a_content_security_policy() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json is valid JSON");
        let csp = conf["app"]["security"]["csp"]
            .as_str()
            .expect("app.security.csp is set");

        assert!(csp.contains("default-src 'self'"));
    }
}
