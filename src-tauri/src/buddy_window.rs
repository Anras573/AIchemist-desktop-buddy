//! The buddy window shell (spec 4.1): window behaviour, positioning and
//! re-positioning on display changes.
//!
//! Static properties (transparent, frameless, always-on-top, skip-taskbar,
//! no shadow, hidden until placed, not focused on show, all Spaces on macOS)
//! are set in `tauri.conf.json`. This module adds what config cannot:
//!
//! - **Placement** from [`AppearanceSettings`] (corner, monitor, cat scale);
//!   the math is in [`crate::placement`].
//! - **Re-placement**, event-driven, no polling: on `ScaleFactorChanged`, on
//!   display or work-area changes (Windows: `WM_DISPLAYCHANGE` and
//!   `WM_SETTINGCHANGE(SPI_SETWORKAREA)` through a window subclass; macOS:
//!   `NSApplicationDidChangeScreenParametersNotification`), and when
//!   [`set_appearance`] is called.
//! - **Windows**: `WS_EX_TOOLWINDOW`, which hides the window from Alt+Tab
//!   and makes Windows show it on every virtual desktop. Tao rewrites the
//!   extended style whenever it changes window flags, so the subclass
//!   re-adds the bit in `WM_STYLECHANGING`.
//! - **macOS**: `ActivationPolicy::Accessory` (no Dock icon, no app menu).
//!
//! Showing the window does not take focus: config `focus: false` makes tao
//! show it with `SW_SHOWNOACTIVATE` on Windows, and on macOS an accessory
//! app is never activated by showing a window.

use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow, WindowEvent};

use crate::domain::ipc::event;
use crate::domain::AppearanceSettings;
use crate::placement::{self, Placement, Rect, Screen};
use crate::BUDDY_WINDOW_LABEL;

/// Managed state: the appearance the window is currently placed for.
#[derive(Default)]
struct Appearance(Mutex<AppearanceSettings>);

fn lock(m: &Mutex<AppearanceSettings>) -> MutexGuard<'_, AppearanceSettings> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets up the buddy window: platform behaviour, initial placement, then
/// shows it. Call once from the Tauri `setup` hook, on the main thread.
///
/// `appearance` is the persisted appearance (defaults until settings
/// loading lands).
pub fn init<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    appearance: AppearanceSettings,
) -> tauri::Result<()> {
    app.manage(Appearance(Mutex::new(appearance)));
    platform::init(app, window)?;

    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::ScaleFactorChanged { .. } = event {
            // Deferred: tao applies its own suggested size right after this
            // event, which would overwrite a size set from inside it.
            schedule_reposition(&handle);
        }
    });

    // Place before showing, so the window never flashes in the wrong spot.
    // A placement failure must not leave the buddy hidden.
    let placed = reposition(window);
    window.show()?;
    placed
}

/// The appearance the buddy window is currently placed for. For the
/// `get_appearance` command.
pub fn appearance<R: Runtime>(app: &AppHandle<R>) -> AppearanceSettings {
    app.try_state::<Appearance>()
        .map(|s| lock(&s.0).clone())
        .unwrap_or_default()
}

/// Applies a new appearance live (spec 8): stores it, re-places and resizes
/// the window, and emits `appearance-changed` to the buddy page so it can
/// redraw the cat at the new scale. Call from `update_settings` after
/// validation. Safe to call from any thread.
pub fn set_appearance<R: Runtime>(app: &AppHandle<R>, appearance: AppearanceSettings) {
    if let Some(state) = app.try_state::<Appearance>() {
        *lock(&state.0) = appearance.clone();
    }
    let _ = app.emit_to(BUDDY_WINDOW_LABEL, event::APPEARANCE_CHANGED, appearance);
    schedule_reposition(app);
}

/// Re-places the buddy window soon, from the event loop rather than inside
/// the current call stack (safe inside window procedures and event handlers).
pub fn schedule_reposition<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    // Window calls from a non-main thread are posted to the event loop, so
    // this never re-enters the code that triggered it.
    tauri::async_runtime::spawn(async move {
        if let Some(window) = app.get_webview_window(BUDDY_WINDOW_LABEL) {
            let _ = reposition(&window);
        }
    });
}

/// Computes the placement for the current appearance and monitors and
/// applies it if it differs from the window's current frame.
pub fn reposition<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    let appearance = appearance(window.app_handle());
    let screens: Vec<Screen> = window.available_monitors()?.iter().map(to_screen).collect();
    let primary = window.primary_monitor()?.map(|m| to_screen(&m));
    let Some(screen) =
        placement::select_screen(&screens, primary.as_ref(), appearance.monitor.as_deref())
    else {
        return Ok(()); // No monitor at all (e.g. all displays asleep).
    };
    let target = placement::place(
        screen,
        appearance.corner,
        placement::window_size_css(appearance.cat_scale),
    );
    apply(window, target)
}

