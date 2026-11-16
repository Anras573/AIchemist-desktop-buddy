//! Click-through for the buddy window (spec section 4, task 0.5 spike).
//!
//! The buddy window is transparent and covers more screen than the cat and
//! the speech bubble actually use. Only the *hit regions* (the bounding boxes
//! of the cat and the bubble) may receive the mouse; everywhere else clicks,
//! hovers and scrolls must reach whatever is underneath.
//!
//! Hit regions are rectangles in CSS pixels relative to the webview's
//! top-left corner. They are updated at runtime with [`set_regions`] (from
//! Rust) or the `set_hit_regions` IPC command (from the buddy page, which owns
//! the layout).
//!
//! Per-platform mechanism (see `docs/click-through.md` for the evaluation):
//!
//! - **Windows**: the window is shaped with `SetWindowRgn`. The OS hit-tests
//!   against the window region itself, so outside the region the window simply
//!   does not exist for the mouse. No events, no timers: zero runtime cost.
//! - **macOS**: `NSWindow.ignoresMouseEvents` is toggled from mouse-move
//!   events. While ignoring, a global `NSEvent` monitor sees the moves that go
//!   to other apps; while accepting, a local monitor (fed by a tracking area on
//!   the content view) sees ours. Purely event-driven: idle when the mouse is
//!   still.
//! - **Other platforms**: no-op (Linux is best effort, spec section 2).

use std::sync::{Arc, Mutex, MutexGuard};

use serde::Deserialize;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

/// Upper bound on the number of hit regions accepted from the page.
pub const MAX_REGIONS: usize = 8;

/// Largest coordinate or size accepted, in CSS pixels. Generous enough for
/// any real monitor; rejects absurd values from a misbehaving page.
const MAX_COORD: f64 = 100_000.0;

/// An axis-aligned hit rectangle in CSS pixels, relative to the webview's
/// top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct HitRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl HitRect {
    /// Whether the point (CSS pixels, webview-relative) lies inside.
    /// Left/top edges are inclusive, right/bottom exclusive.
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }

    fn is_valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= MAX_COORD)
            && self.width >= 0.0
            && self.height >= 0.0
    }

    /// Converts to a physical-pixel rectangle `(left, top, right, bottom)`,
    /// offset by `(dx, dy)` physical pixels. Rounds outwards so the region
    /// never clips a partially covered pixel.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn to_physical(self, scale: f64, dx: i32, dy: i32) -> (i32, i32, i32, i32) {
        let left = (self.x * scale).floor() as i32 + dx;
        let top = (self.y * scale).floor() as i32 + dy;
        let right = ((self.x + self.width) * scale).ceil() as i32 + dx;
        let bottom = ((self.y + self.height) * scale).ceil() as i32 + dy;
        (left, top, right, bottom)
    }
}

/// Validates regions received over IPC: at most [`MAX_REGIONS`], all finite,
/// non-negative sizes. Empty rectangles are dropped.
pub fn sanitize(regions: Vec<HitRect>) -> Result<Vec<HitRect>, String> {
    if regions.len() > MAX_REGIONS {
        return Err(format!("at most {MAX_REGIONS} hit regions are allowed"));
    }
    if let Some(bad) = regions.iter().find(|r| !r.is_valid()) {
        return Err(format!("invalid hit region: {bad:?}"));
    }
    Ok(regions
        .into_iter()
        .filter(|r| r.width > 0.0 && r.height > 0.0)
        .collect())
}

/// Managed state: the current hit regions of the buddy window.
#[derive(Default)]
struct HitRegions(Arc<Mutex<Vec<HitRect>>>);

fn lock(regions: &Mutex<Vec<HitRect>>) -> MutexGuard<'_, Vec<HitRect>> {
    // A poisoned lock only means a panic elsewhere; the data is still valid.
    regions.lock().unwrap_or_else(|e| e.into_inner())
}

/// Installs click-through on `window`. Until regions are set, the whole window
/// is click-through (and, on Windows, invisible).
pub fn init<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>) -> tauri::Result<()> {
    let state = HitRegions::default();
    let shared = Arc::clone(&state.0);
    app.manage(state);
    platform::install(window, shared)
}

/// Replaces the hit regions of `window` and applies them immediately.
pub fn set_regions<R: Runtime>(
    window: &WebviewWindow<R>,
    regions: Vec<HitRect>,
) -> Result<(), String> {
    let regions = sanitize(regions)?;
    let state = window
        .try_state::<HitRegions>()
        .ok_or("click-through is not initialised")?;
    *lock(&state.0) = regions;
    platform::apply(window, Arc::clone(&state.0)).map_err(|e| e.to_string())
}

/// IPC: the buddy page reports the bounding boxes of the cat and the bubble
/// whenever its layout changes (e.g. the bubble appears or disappears).
#[tauri::command]
pub fn set_hit_regions<R: Runtime>(
    window: WebviewWindow<R>,
    regions: Vec<HitRect>,
) -> Result<(), String> {
    if window.label() != crate::BUDDY_WINDOW_LABEL {
        return Err("only the buddy window has hit regions".into());
    }
    set_regions(&window, regions)
}

