//! Pure placement math for the buddy window (spec 4, 8 Appearance, 14.6).
//!
//! No Tauri types here, so everything is unit-testable. The glue in
//! [`crate::buddy_window`] feeds in the connected monitors and applies the
//! result.
//!
//! # Units
//!
//! Window content is laid out in CSS pixels. [`place`] works in whatever
//! *screen units* the caller uses for the work area, given how many screen
//! units one CSS pixel takes on the target monitor:
//!
//! - **Windows** (per-monitor DPI aware): physical pixels, `units_per_css_px`
//!   = the monitor's scale factor. Physical coordinates are global and
//!   consistent across monitors with different scales.
//! - **macOS**: points (physical / monitor scale), `units_per_css_px` = 1.
//!   AppKit positions windows in points, and a secondary monitor's physical
//!   origin as reported by Tauri is scaled by *its own* factor, so physical
//!   coordinates are not comparable across monitors there.
//!
//! # Window size
//!
//! The window holds the cat (one 48x48 sprite frame, `assets/sprites/
//! default/pack.json`, at the integer `cat_scale`) with the speech-bubble
//! area stacked above it, plus a small padding on every side for animation
//! overshoot. See [`window_size_css`].

use crate::domain::ScreenCorner;

/// Sprite frame size in CSS pixels, before `cat_scale`.
pub const FRAME_SIZE: u32 = 48;
/// Width of the speech-bubble area in CSS pixels.
pub const BUBBLE_AREA_WIDTH: u32 = 240;
/// Height of the speech-bubble area (above the cat) in CSS pixels.
pub const BUBBLE_AREA_HEIGHT: u32 = 120;
/// Transparent padding inside the window on every side, in CSS pixels.
pub const WINDOW_PADDING: u32 = 8;
/// Gap between the window and the edges of the work area, in CSS pixels.
pub const SCREEN_MARGIN: f64 = 8.0;

/// Window content size in CSS pixels for the given cat scale:
/// width `max(bubble, cat) + 2 * padding`, height
/// `bubble + cat + 2 * padding`. Default scale 2 gives 256 x 232.
pub fn window_size_css(cat_scale: u32) -> (u32, u32) {
    let cat = FRAME_SIZE.saturating_mul(cat_scale.max(1));
    (
        BUBBLE_AREA_WIDTH.max(cat) + 2 * WINDOW_PADDING,
        BUBBLE_AREA_HEIGHT + cat + 2 * WINDOW_PADDING,
    )
}

/// An axis-aligned rectangle in screen units (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// A connected monitor, as the glue reads it from Tauri.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    /// `Monitor::name()`, if the OS reports one.
    pub name: Option<String>,
    /// Work area (excludes taskbar, menu bar, Dock) in screen units.
    pub work_area: Rect,
    /// Screen units per CSS pixel on this monitor.
    pub units_per_css_px: f64,
}

/// Where the window goes and how big it is, in screen units, rounded to
/// whole units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Left edge of the window.
    pub x: i32,
    /// Top edge of the window.
    pub y: i32,
    /// Window width.
    pub width: u32,
    /// Window height.
    pub height: u32,
}

/// Picks the monitor to use: the one whose name equals `wanted`, else the
/// primary, else the first one. `None` only if `screens` is empty.
pub fn select_screen<'a>(
    screens: &'a [Screen],
    primary: Option<&'a Screen>,
    wanted: Option<&str>,
) -> Option<&'a Screen> {
    wanted
        .and_then(|name| screens.iter().find(|s| s.name.as_deref() == Some(name)))
        .or(primary)
        .or_else(|| screens.first())
}

/// Places a window of `size_css` CSS pixels in `corner` of `screen`'s work
/// area, [`SCREEN_MARGIN`] away from both edges.
///
/// If the window does not fit, it is clamped so its top-left corner stays
/// inside the work area (the cat may then be cut off at the bottom/right,
/// which only happens with a huge scale on a tiny screen).
pub fn place(screen: &Screen, corner: ScreenCorner, size_css: (u32, u32)) -> Placement {
    let k = if screen.units_per_css_px.is_finite() && screen.units_per_css_px > 0.0 {
        screen.units_per_css_px
    } else {
        1.0
    };
    let width = (f64::from(size_css.0) * k).ceil();
    let height = (f64::from(size_css.1) * k).ceil();
    let margin = (SCREEN_MARGIN * k).round();
    let area = screen.work_area;

    let left = area.x + margin;
    let top = area.y + margin;
    let right = area.x + area.width - margin - width;
    let bottom = area.y + area.height - margin - height;

    let (x, y) = match corner {
        ScreenCorner::TopLeft => (left, top),
        ScreenCorner::TopRight => (right, top),
        ScreenCorner::BottomLeft => (left, bottom),
        ScreenCorner::BottomRight => (right, bottom),
    };
    Placement {
        x: x.max(area.x).round() as i32,
        y: y.max(area.y).round() as i32,
        width: width as u32,
        height: height as u32,
    }
}

