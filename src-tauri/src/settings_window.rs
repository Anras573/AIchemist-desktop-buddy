//! The settings window (spec 4.5): a normal, decorated window created on
//! demand and **destroyed** when closed, so its webview does not stay in
//! memory. Only the buddy window exists while the app is idle.
//!
//! Closing uses Tauri's default close behaviour, which destroys the window
//! and its webview (nothing here calls `prevent_close` or hides it).

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

use crate::BUDDY_WINDOW_LABEL;

/// Label of the settings window.
pub const SETTINGS_WINDOW_LABEL: &str = "settings";
/// Window title.
pub const TITLE: &str = "AIchemist Buddy Settings";
/// Page inside the frontend dist.
const URL: &str = "settings/index.html";

/// Opens the settings window, or focuses it if it is already open.
/// Safe to call from any thread, including menu event handlers.
pub fn open<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    // Build off the calling thread: on Windows, creating a webview from a
    // handler that runs on the event loop thread can deadlock (wry#583).
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = build(&app) {
            log::warn!("cannot open the settings window: {e}");
        }
    });
}

fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let mut builder =
        WebviewWindowBuilder::new(app, SETTINGS_WINDOW_LABEL, WebviewUrl::App(URL.into()))
            .title(TITLE)
            .inner_size(560.0, 680.0)
            .min_inner_size(420.0, 360.0)
            .resizable(true)
            .center()
            .focused(true);
    // WebView2 shares one browser process per user data folder and refuses a
    // second environment with different options, so reuse the buddy's args.
    if let Some(args) = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == BUDDY_WINDOW_LABEL)
        .and_then(|w| w.additional_browser_args.as_deref())
    {
        builder = builder.additional_browser_args(args);
    }
    let window = builder.build()?;
    let _ = window.set_focus();
    Ok(())
}

/// A connected monitor, for the monitor selector.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    /// Name as stored in `AppearanceSettings::monitor`.
    pub name: String,
    /// Whether this is the primary monitor.
    pub primary: bool,
    /// Size in physical pixels.
    pub width: u32,
    /// Size in physical pixels.
    pub height: u32,
}

/// `list_monitors` (settings window only; not part of the frozen IPC
/// contract, see the PR notes): connected monitors with a name, primary
/// first.
#[tauri::command]
pub fn list_monitors(app: AppHandle) -> Vec<MonitorInfo> {
    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .and_then(|m| m.name().cloned());
    let mut monitors: Vec<MonitorInfo> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let name = m.name()?.clone();
            Some(MonitorInfo {
                primary: primary.as_ref() == Some(&name),
                name,
                width: m.size().width,
                height: m.size().height,
            })
        })
        .collect();
    monitors.sort_by_key(|m| !m.primary);
    monitors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ipc::Window;

    #[test]
    fn label_matches_the_ipc_contract() {
        assert_eq!(Window::Settings.label(), SETTINGS_WINDOW_LABEL);
    }
}
