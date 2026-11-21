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
pub mod calendar_sync;
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
pub mod system;
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
            upcoming::create_task,
            upcoming::set_task_done,
            upcoming::delete_task,
            cat_menu::show_cat_menu,
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
            // After the settings service and the tray (task 4.1).
            system::init(app.handle());
            // Last: the first sync starts at once and reports to the
            // accounts, the scheduler and the tray.
            calendar_sync::init(app.handle());
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

    // -----------------------------------------------------------------------
    // Security review (task 4.4, docs/security.md): pin the IPC surface and
    // the webview hardening, so widening either needs a test change.
    // -----------------------------------------------------------------------

    use std::collections::BTreeSet;

    use crate::domain::ipc::{Window, COMMANDS, EVENTS};

    /// Buddy-window plumbing commands outside the contract in `domain::ipc`
    /// (both check the caller's window label too).
    const BUDDY_PLUMBING: &[&str] = &["set_hit_regions", "set_popover_open"];

    /// The only core (non-app) permission any window gets.
    const LISTEN: &str = "core:event:allow-listen";

    /// The quoted names inside `marker ... ]` in `source`.
    fn quoted_names_after(source: &str, marker: &str) -> BTreeSet<String> {
        let start = source.find(marker).expect("marker present") + marker.len();
        let end = start + source[start..].find(']').expect("closing bracket");
        source[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect()
    }

    /// Command names in `tauri::generate_handler![...]` in this file.
    fn handler_names() -> BTreeSet<String> {
        let source = include_str!("lib.rs");
        let marker = "generate_handler![";
        let start = source.find(marker).unwrap() + marker.len();
        let end = start + source[start..].find(']').unwrap();
        source[start..end]
            .split(',')
            .filter_map(|path| path.trim().rsplit("::").next())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn contract_and_plumbing() -> BTreeSet<String> {
        COMMANDS
            .iter()
            .map(|c| c.name)
            .chain(BUDDY_PLUMBING.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn capability(window: Window) -> serde_json::Value {
        let raw = match window {
            Window::Buddy => include_str!("../capabilities/buddy.json"),
            Window::Settings => include_str!("../capabilities/settings.json"),
        };
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn every_registered_command_is_declared_in_the_app_manifest() {
        // Declared in build.rs = has an `allow-*` permission and is denied
        // unless a capability grants it.
        let declared = quoted_names_after(include_str!("../build.rs"), ".commands(&[");
        assert_eq!(declared, handler_names());
        assert_eq!(declared, contract_and_plumbing());
    }

    #[test]
    fn capabilities_grant_exactly_the_contract_commands_per_window() {
        for window in [Window::Buddy, Window::Settings] {
            let cap = capability(window);
            assert_eq!(cap["windows"], serde_json::json!([window.label()]));
            assert!(cap.get("webviews").is_none() && cap.get("remote").is_none());

            let granted: Vec<&str> = cap["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().expect("plain permission identifiers only"))
                .collect();
            let unique: BTreeSet<&str> = granted.iter().copied().collect();
            assert_eq!(unique.len(), granted.len(), "duplicate grant");

            let plumbing: &[&str] = match window {
                Window::Buddy => BUDDY_PLUMBING,
                Window::Settings => &[],
            };
            let expected: BTreeSet<String> = COMMANDS
                .iter()
                .filter(|c| c.windows.contains(&window))
                .map(|c| c.name)
                .chain(plumbing.iter().copied())
                .map(|name| format!("allow-{}", name.replace('_', "-")))
                .chain([LISTEN.to_owned()])
                .collect();
            let granted: BTreeSet<String> = unique.into_iter().map(str::to_owned).collect();
            assert_eq!(granted, expected, "{} capability", window.label());
        }
    }

    #[test]
    fn only_the_two_window_capabilities_exist() {
        // Tauri loads every file in capabilities/, so a stray one would
        // widen the grants unnoticed.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/capabilities");
        let names: BTreeSet<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["buddy.json".to_owned(), "settings.json".to_owned()])
        );
    }

    #[test]
    fn rust_only_commands_are_not_exposed_to_any_window() {
        // Least privilege (owner 2026-11-21, docs/contracts.md): the cat
        // menu and tray do these in Rust; no page calls them.
        let declared = quoted_names_after(include_str!("../build.rs"), ".commands(&[");
        let handlers = handler_names();
        for name in [
            "snooze_all",
            "list_tasks",
            "update_task",
            "get_paused",
            "set_paused",
            "open_settings",
        ] {
            assert!(
                COMMANDS.iter().all(|c| c.name != name),
                "{name} in COMMANDS"
            );
            assert!(!declared.contains(name), "{name} declared in build.rs");
            assert!(!handlers.contains(name), "{name} registered");
        }
    }

    #[test]
    fn chime_event_is_app_internal() {
        assert!(EVENTS
            .iter()
            .all(|e| e.name != crate::reminders::CHIME_EVENT));
    }

    #[test]
    fn webview_is_hardened() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["app"]["withGlobalTauri"], false);
        let security = &conf["app"]["security"];
        assert_eq!(security["freezePrototype"], true);
        assert!(security
            .get("dangerousDisableAssetCspModification")
            .is_none());
        assert!(security.get("assetProtocol").is_none());

        for key in ["csp", "devCsp"] {
            let csp = security[key].as_str().unwrap();
            for required in [
                "default-src 'self'",
                "script-src 'self'",
                "object-src 'none'",
                "base-uri 'self'",
                "form-action 'none'",
                "frame-ancestors 'none'",
            ] {
                assert!(csp.contains(required), "{key} lacks {required}");
            }
            assert!(!csp.contains("unsafe-eval"), "{key}");
            assert!(!csp.contains('*'), "{key}");
            assert!(!csp.contains("https:"), "{key}: no remote sources");
        }
        let csp = security["csp"].as_str().unwrap();
        assert!(!csp.contains("unsafe-inline"));
        // The only non-self source: Tauri's IPC endpoint on Windows.
        let remote: Vec<&str> = csp
            .split([' ', ';'])
            .filter(|s| s.starts_with("http") || s.starts_with("ws"))
            .collect();
        assert_eq!(remote, ["http://ipc.localhost"]);
    }
}