/// Size of the upcoming popover in CSS pixels (task 2.3). It does not fit
/// the bubble area, so the window grows while the popover is open.
pub const POPOVER_WIDTH: u32 = 288;
/// Height of the upcoming popover in CSS pixels.
pub const POPOVER_HEIGHT: u32 = 360;

/// Window content size in CSS pixels while the popover is open.
///
/// The cat must not move, so the window grows away from its corner:
/// - bottom corners: the popover sits above the cat, in place of the bubble
///   area: height `popover + cat + 2 * padding`;
/// - top corners: the bubble area stays above the cat and the popover goes
///   below it: height `bubble + cat + popover + 2 * padding`.
pub fn popover_window_size_css(cat_scale: u32, corner: ScreenCorner) -> (u32, u32) {
    let cat = FRAME_SIZE.saturating_mul(cat_scale.max(1));
    let width = POPOVER_WIDTH.max(BUBBLE_AREA_WIDTH).max(cat) + 2 * WINDOW_PADDING;
    let above_cat = match corner {
        ScreenCorner::BottomLeft | ScreenCorner::BottomRight => {
            POPOVER_HEIGHT.max(BUBBLE_AREA_HEIGHT)
        }
        ScreenCorner::TopLeft | ScreenCorner::TopRight => BUBBLE_AREA_HEIGHT + POPOVER_HEIGHT,
    };
    (width, above_cat + cat + 2 * WINDOW_PADDING)
}

