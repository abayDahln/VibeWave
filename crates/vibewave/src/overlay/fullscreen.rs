//! Fullscreen detection on the target monitor (PRD FR-13).
//!
//! Method from spike S-4: the foreground window counts as fullscreen when it
//! is visible, not minimised, not maximised, not the desktop/shell, sits on the
//! target monitor, and its window rectangle fully covers that monitor. This is
//! what catches borderless fullscreen (F11 in a browser, most modern games);
//! `SHQueryUserNotificationState` does not, so it is not used.
//!
//! `IsZoomed` (maximised) is excluded because a maximised window on a monitor
//! with auto-hide taskbar and no appbars covers the whole monitor too, which
//! would otherwise hide the glow for an ordinary maximised app (spike S-4 risk).
//!
//! Every query is a handful of read-only Win32 calls (<1 ms measured), so the
//! render loop can poll it cheaply.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible, IsZoomed,
};

use vibewave_core::geometry::{covers, Rect};

/// Shell window classes that cover the screen but are not apps.
const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// True when a fullscreen app currently covers `monitor`.
///
/// `monitor` is the full monitor rectangle in physical pixels.
pub fn covers_monitor(monitor: Rect) -> bool {
    // SAFETY: read-only queries on the current foreground window; the HWND is
    // only used within this call and every out-param is a valid local.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return false;
        }
        if !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() || IsZoomed(fg).as_bool() {
            return false;
        }
        if SHELL_CLASSES.contains(&class_name(fg).as_str()) {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        // Only the monitor we draw on matters; a fullscreen app on another
        // screen must not hide the glow.
        if !on_monitor(fg, monitor) {
            return false;
        }
        covers(crate::monitors::to_core(r), monitor)
    }
}

/// Is the window's nearest monitor the one we draw on? Compared by rectangle,
/// because `HMONITOR` handles are not stable across display changes.
unsafe fn on_monitor(hwnd: HWND, monitor: Rect) -> bool {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    // SAFETY: valid foreground HWND; correctly sized out-struct.
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(hmon, &mut mi).as_bool()
            && crate::monitors::to_core(mi.rcMonitor) == monitor
    }
}

/// Window class name of `hwnd` (empty on failure).
unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer is valid for its full length.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}
