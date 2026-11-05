//! VibeWave: audio-reactive edge glow for Windows.
//!
//! Startup, in order:
//! 1. Parse args; exit early if VibeWave already runs (FR-18); load settings.
//! 2. Start the **overlay** on its own thread (its Win32 window + render loop).
//! 3. Start the **audio** thread (WASAPI capture → band levels).
//! 4. Run the **Tauri event loop** on the main thread; it hosts the tray icon
//!    and the settings window, which is created on demand and destroyed on
//!    close (FR-17).
//!
//! Keeping the overlay off the main thread is the same layout the S-1b spike
//! proved works inside a Tauri process. The two sides share settings through
//! `SharedSettings`, so a change in the window is visible in the glow next
//! frame (FR-15).
//!
//! See `specs/001-audio-reactive-glow/` for the requirements this implements.

mod audio;
mod autostart;
mod cli;
mod clock;
mod config;
mod log;
mod media;
mod monitors;
mod overlay;
mod settings_state;
mod settings_ui;
mod single_instance;
mod stats;
mod tray;

use std::sync::atomic::Ordering;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (options, warnings) = cli::parse(&args);
    for w in warnings {
        log::warn(&w);
    }

    // FR-18: one VibeWave per session. A manual second launch asks the running
    // one to open settings; a login autostart just exits quietly.
    let listener = match single_instance::claim(!options.autostart) {
        Some(single_instance::Claim::Secondary) => {
            log::info("VibeWave is already running; exiting");
            return;
        }
        Some(single_instance::Claim::Primary(l)) => Some(l),
        None => None,
    };

    // Measurement runs (`--seconds N`) skip first-run side effects so they
    // never touch the registry or create the settings file.
    let first_run = options.seconds.is_none() && config::is_first_run();
    let settings = config::load();
    log::info(&format!("pid={}", std::process::id()));

    let live = Arc::new(settings_state::SharedSettings::new(settings));

    // Audio on its own thread.
    let audio = audio::spawn(settings.sensitivity);

    // Media watcher: publishes the cover-art palette when a track changes.
    media::spawn(Arc::clone(&live));

    // Overlay on its own thread; it reads `live` every frame.
    let overlay_live = Arc::clone(&live);
    let overlay_levels = Arc::clone(&audio.levels);
    let overlay = std::thread::Builder::new()
        .name("vibewave-overlay".into())
        .spawn(move || {
            if let Err(e) =
                overlay::run(overlay_live, overlay_levels, options.seconds, options.stats)
            {
                log::error(&format!("overlay failed: {e}"));
                // Signal the app to exit if the overlay cannot run at all.
                std::process::exit(1);
            }
        });
    let overlay = match overlay {
        Ok(h) => h,
        Err(e) => {
            log::error(&format!("cannot spawn overlay thread: {e}"));
            std::process::exit(1);
        }
    };

    // Tauri owns the main thread. Build and run its event loop.
    let app = match settings_ui::build_app(Arc::clone(&live), Arc::clone(&audio)) {
        Ok(app) => app,
        Err(e) => {
            log::error(&format!("cannot start settings UI: {e}"));
            std::process::exit(1);
        }
    };
    let handle = app.handle().clone();

    // A second launch signals us; open settings on the main thread, where
    // Tauri windows must be created.
    if let Some(listener) = listener {
        let h = handle.clone();
        listener.spawn(move || {
            let main_h = h.clone();
            let posted = h.run_on_main_thread(move || {
                if let Err(e) = settings_ui::open_settings_window(&main_h) {
                    log::warn(&format!("cannot open settings window: {e}"));
                }
            });
            if let Err(e) = posted {
                log::warn(&format!("cannot reach main thread: {e}"));
            }
        });
    }

    if first_run {
        // FR-19: autostart defaults to on, set once. Later runs leave it alone
        // so turning it off in the tray sticks.
        match autostart::set(true) {
            Ok(()) => log::info("first run: autostart enabled"),
            Err(e) => log::warn(&format!("first run: cannot enable autostart: {e}")),
        }
        // Writing the file records that the first run happened (FR-18).
        config::save_logged(&settings);
    }

    // FR-18: settings open once on the very first manual run, or on request.
    // Never at login autostart.
    if options.settings || (first_run && !options.autostart) {
        if let Err(e) = settings_ui::open_settings_window(&handle) {
            log::warn(&format!("cannot open settings window: {e}"));
        }
    }

    // When the overlay loop ends (e.g. `--seconds N` elapsed), exit the Tauri
    // event loop so the process finishes cleanly.
    {
        let exit_handle = handle.clone();
        std::thread::Builder::new()
            .name("vibewave-exit-watch".into())
            .spawn(move || {
                let _ = overlay.join();
                exit_handle.exit(0);
            })
            .ok();
    }

    // Run until the event loop exits. Closing the last (settings) window asks
    // Tauri to exit with `code: None`; VibeWave lives in the tray, so refuse
    // that (FR-17). `app.exit(n)` from tray Quit carries `Some(n)` and goes
    // through (FR-20).
    app.run(|_app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });

    // The Tauri loop has ended; ask the audio thread to stop.
    audio.running.store(false, Ordering::Relaxed);
    log::info("exited cleanly");
}