/// Places a window of `grown_css` so that the content a window of
/// `normal_css` would show at its corner side stays where it is: the
/// window extends left for right corners, up for bottom corners, and right
/// or down otherwise. The top-left corner is clamped into the work area.
pub fn place_grown(
    screen: &Screen,
    corner: ScreenCorner,
    normal_css: (u32, u32),
    grown_css: (u32, u32),
) -> Placement {
    let normal = place(screen, corner, normal_css);
    let grown = place(screen, corner, grown_css);
    let (w, h) = (i64::from(grown.width), i64::from(grown.height));
    let x = match corner {
        ScreenCorner::TopRight | ScreenCorner::BottomRight => {
            i64::from(normal.x) + i64::from(normal.width) - w
        }
        ScreenCorner::TopLeft | ScreenCorner::BottomLeft => i64::from(normal.x),
    };
    let y = match corner {
        ScreenCorner::BottomLeft | ScreenCorner::BottomRight => {
            i64::from(normal.y) + i64::from(normal.height) - h
        }
        ScreenCorner::TopLeft | ScreenCorner::TopRight => i64::from(normal.y),
    };
    let area = screen.work_area;
    Placement {
        x: (x as f64).max(area.x).round() as i32,
        y: (y as f64).max(area.y).round() as i32,
        width: grown.width,
        height: grown.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, x: f64, y: f64, w: f64, h: f64, k: f64) -> Screen {
        Screen {
            name: Some(name.to_owned()),
            work_area: Rect {
                x,
                y,
                width: w,
                height: h,
            },
            units_per_css_px: k,
        }
    }

    #[test]
    fn window_size_fits_bubble_and_cat() {
        assert_eq!(window_size_css(2), (256, 232));
        assert_eq!(window_size_css(1), (256, 184));
        // Cat wider than the bubble area.
        assert_eq!(window_size_css(8), (400, 520));
        // Scale 0 is invalid; treated as 1 rather than a zero-size cat.
        assert_eq!(window_size_css(0), window_size_css(1));
    }

    #[test]
    fn default_is_bottom_right_above_the_taskbar() {
        // 1920x1080 at 100%, 40 px taskbar at the bottom.
        let s = screen("A", 0.0, 0.0, 1920.0, 1040.0, 1.0);
        let p = place(&s, ScreenCorner::BottomRight, (256, 232));
        assert_eq!(
            p,
            Placement {
                x: 1920 - 8 - 256,
                y: 1040 - 8 - 232,
                width: 256,
                height: 232
            }
        );
    }

    #[test]
    fn all_corners() {
        let s = screen("A", 0.0, 0.0, 1000.0, 800.0, 1.0);
        let at = |c| {
            let p = place(&s, c, (100, 50));
            (p.x, p.y)
        };
        assert_eq!(at(ScreenCorner::TopLeft), (8, 8));
        assert_eq!(at(ScreenCorner::TopRight), (892, 8));
        assert_eq!(at(ScreenCorner::BottomLeft), (8, 742));
        assert_eq!(at(ScreenCorner::BottomRight), (892, 742));
    }

    #[test]
    fn work_area_offset_on_a_secondary_monitor() {
        // Monitor to the right of the primary, taskbar on its left (60 px).
        let s = screen("B", 1920.0 + 60.0, 0.0, 2560.0 - 60.0, 1440.0, 1.0);
        let p = place(&s, ScreenCorner::TopLeft, (256, 232));
        assert_eq!((p.x, p.y), (1920 + 60 + 8, 8));
        let p = place(&s, ScreenCorner::BottomRight, (256, 232));
        assert_eq!((p.x, p.y), (1920 + 2560 - 8 - 256, 1440 - 8 - 232));
    }

    #[test]
    fn negative_coordinates_left_of_and_above_the_primary() {
        let s = screen("L", -1280.0, -200.0, 1280.0, 984.0, 1.0);
        let p = place(&s, ScreenCorner::TopLeft, (256, 232));
        assert_eq!((p.x, p.y), (-1272, -192));
        let p = place(&s, ScreenCorner::BottomRight, (256, 232));
        assert_eq!((p.x, p.y), (-8 - 256, -200 + 984 - 8 - 232));
    }

    #[test]
    fn scale_factor_multiplies_size_and_margin() {
        // 4K at 150%: physical pixels, 1.5 units per CSS pixel.
        let s = screen("A", 0.0, 0.0, 3840.0, 2088.0, 1.5);
        let p = place(&s, ScreenCorner::BottomRight, (256, 232));
        assert_eq!(p.width, 384);
        assert_eq!(p.height, 348);
        assert_eq!((p.x, p.y), (3840 - 12 - 384, 2088 - 12 - 348));

        // 125%: fractional sizes round up, so the content is never clipped.
        let s = screen("A", 0.0, 0.0, 1536.0, 824.0, 1.25);
        let p = place(&s, ScreenCorner::TopLeft, (257, 233));
        assert_eq!((p.width, p.height), (322, 292));
        assert_eq!((p.x, p.y), (10, 10));
    }

    #[test]
    fn invalid_scale_falls_back_to_one() {
        let s = screen("A", 0.0, 0.0, 1000.0, 800.0, f64::NAN);
        assert_eq!(place(&s, ScreenCorner::TopLeft, (10, 10)).width, 10);
        let s = screen("A", 0.0, 0.0, 1000.0, 800.0, 0.0);
        assert_eq!(place(&s, ScreenCorner::TopLeft, (10, 10)).width, 10);
    }

    #[test]
    fn too_large_window_stays_inside_at_top_left() {
        let s = screen("A", 100.0, 50.0, 300.0, 200.0, 1.0);
        let p = place(&s, ScreenCorner::BottomRight, (400, 520));
        assert_eq!((p.x, p.y), (100, 50));
    }

    #[test]
    fn select_screen_by_name_with_primary_fallback() {
        let a = screen("A", 0.0, 0.0, 1920.0, 1040.0, 1.0);
        let b = screen("B", 1920.0, 0.0, 1920.0, 1080.0, 1.0);
        let screens = vec![a.clone(), b.clone()];

        assert_eq!(select_screen(&screens, Some(&b), None), Some(&b));
        assert_eq!(select_screen(&screens, Some(&a), Some("B")), Some(&b));
        // Configured monitor missing: primary.
        assert_eq!(select_screen(&screens, Some(&a), Some("Gone")), Some(&a));
        // No primary reported: first monitor.
        assert_eq!(select_screen(&screens, None, Some("Gone")), Some(&a));
        assert_eq!(select_screen(&[], None, None), None);
    }

    #[test]
    fn popover_size_per_corner() {
        assert_eq!(
            popover_window_size_css(2, ScreenCorner::BottomRight),
            (304, 472)
        );
        assert_eq!(
            popover_window_size_css(2, ScreenCorner::TopLeft),
            (304, 592)
        );
        assert_eq!(
            popover_window_size_css(8, ScreenCorner::BottomLeft),
            (400, 760)
        );
    }

    #[test]
    fn grown_window_keeps_the_cat_in_place() {
        let s = screen("A", 0.0, 0.0, 1920.0, 1040.0, 1.5);
        let normal = (256, 232);
        for corner in [
            ScreenCorner::BottomRight,
            ScreenCorner::BottomLeft,
            ScreenCorner::TopRight,
            ScreenCorner::TopLeft,
        ] {
            let n = place(&s, corner, normal);
            let g = place_grown(&s, corner, normal, popover_window_size_css(2, corner));
            // The cat hugs the corner side horizontally ...
            match corner {
                ScreenCorner::BottomRight | ScreenCorner::TopRight => {
                    assert_eq!(g.x + g.width as i32, n.x + n.width as i32)
                }
                _ => assert_eq!(g.x, n.x),
            }
            // ... and the bottom (bottom corners) or its offset from the
            // top (top corners) stays.
            match corner {
                ScreenCorner::BottomRight | ScreenCorner::BottomLeft => {
                    assert_eq!(g.y + g.height as i32, n.y + n.height as i32)
                }
                _ => assert_eq!(g.y, n.y),
            }
        }
    }

    #[test]
    fn grown_window_is_clamped_into_the_work_area() {
        let s = screen("A", 0.0, 0.0, 400.0, 300.0, 1.0);
        let g = place_grown(&s, ScreenCorner::BottomRight, (256, 232), (304, 472));
        assert_eq!((g.x, g.y), (88, 0));
    }
}
