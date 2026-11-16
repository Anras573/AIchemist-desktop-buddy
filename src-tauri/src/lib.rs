//! AIchemist Desktop Buddy core.
//!
//! [`domain`] holds the frozen shared contracts (types, traits, IPC contract).
//! The scheduler, providers, storage and state machine are added by later
//! tasks and build against it.

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

pub mod buddy_window;
pub mod click_through;
pub mod pause;
pub mod placement;
pub mod tray;

/// Label of the buddy window declared in `tauri.conf.json`.
pub const BUDDY_WINDOW_LABEL: &str = "buddy";

/// Builds and runs the Tauri application. Blocks until the app exits.
pub fn run() {
    tauri::Builder::default()
        // Must be the first plugin: a second launch exits here, before it
        // creates any window (spec 4: one buddy).
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            // The buddy is already on screen; nothing else to do.
        }))
        .invoke_handler(tauri::generate_handler![click_through::set_hit_regions])
        .setup(|app| {
            pause::init(app.handle());
            if let Some(buddy) = app.get_webview_window(BUDDY_WINDOW_LABEL) {
                click_through::init(app.handle(), &buddy)?;
                // Settings loading lands later; until then, the defaults.
                buddy_window::init(app.handle(), &buddy, Default::default())?;
            }
            tray::init(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Tauri application");
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
    fn config_sets_a_content_security_policy() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json is valid JSON");
        let csp = conf["app"]["security"]["csp"]
            .as_str()
            .expect("app.security.csp is set");

        assert!(csp.contains("default-src 'self'"));
    }
}