/// Converts a Tauri monitor to screen units (see [`crate::placement`]).
fn to_screen(m: &tauri::Monitor) -> Screen {
    let area = m.work_area();
    let scale = m.scale_factor();
    // macOS: points. Windows/Linux: physical pixels.
    let div = if cfg!(target_os = "macos") {
        scale
    } else {
        1.0
    };
    Screen {
        name: m.name().cloned(),
        work_area: Rect {
            x: f64::from(area.position.x) / div,
            y: f64::from(area.position.y) / div,
            width: f64::from(area.size.width) / div,
            height: f64::from(area.size.height) / div,
        },
        units_per_css_px: scale / div,
    }
}

/// Moves first, then resizes: if the move crosses to a monitor with another
/// scale, tao applies the OS-suggested size during the move and our resize
/// then sets the exact one.
fn apply<R: Runtime>(window: &WebviewWindow<R>, p: Placement) -> tauri::Result<()> {
    if cfg!(target_os = "macos") {
        use tauri::{LogicalPosition, LogicalSize};
        let scale = window.scale_factor()?;
        let pos = window.outer_position()?.to_logical::<f64>(scale);
        if (pos.x.round() as i32, pos.y.round() as i32) != (p.x, p.y) {
            window.set_position(LogicalPosition::new(p.x, p.y))?;
        }
        let size = window.inner_size()?.to_logical::<f64>(scale);
        if (size.width.round() as u32, size.height.round() as u32) != (p.width, p.height) {
            window.set_size(LogicalSize::new(p.width, p.height))?;
        }
    } else {
        use tauri::{PhysicalPosition, PhysicalSize};
        let pos = window.outer_position()?;
        if (pos.x, pos.y) != (p.x, p.y) {
            window.set_position(PhysicalPosition::new(p.x, p.y))?;
        }
        let size = window.inner_size()?;
        if (size.width, size.height) != (p.width, p.height) {
            window.set_size(PhysicalSize::new(p.width, p.height))?;
        }
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    //! Window subclass: tool-window style and display-change notifications.

    use tauri::{AppHandle, Runtime, WebviewWindow};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, SPI_SETWORKAREA,
        STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        WM_DISPLAYCHANGE, WM_SETTINGCHANGE, WM_STYLECHANGING, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    /// Arbitrary id for our subclass on the buddy window ("BUDY").
    const SUBCLASS_ID: usize = 0x4255_4459;

    type Callback = Box<dyn Fn() + Send + Sync>;

    pub fn init<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>) -> tauri::Result<()> {
        let hwnd = window.hwnd()?;
        let handle = app.clone();
        let on_display_change: Callback = Box::new(move || super::schedule_reposition(&handle));
        // Lives as long as the window, which lives as long as the app.
        let data = Box::into_raw(Box::new(on_display_change)) as usize;

        // SAFETY: called on the main thread, which owns `hwnd`. `data` points
        // to a leaked callback, valid for the rest of the process.
        unsafe {
            if !SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, data).as_bool() {
                drop(Box::from_raw(data as *mut Callback));
                return Ok(()); // Not fatal: no tool-window style, no auto re-place.
            }
            // Triggers WM_STYLECHANGING, where the subclass adds the bits.
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW.0 as isize);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        Ok(())
    }

    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        match msg {
            WM_DISPLAYCHANGE => notify(data),
            WM_SETTINGCHANGE if wparam.0 as u32 == SPI_SETWORKAREA.0 => notify(data),
            WM_STYLECHANGING if wparam.0 as i32 == GWL_EXSTYLE.0 => {
                // SAFETY: for WM_STYLECHANGING, lparam points to a STYLESTRUCT
                // the system lets us modify.
                let style = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
                style.styleNew = (style.styleNew | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0;
            }
            _ => {}
        }
        // SAFETY: forwarding the unmodified message to the next handler.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    fn notify(data: usize) {
        // SAFETY: `data` is the leaked callback from `init`.
        let callback = unsafe { &*(data as *const Callback) };
        callback();
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! Accessory activation policy and screen-change notifications.

    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2_app_kit::NSApplicationDidChangeScreenParametersNotification;
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    use tauri::{ActivationPolicy, AppHandle, Runtime, WebviewWindow};

    pub fn init<R: Runtime>(app: &AppHandle<R>, _window: &WebviewWindow<R>) -> tauri::Result<()> {
        // No Dock icon and no app menu; also keeps showing the window from
        // activating the app (no focus stealing).
        app.set_activation_policy(ActivationPolicy::Accessory)?;

        // Fires when displays are added, removed or rearranged and when a
        // resolution or scale changes. Whether Dock/menu bar changes (visible
        // frame) also post it is on the macOS checklist
        // (docs/click-through.md).
        let handle = app.clone();
        let block = RcBlock::new(move |_n: NonNull<NSNotification>| {
            super::schedule_reposition(&handle);
        });
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: a valid notification name, no sender filter, delivered on
        // the posting (main) thread; the block only schedules work.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidChangeScreenParametersNotification),
                None,
                None,
                &block,
            )
        };
        // The observer stays registered for the life of the app.
        std::mem::forget(observer);
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    //! Linux is best effort (spec 2): placement on start, scale changes and
    //! appearance changes only.

    use tauri::{AppHandle, Runtime, WebviewWindow};

    pub fn init<R: Runtime>(_app: &AppHandle<R>, _window: &WebviewWindow<R>) -> tauri::Result<()> {
        Ok(())
    }
}
