//! "Already running" notice: a second launch of the app exits (the
//! single-instance plugin) and the running instance shows a small native
//! message so the user knows where the buddy is.
//!
//! - Windows: `MessageBoxW` on its own thread, so the event loop keeps
//!   running.
//! - macOS: `NSAlert` on the main thread (AppKit requires it). `runModal`
//!   blocks the event loop until the user clicks OK.
//! - Linux: logged only.
//!
//! Only one notice at a time: launches while it is showing are ignored.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Runtime};

/// Message title / first line.
pub const MESSAGE: &str = "AIchemist Buddy is already running.";
/// Hint shown below the message.
#[cfg(target_os = "macos")]
pub const HINT: &str = "Find it in the menu bar.";
/// Hint shown below the message.
#[cfg(not(target_os = "macos"))]
pub const HINT: &str = "Find it in the system tray (click the ^ arrow if the icon is hidden).";

static SHOWING: AtomicBool = AtomicBool::new(false);

/// Claims the single notice slot. Returns false if a notice is showing.
fn try_claim() -> bool {
    !SHOWING.swap(true, Ordering::SeqCst)
}

fn release() {
    SHOWING.store(false, Ordering::SeqCst);
}

/// Shows the notice unless one is already showing. Call from the
/// single-instance callback; returns immediately.
pub fn notify<R: Runtime>(app: &AppHandle<R>) {
    if !try_claim() {
        return;
    }
    log::info!("a second launch was refused: the app is already running");
    platform::show(app);
}

#[cfg(windows)]
mod platform {
    use tauri::{AppHandle, Runtime};
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };

    pub fn show<R: Runtime>(_app: &AppHandle<R>) {
        std::thread::spawn(|| {
            let text = HSTRING::from(format!("{}\n\n{}", super::MESSAGE, super::HINT));
            let caption = HSTRING::from("AIchemist Buddy");
            // SAFETY: plain Win32 call with valid, NUL-terminated strings
            // that outlive it; no owner window.
            let result = unsafe {
                MessageBoxW(
                    None,
                    &text,
                    &caption,
                    MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
                )
            };
            if result.0 == 0 {
                log::warn!(
                    "cannot show the already-running notice: {}",
                    windows::core::Error::from_thread()
                );
            }
            super::release();
        });
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication};
    use objc2_foundation::NSString;
    use tauri::{AppHandle, Runtime};

    pub fn show<R: Runtime>(app: &AppHandle<R>) {
        let shown = app.run_on_main_thread(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                // An accessory app must activate itself, or the alert opens
                // behind the frontmost app.
                #[allow(deprecated)]
                NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
                let alert = NSAlert::new(mtm);
                alert.setAlertStyle(NSAlertStyle::Informational);
                alert.setMessageText(&NSString::from_str(super::MESSAGE));
                alert.setInformativeText(&NSString::from_str(super::HINT));
                alert.runModal();
            }
            super::release();
        });
        if shown.is_err() {
            super::release();
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use tauri::{AppHandle, Runtime};

    pub fn show<R: Runtime>(_app: &AppHandle<R>) {
        log::warn!("{} {}", super::MESSAGE, super::HINT);
        super::release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_notice_at_a_time() {
        assert!(try_claim());
        assert!(!try_claim(), "a second launch while showing is ignored");
        release();
        assert!(try_claim());
        release();
    }
}
