//! S-4 spike: is a fullscreen app covering the target (primary) monitor?
//!
//! Method A (geometry): the foreground window, excluding the desktop/shell,
//! has a window rect that fully covers the monitor rect. This catches
//! borderless fullscreen (browser video, F11, most modern games' "fullscreen").
//! Method B: SHQueryUserNotificationState, the shell's own "busy" signal.
//!
//! Flags:
//!   --seconds N     run time (default 30)
//!   --test-window   instead: show a borderless window covering the primary
//!                   monitor for --seconds, to act as the fullscreen app.

use std::time::{Duration, Instant};
use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClassNameW, GetForegroundWindow,
    GetWindowRect, IsIconic, IsWindowVisible, PeekMessageW, RegisterClassExW, SetForegroundWindow,
    ShowWindow, TranslateMessage, MSG, PM_REMOVE, SW_SHOW, WNDCLASSEXW, WS_EX_APPWINDOW, WS_POPUP,
    WS_VISIBLE,
};

fn monitor_rect(h: HMONITOR) -> RECT {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: correctly sized out-struct.
    unsafe {
        let _ = GetMonitorInfoW(h, &mut mi);
    }
    mi.rcMonitor
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: buffer is valid for its length.
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Method A. Returns (is_fullscreen, foreground class name).
fn geometry_fullscreen(target: HMONITOR) -> (bool, String) {
    // SAFETY: read-only Win32 queries on the current foreground window.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return (false, "<none>".into());
        }
        let cls = class_name(fg);
        // The desktop and shell windows also cover the screen; never count them.
        if matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return (false, cls);
        }
        if !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return (false, cls);
        }
        if MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) != target {
            return (false, cls);
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return (false, cls);
        }
        let m = monitor_rect(target);
        let covers = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
        (covers, cls)
    }
}

/// Method B.
fn shell_state() -> String {
    // SAFETY: plain query.
    match unsafe { SHQueryUserNotificationState() } {
        Ok(QUERY_USER_NOTIFICATION_STATE(s)) => match s {
            1 => "NOT_PRESENT".into(),
            2 => "BUSY".into(),
            3 => "RUNNING_D3D_FULL_SCREEN".into(),
            4 => "PRESENTATION_MODE".into(),
            5 => "ACCEPTS_NOTIFICATIONS".into(),
            6 => "QUIET_TIME".into(),
            7 => "APP".into(),
            n => format!("UNKNOWN({n})"),
        },
        Err(e) => format!("err({e})"),
    }
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

fn test_window(seconds: u64) -> Result<()> {
    // SAFETY: standard window creation and message pump on this thread.
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: w!("VibeWaveSpikeFullscreenTest"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let m = monitor_rect(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY));
        let h = CreateWindowExW(
            WS_EX_APPWINDOW,
            w!("VibeWaveSpikeFullscreenTest"),
            w!("fullscreen test"),
            WS_POPUP | WS_VISIBLE,
            m.left,
            m.top,
            m.right - m.left,
            m.bottom - m.top,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(seconds) {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let seconds: u64 = std::env::args()
        .skip_while(|a| a != "--seconds")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    // SAFETY: once at startup.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if std::env::args().any(|a| a == "--test-window") {
        return test_window(seconds);
    }
    println!("pid={}", std::process::id());
    // SAFETY: plain query.
    let target = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let start = Instant::now();
    let mut last = (false, String::new(), String::new());
    let mut worst_us = 0u128;
    while start.elapsed() < Duration::from_secs(seconds) {
        let tq = Instant::now();
        let (fs, cls) = geometry_fullscreen(target);
        let shell = shell_state();
        worst_us = worst_us.max(tq.elapsed().as_micros());
        let now = (fs, cls, shell);
        if now != last {
            println!(
                "t={:.2} geometry_fullscreen={} fg_class={:?} shell_state={}",
                start.elapsed().as_secs_f32(),
                now.0,
                now.1,
                now.2
            );
            last = now;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("done worst_query_us={worst_us}");
    Ok(())
}
