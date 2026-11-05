//! S-1a spike runner. See docs/spikes/S-1-overlay.md for the protocol.
//!
//! Flags:
//!   --seconds N       run time (default 20)
//!   --over-taskbar    cover the full monitor instead of the work area (FR-4 option)
//!   --reassert        re-assert HWND_TOPMOST once per second (taskbar z-order test)
//!   --idle-after N    stop rendering after N seconds (0 FPS idle measurement)
//!   --thickness PX    glow thickness in pixels (default 90)

mod overlay;

use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetAncestor, GetForegroundWindow, GetWindowLongW, MsgWaitForMultipleObjects,
    PeekMessageW, TranslateMessage, WindowFromPoint, GA_ROOT, GWL_EXSTYLE, MSG, PM_REMOVE,
    QS_ALLINPUT, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT,
};

fn arg<T: std::str::FromStr>(name: &str, default: T) -> T {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn pump() -> bool {
    // SAFETY: standard message pump on the thread that owns the window.
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

fn hsv(h: f32) -> [f32; 3] {
    let f = |n: f32| {
        let k = (n + h * 6.0) % 6.0;
        1.0 - k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [f(5.0), f(3.0), f(1.0)]
}

fn main() -> windows::core::Result<()> {
    let seconds: u64 = arg("--seconds", 20);
    let idle_after: u64 = arg("--idle-after", u64::MAX);
    let thickness: f32 = arg("--thickness", 90.0);

    // SAFETY: called once at startup before any window exists.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let mon = overlay::primary_monitor();
    let rect = if flag("--over-taskbar") { mon.full } else { mon.work };
    println!(
        "pid={} monitor={:?} work={:?} using={:?}",
        std::process::id(),
        (mon.full.left, mon.full.top, mon.full.right, mon.full.bottom),
        (mon.work.left, mon.work.top, mon.work.right, mon.work.bottom),
        (rect.left, rect.top, rect.right, rect.bottom)
    );

    // SAFETY: read-only query.
    let fg_before = unsafe { GetForegroundWindow() };
    let mut ov = overlay::Overlay::create(rect, thickness)?;
    ov.frame([1.0, 1.0])?;
    ov.commit()?;

    self_test(&ov, fg_before, rect);

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut last_reassert = Instant::now();
    let mut frames = 0u32;
    let mut worst = Duration::ZERO;
    let mut last_frame = Instant::now();
    let mut idle = false;
    while start.elapsed() < Duration::from_secs(seconds) && pump() {
        let t = start.elapsed().as_secs_f32();
        if flag("--reassert") && last_reassert.elapsed() >= Duration::from_secs(1) {
            ov.reassert_topmost();
            last_reassert = Instant::now();
        }
        if start.elapsed() >= Duration::from_secs(idle_after) {
            if !idle {
                ov.clear()?;
                idle = true;
                println!("idle: rendering stopped at t={t:.1}s");
            }
            // Idle: block on window messages only, with a timeout. No frames.
            // SAFETY: no handles; waits on the thread message queue.
            unsafe {
                MsgWaitForMultipleObjects(None, false, 250, QS_ALLINPUT);
            }
            continue;
        }
        // Fake "audio": bass pulse on top/bottom, faster shimmer on left/right.
        let bass = 0.55 + 0.45 * (t * std::f32::consts::TAU * 1.2).sin();
        let treble = 0.6 + 0.4 * (t * std::f32::consts::TAU * 5.0).sin();
        if frames % 30 == 0 {
            ov.set_color(hsv((t * 0.05) % 1.0))?;
        }
        ov.frame([bass, treble])?;
        frames += 1;
        let dt = last_frame.elapsed();
        last_frame = Instant::now();
        if dt > worst {
            worst = dt;
        }
        if last_report.elapsed() >= Duration::from_secs(5) {
            let secs = last_report.elapsed().as_secs_f32();
            println!(
                "fps={:.1} worst_frame_ms={:.1}",
                frames as f32 / secs,
                worst.as_secs_f64() * 1000.0
            );
            frames = 0;
            worst = Duration::ZERO;
            last_report = Instant::now();
        }
    }
    drop(ov);
    println!("done");
    Ok(())
}

/// Programmatic checks for the S-1 criteria that can be observed from code.
fn self_test(ov: &overlay::Overlay, fg_before: HWND, rect: windows::Win32::Foundation::RECT) {
    // SAFETY: read-only Win32 queries.
    unsafe {
        let fg_after = GetForegroundWindow();
        println!("check focus_unchanged={}", fg_before == fg_after);

        let ex = GetWindowLongW(ov.hwnd, GWL_EXSTYLE) as u32;
        let has = |s: u32| ex & s == s;
        println!(
            "check exstyle layered={} transparent={} noactivate={} toolwindow={} topmost={}",
            has(WS_EX_LAYERED.0),
            has(WS_EX_TRANSPARENT.0),
            has(WS_EX_NOACTIVATE.0),
            has(WS_EX_TOOLWINDOW.0),
            has(WS_EX_TOPMOST.0)
        );

        // Hit-test points inside the glow (each edge). Click-through means the
        // window found at that point is NOT the overlay.
        let mid_x = (rect.left + rect.right) / 2;
        let mid_y = (rect.top + rect.bottom) / 2;
        let points = [
            ("top", POINT { x: mid_x, y: rect.top + 5 }),
            ("bottom", POINT { x: mid_x, y: rect.bottom - 5 }),
            ("left", POINT { x: rect.left + 5, y: mid_y }),
            ("right", POINT { x: rect.right - 5, y: mid_y }),
        ];
        for (name, p) in points {
            let hit = GetAncestor(WindowFromPoint(p), GA_ROOT);
            println!("check click_through_{name}={}", hit != ov.hwnd);
        }
    }
}
