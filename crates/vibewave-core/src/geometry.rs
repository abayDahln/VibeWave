//! Overlay geometry: which rectangle the glow covers and how thick it is.
//!
//! The overlay window is positioned over a monitor. By default it covers the
//! monitor's *work area* (`rcWork`: the screen minus the taskbar and any other
//! appbar), so the taskbar stays clean. With `over_taskbar` the full monitor
//! rectangle (`rcMonitor`) is used instead.

/// An integer rectangle in physical pixels, in the Win32 `RECT` convention:
/// `right`/`bottom` are exclusive (a 1920-wide rect from x=0 has `right = 1920`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// A rect is usable as an overlay target only if it has positive area.
    pub fn is_valid(&self) -> bool {
        self.width() > 0 && self.height() > 0
    }
}

/// Choose the rectangle the overlay should cover.
///
/// `over_taskbar == false` prefers the work area; `true` prefers the full
/// monitor. A work area with no area (some remote/virtual setups report an
/// empty `rcWork`) falls back to the full monitor so the overlay is never
/// zero-sized.
pub fn overlay_rect(monitor: Rect, work: Rect, over_taskbar: bool) -> Rect {
    if over_taskbar {
        monitor
    } else if work.is_valid() {
        work
    } else {
        monitor
    }
}

/// Scale a 96-DPI design size to the monitor's actual DPI.
///
/// A value of 24 px at 96 DPI becomes 36 px at 144 DPI (150% scaling), so the
/// glow keeps the same physical thickness on every display.
pub fn scale_px(px_96: f32, dpi: u32) -> f32 {
    px_96 * dpi as f32 / 96.0
}

/// One attached monitor, as the overlay needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Windows display number (`n` in `\\.\DISPLAYn`); 0 if unknown.
    pub number: u32,
    pub primary: bool,
    /// Full monitor rectangle.
    pub full: Rect,
    /// Work area (monitor minus taskbar/appbars).
    pub work: Rect,
}

/// Pick the monitor to draw on.
///
/// `wanted` is a display number, or [`crate::settings::MONITOR_PRIMARY`] (0)
/// for "the primary monitor". When the wanted display is not attached (e.g. it
/// was unplugged) the primary monitor is used, then the first one listed, so
/// the glow never disappears just because a cable was pulled. Returns `None`
/// only for an empty list.
pub fn pick_monitor(monitors: &[MonitorInfo], wanted: u32) -> Option<usize> {
    let by_number = || {
        (wanted != crate::settings::MONITOR_PRIMARY)
            .then(|| monitors.iter().position(|m| m.number == wanted))
            .flatten()
    };
    by_number()
        .or_else(|| monitors.iter().position(|m| m.primary))
        .or(if monitors.is_empty() { None } else { Some(0) })
}

/// Parse the display number from a GDI device name such as `\\.\DISPLAY2`.
pub fn display_number(device: &str) -> Option<u32> {
    let digits = device.trim().strip_prefix(r"\\.\DISPLAY")?;
    digits.parse().ok().filter(|n| *n > 0)
}

/// True when `window` fully covers `monitor` — the geometry test for
/// "a fullscreen app is on this monitor" (spike S-4, method A).
pub fn covers(window: Rect, monitor: Rect) -> bool {
    monitor.is_valid()
        && window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(number: u32, primary: bool) -> MonitorInfo {
        let r = rect(0, 0, 100, 100);
        MonitorInfo {
            number,
            primary,
            full: r,
            work: r,
        }
    }

    #[test]
    fn pick_monitor_prefers_wanted_then_primary_then_first() {
        let list = [mon(1, false), mon(2, true), mon(3, false)];
        assert_eq!(pick_monitor(&list, 3), Some(2));
        // 0 = primary.
        assert_eq!(pick_monitor(&list, 0), Some(1));
        // Unplugged display → primary.
        assert_eq!(pick_monitor(&list, 7), Some(1));
        // No primary flagged → first.
        assert_eq!(pick_monitor(&[mon(4, false), mon(5, false)], 9), Some(0));
        assert_eq!(pick_monitor(&[], 0), None);
    }

    #[test]
    fn display_number_parses_gdi_names() {
        assert_eq!(display_number(r"\\.\DISPLAY1"), Some(1));
        assert_eq!(display_number(r"\\.\DISPLAY12"), Some(12));
        assert_eq!(display_number(r"\\.\DISPLAY"), None);
        assert_eq!(display_number(r"\\.\DISPLAY0"), None);
        assert_eq!(display_number("HDMI"), None);
    }

    #[test]
    fn covers_detects_fullscreen_geometry() {
        let m = rect(1920, 0, 4480, 1440);
        // Exactly the monitor, or larger (some apps overhang by a few px).
        assert!(covers(m, m));
        assert!(covers(rect(1912, -8, 4488, 1448), m));
        // A window smaller than the monitor (e.g. leaves the taskbar) is not.
        assert!(!covers(rect(1920, 0, 4480, 1400), m));
        // A window on another monitor is not.
        assert!(!covers(rect(0, 0, 1920, 1080), m));
        assert!(!covers(m, rect(0, 0, 0, 0)));
    }

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn rect_width_height() {
        let r = rect(0, 32, 1920, 1200);
        assert_eq!(r.width(), 1920);
        assert_eq!(r.height(), 1168);
        assert!(r.is_valid());
    }

    #[test]
    fn zero_or_negative_area_is_invalid() {
        assert!(!rect(0, 0, 0, 100).is_valid());
        assert!(!rect(0, 0, 100, 0).is_valid());
        assert!(!rect(100, 100, 50, 50).is_valid());
    }

    #[test]
    fn default_uses_work_area() {
        let monitor = rect(0, 0, 1920, 1200);
        let work = rect(0, 32, 1920, 1200);
        assert_eq!(overlay_rect(monitor, work, false), work);
    }

    #[test]
    fn over_taskbar_uses_monitor() {
        let monitor = rect(0, 0, 1920, 1200);
        let work = rect(0, 32, 1920, 1200);
        assert_eq!(overlay_rect(monitor, work, true), monitor);
    }

    #[test]
    fn invalid_work_falls_back_to_monitor() {
        let monitor = rect(0, 0, 1920, 1200);
        let work = rect(0, 0, 0, 0);
        assert_eq!(overlay_rect(monitor, work, false), monitor);
    }

    #[test]
    fn scale_px_scales_by_dpi() {
        assert_eq!(scale_px(24.0, 96), 24.0);
        assert_eq!(scale_px(24.0, 144), 36.0);
        assert_eq!(scale_px(10.0, 120), 12.5);
    }
}
