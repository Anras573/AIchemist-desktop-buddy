//! AIchemist Desktop Buddy core.
//!
//! [`domain`] holds the frozen shared contracts (types, traits, IPC contract).
//! The scheduler, providers, storage and state machine are added by later
//! tasks and build against it.

#[deny(missing_docs)]
pub mod domain;
pub mod state_machine;
pub mod storage;

/// Label of the buddy window declared in `tauri.conf.json`.
pub const BUDDY_WINDOW_LABEL: &str = "buddy";

/// Builds and runs the Tauri application. Blocks until the app exits.
pub fn run() {
    tauri::Builder::default()
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
    fn config_sets_a_content_security_policy() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json is valid JSON");
        let csp = conf["app"]["security"]["csp"]
            .as_str()
            .expect("app.security.csp is set");

        assert!(csp.contains("default-src 'self'"));
    }
}