#[cfg(windows)]
mod platform {
    //! `SetWindowRgn`: the window region is both the visible and the
    //! hit-testable area of the window. Outside it, the OS delivers mouse
    //! input to whatever window is underneath.

    use std::sync::{Arc, Mutex};

    use tauri::{Runtime, WebviewWindow, WindowEvent};
    use windows::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, HGDIOBJ, RGN_OR,
    };

    use super::{lock, HitRect};

    pub fn install<R: Runtime>(
        window: &WebviewWindow<R>,
        regions: Arc<Mutex<Vec<HitRect>>>,
    ) -> tauri::Result<()> {
        // The region is in physical pixels, so it must be rebuilt when the
        // scale factor or the window size changes.
        let w = window.clone();
        let shared = Arc::clone(&regions);
        window.on_window_event(move |event| {
            if matches!(
                event,
                WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Resized(_)
            ) {
                let _ = apply(&w, Arc::clone(&shared));
            }
        });
        apply(window, regions)
    }

    pub fn apply<R: Runtime>(
        window: &WebviewWindow<R>,
        regions: Arc<Mutex<Vec<HitRect>>>,
    ) -> tauri::Result<()> {
        let hwnd = window.hwnd()?;
        let scale = window.scale_factor()?;
        // Window-region coordinates are relative to the window rect, not the
        // client area. For a frameless window they coincide, but compute the
        // offset rather than assume it.
        let outer = window.outer_position()?;
        let inner = window.inner_position()?;
        let (dx, dy) = (inner.x - outer.x, inner.y - outer.y);
        let rects: Vec<_> = lock(&regions)
            .iter()
            .map(|r| r.to_physical(scale, dx, dy))
            .collect();

        // SAFETY: plain GDI calls on a valid HWND owned by this process. On
        // success SetWindowRgn takes ownership of `combined`; every other
        // region is deleted here.
        unsafe {
            let combined = CreateRectRgn(0, 0, 0, 0);
            for (l, t, r, b) in rects {
                let part = CreateRectRgn(l, t, r, b);
                CombineRgn(Some(combined), Some(combined), Some(part), RGN_OR);
                let _ = DeleteObject(HGDIOBJ(part.0));
            }
            if SetWindowRgn(hwnd, Some(combined), true) == 0 {
                let _ = DeleteObject(HGDIOBJ(combined.0));
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! `ignoresMouseEvents` toggled from mouse-move events.
    //!
    //! - While the window ignores the mouse, moves are delivered to other
    //!   apps, which a *global* monitor observes (mouse events do not need
    //!   the Accessibility permission; only key events do).
    //! - While the window accepts the mouse, moves are delivered to us. A
    //!   tracking area (`ActiveAlways`, so it also works while the app is
    //!   inactive, which is almost always) makes AppKit generate them, and a
    //!   *local* monitor observes them.
    //!
    //! Each event costs one point-in-rect test. Nothing runs while the mouse
    //! is still, so idle CPU is unaffected.

    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSEvent, NSEventMask, NSTrackingArea, NSTrackingAreaOptions, NSWindow};
    use tauri::{Runtime, WebviewWindow};

    use super::{lock, HitRect};

    /// Main-thread-only state. AppKit objects are not `Send`, so they live in
    /// a thread local on the main thread rather than in Tauri state.
    struct Tracker {
        window: Retained<NSWindow>,
        regions: Arc<Mutex<Vec<HitRect>>>,
        // Kept alive for the lifetime of the app; never removed.
        _monitors: Vec<Retained<AnyObject>>,
        _tracking_area: Option<Retained<NSTrackingArea>>,
    }

    thread_local! {
        static TRACKER: RefCell<Option<Tracker>> = const { RefCell::new(None) };
    }

    impl Tracker {
        /// Point-in-region test at the current cursor position, then flip
        /// `ignoresMouseEvents` only if the answer changed.
        fn update(&self) {
            // Both in global screen points, origin bottom-left.
            let mouse = NSEvent::mouseLocation();
            let content = self.window.contentRectForFrameRect(self.window.frame());
            // Webview-relative, origin top-left. One point is one CSS pixel
            // at the default zoom.
            let x = mouse.x - content.origin.x;
            let y = content.origin.y + content.size.height - mouse.y;
            let inside = lock(&self.regions).iter().any(|r| r.contains(x, y));
            if self.window.ignoresMouseEvents() == inside {
                self.window.setIgnoresMouseEvents(!inside);
            }
        }
    }

    fn update_tracker() {
        TRACKER.with(|t| {
            if let Ok(t) = t.try_borrow() {
                if let Some(t) = t.as_ref() {
                    t.update();
                }
            }
        });
    }

    fn mouse_mask() -> NSEventMask {
        NSEventMask::MouseMoved
            | NSEventMask::LeftMouseDragged
            | NSEventMask::RightMouseDragged
            | NSEventMask::OtherMouseDragged
            | NSEventMask::MouseEntered
            | NSEventMask::MouseExited
    }

    pub fn install<R: Runtime>(
        window: &WebviewWindow<R>,
        regions: Arc<Mutex<Vec<HitRect>>>,
    ) -> tauri::Result<()> {
        let w = window.clone();
        window.run_on_main_thread(move || {
            let Ok(ptr) = w.ns_window() else { return };
            // SAFETY: Tauri returns the window's live NSWindow pointer; we
            // take our own strong reference to it.
            let Some(ns_window) = (unsafe { Retained::retain(ptr.cast::<NSWindow>()) }) else {
                return;
            };
            // Start fully click-through until the page reports its regions.
            ns_window.setIgnoresMouseEvents(true);

            let global = RcBlock::new(|_event: NonNull<NSEvent>| update_tracker());
            let local = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
                update_tracker();
                // Pass the event on unchanged.
                event.as_ptr()
            });
            let mut monitors = Vec::new();
            monitors.extend(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
                mouse_mask(),
                &global,
            ));
            // SAFETY: the handler returns the event it was given, which is a
            // valid pointer.
            monitors.extend(unsafe {
                NSEvent::addLocalMonitorForEventsMatchingMask_handler(mouse_mask(), &local)
            });

            // Make AppKit generate mouse-moved/exited events for this window
            // even while the app is inactive, so the local monitor sees the
            // cursor leave a hit region.
            let tracking_area = ns_window.contentView().map(|view| {
                // SAFETY: owner is the content view (an NSResponder, which
                // handles mouseMoved:/mouseExited: by forwarding them up the
                // responder chain); no user info.
                let area = unsafe {
                    NSTrackingArea::initWithRect_options_owner_userInfo(
                        NSTrackingArea::alloc(),
                        Default::default(),
                        NSTrackingAreaOptions::MouseMoved
                            | NSTrackingAreaOptions::MouseEnteredAndExited
                            | NSTrackingAreaOptions::ActiveAlways
                            | NSTrackingAreaOptions::InVisibleRect,
                        Some(&view),
                        None,
                    )
                };
                view.addTrackingArea(&area);
                area
            });

            TRACKER.with(|t| {
                *t.borrow_mut() = Some(Tracker {
                    window: ns_window,
                    regions,
                    _monitors: monitors,
                    _tracking_area: tracking_area,
                });
            });
            // Regions may already have been reported.
            update_tracker();
        })
    }

    pub fn apply<R: Runtime>(
        window: &WebviewWindow<R>,
        _regions: Arc<Mutex<Vec<HitRect>>>,
    ) -> tauri::Result<()> {
        // Re-evaluate now: the bubble may have appeared under, or vanished
        // from under, a cursor that is not moving.
        window.run_on_main_thread(update_tracker)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    //! Linux is best effort (spec section 2): the window is not click-through.

    use std::sync::{Arc, Mutex};

    use tauri::{Runtime, WebviewWindow};

    use super::HitRect;

    pub fn install<R: Runtime>(
        _window: &WebviewWindow<R>,
        _regions: Arc<Mutex<Vec<HitRect>>>,
    ) -> tauri::Result<()> {
        Ok(())
    }

    pub fn apply<R: Runtime>(
        _window: &WebviewWindow<R>,
        _regions: Arc<Mutex<Vec<HitRect>>>,
    ) -> tauri::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> HitRect {
        HitRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn contains_is_inclusive_top_left_exclusive_bottom_right() {
        let r = rect(10.0, 20.0, 30.0, 40.0);
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(39.9, 59.9));
        assert!(!r.contains(40.0, 30.0));
        assert!(!r.contains(20.0, 60.0));
        assert!(!r.contains(9.9, 30.0));
    }

    #[test]
    fn to_physical_scales_offsets_and_rounds_outwards() {
        let r = rect(10.5, 0.0, 10.0, 10.25);
        assert_eq!(r.to_physical(1.0, 0, 0), (10, 0, 21, 11));
        assert_eq!(r.to_physical(1.5, 2, 3), (17, 3, 33, 19));
    }

    #[test]
    fn sanitize_drops_empty_and_rejects_invalid() {
        let ok = sanitize(vec![rect(0.0, 0.0, 10.0, 10.0), rect(5.0, 5.0, 0.0, 3.0)]).unwrap();
        assert_eq!(ok, vec![rect(0.0, 0.0, 10.0, 10.0)]);

        assert!(sanitize(vec![rect(f64::NAN, 0.0, 1.0, 1.0)]).is_err());
        assert!(sanitize(vec![rect(0.0, 0.0, -1.0, 1.0)]).is_err());
        assert!(sanitize(vec![rect(0.0, 0.0, 1e9, 1.0)]).is_err());
        assert!(sanitize(vec![rect(0.0, 0.0, 1.0, 1.0); MAX_REGIONS + 1]).is_err());
        assert_eq!(sanitize(vec![]).unwrap(), vec![]);
    }

    #[test]
    fn regions_deserialize_from_the_page_payload() {
        let json = r#"[{"x":1,"y":2,"width":3,"height":4}]"#;
        let regions: Vec<HitRect> = serde_json::from_str(json).unwrap();
        assert_eq!(regions, vec![rect(1.0, 2.0, 3.0, 4.0)]);
    }
}
