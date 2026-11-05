//! S-1b spike: native overlay thread inside a Tauri v2 process.
//!
//! Timeline (printed with markers for external RAM measurement):
//!   t=0   Tauri runs with no windows; overlay thread renders at 60 FPS
//!   t=10  settings WebView window created
//!   t=20  settings window destroyed
//!   t=40  app exits
//! Flag `--hold` keeps the app alive (no auto exit) for manual testing.

#[path = "../../s1-overlay/src/overlay.rs"]
mod overlay;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};

fn overlay_thread(stop: Arc<AtomicBool>) {
    let rect = overlay::primary_monitor().work;
    let ov = match overlay::Overlay::create(rect, 90.0) {
        Ok(ov) => ov,
        Err(e) => {
            eprintln!("overlay create failed: {e}");
            return;
        }
    };
    let start = Instant::now();
    let mut frames = 0u32;
    let mut last = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: message pump for the window owned by this thread.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let t = start.elapsed().as_secs_f32();
        let bass = 0.55 + 0.45 * (t * std::f32::consts::TAU * 1.2).sin();
        if let Err(e) = ov.frame([bass, bass]) {
            eprintln!("frame failed: {e}");
            break;
        }
        frames += 1;
        if last.elapsed() >= Duration::from_secs(5) {
            println!("overlay fps={:.1}", frames as f32 / last.elapsed().as_secs_f32());
            frames = 0;
            last = Instant::now();
        }
    }
}

fn mark(start: Instant, what: &str) {
    println!("MARK t={:.1} {what}", start.elapsed().as_secs_f32());
}

fn main() {
    // SAFETY: once, before any window exists.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    println!("pid={}", std::process::id());
    let hold = std::env::args().any(|a| a == "--hold");
    let stop = Arc::new(AtomicBool::new(false));
    let ov_stop = stop.clone();
    let ov_handle = std::thread::spawn(move || overlay_thread(ov_stop));

    let app = tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let start = Instant::now();
                mark(start, "no-webview");
                std::thread::sleep(Duration::from_secs(10));
                let built = WebviewWindowBuilder::new(
                    &handle,
                    "settings",
                    WebviewUrl::App("index.html".into()),
                )
                .title("VibeWave settings (spike)")
                .inner_size(420.0, 320.0)
                .build();
                match built {
                    Ok(_) => mark(start, "settings-open"),
                    Err(e) => eprintln!("settings create failed: {e}"),
                }
                std::thread::sleep(Duration::from_secs(10));
                if let Some(w) = handle.get_webview_window("settings") {
                    let _ = w.destroy();
                }
                mark(start, "settings-destroyed");
                if !hold {
                    std::thread::sleep(Duration::from_secs(20));
                    mark(start, "exit");
                    handle.exit(0);
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            eprintln!("tauri build failed: {e}");
            return;
        }
    };
    app.run(|_app, event| {
        // Closing the last window must not quit the app: the overlay lives on.
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
    stop.store(true, Ordering::Relaxed);
    let _ = ov_handle.join();
}
