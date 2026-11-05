//! Attached monitors (PRD FR-14 "Monitor ▸", multi-monitor target choice).
//!
//! `EnumDisplayMonitors` calls us back once per monitor; we read each one's
//! rectangles and GDI device name (`\\.\DISPLAYn`) with `GetMonitorInfoW`.
//! The display number `n` is what the settings file stores, because it stays
//! the same while the monitor is plugged into the same port, unlike `HMONITOR`
//! handles which change whenever the display setup changes.
//!
//! Safe to call from any thread; it only queries the window manager.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

use vibewave_core::geometry::{display_number, MonitorInfo, Rect};

/// List attached monitors in the order Windows reports them.
pub fn list() -> Vec<MonitorInfo> {
    let mut out: Vec<MonitorInfo> = Vec::new();
    // SAFETY: the callback runs synchronously inside EnumDisplayMonitors, so
    // the `&mut out` pointer we pass through LPARAM is valid for every call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM(&mut out as *mut Vec<MonitorInfo> as isize),
        );
    }
    out
}

unsafe extern "system" fn enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&mut Vec` passed by `list`, alive for the call.
    let out = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };
    let mut mi = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: MONITORINFOEXW starts with MONITORINFO and cbSize tells Windows
    // the larger struct is available, which is the documented way to get
    // the device name.
    let ok = unsafe { GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) };
    if ok.as_bool() {
        let len = mi
            .szDevice
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(mi.szDevice.len());
        let name = String::from_utf16_lossy(&mi.szDevice[..len]);
        let info = mi.monitorInfo;
        out.push(MonitorInfo {
            number: display_number(&name).unwrap_or(0),
            primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            full: to_core(info.rcMonitor),
            work: to_core(info.rcWork),
        });
    }
    // Keep enumerating.
    BOOL(1)
}

/// Convert a Win32 `RECT` into the core rect type.
pub fn to_core(r: RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Human label for menus: `Display 2 — 2560×1440 (primary)`.
pub fn label(m: &MonitorInfo) -> String {
    format!(
        "Display {} \u{2014} {}\u{d7}{}{}",
        m.number,
        m.full.width(),
        m.full.height(),
        if m.primary { " (primary)" } else { "" }
    )
}
