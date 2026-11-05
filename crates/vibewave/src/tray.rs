//! System tray icon and menu (PRD FR-14, FR-19, FR-20).
//!
//! Right-click menu:
//! Glow on (toggle) · Show over taskbar · Stay visible in fullscreen ·
//! Monitor ▸ · Open settings… · Run on startup · Hide tray icon · Exit VibeWave.
//! Left-clicking the icon opens settings. Tauri runs the menu callbacks on the
//! main thread, which is also where the settings window must be created.
//!
//! The actions ([`set_active`], [`set_tray_visible`], [`set_autostart`],
//! [`quit`]) are shared with the settings window, so a change made in one place
//! shows in the other: the menu is rebuilt from the real state ([`refresh`])
//! after every change and on right-click, and the settings page polls.

use std::sync::Arc;

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, Wry};

use crate::settings_state::SharedSettings;
use crate::{autostart, config, log, monitors, settings_ui};
use vibewave_core::settings::MONITOR_PRIMARY;

const TRAY_ID: &str = "main";
const ID_ACTIVE: &str = "active";
const ID_OVER_TASKBAR: &str = "over_taskbar";
const ID_STAY_VISIBLE: &str = "stay_visible";
const ID_SETTINGS: &str = "settings";
const ID_AUTOSTART: &str = "autostart";
const ID_HIDE_TRAY: &str = "hide_tray";
const ID_QUIT: &str = "quit";
/// Monitor items are `monitor:<display number>`; `monitor:0` = primary.
const MONITOR_PREFIX: &str = "monitor:";

/// Create the tray icon. Called once from Tauri's `setup`.
pub fn install(app: &App) -> tauri::Result<()> {
    let menu = build_menu(app.handle())?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("VibeWave")
        .menu(&menu)
        .show_menu_on_left_click(false);
    match app.default_window_icon() {
        Some(icon) => builder = builder.icon(icon.clone()),
        None => log::warn("tray: no app icon found; tray icon may be blank"),
    }

    let tray = builder
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => open_settings(tray.app_handle()),
            // The menu opens on button-up; refresh it on button-down so the
            // monitor list and check marks are current.
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Down,
                ..
            } => refresh(tray.app_handle()),
            _ => {}
        })
        .build(app)?;

    let shared = app.state::<Arc<SharedSettings>>();
    if !shared.get().settings.show_tray_icon {
        log::info("tray: icon hidden by settings (launch VibeWave again to open settings)");
        tray.set_visible(false)?;
    }
    Ok(())
}

/// Rebuild the tray menu from the current state. Call after any change made
/// elsewhere (e.g. a setting changed in the settings window).
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match build_menu(app) {
        Ok(menu) => {
            if let Err(e) = tray.set_menu(Some(menu)) {
                log::warn(&format!("tray: cannot update menu: {e}"));
            }
        }
        Err(e) => log::warn(&format!("tray: cannot build menu: {e}")),
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let live = app.state::<Arc<SharedSettings>>().get();
    let s = live.settings;
    let check = |id: &str, text: &str, on: bool| {
        CheckMenuItem::with_id(app, id, text, true, on, None::<&str>)
    };

    let active = check(ID_ACTIVE, "Glow on", live.active)?;
    let over_taskbar = check(ID_OVER_TASKBAR, "Show over taskbar", s.over_taskbar)?;
    let stay_visible = check(
        ID_STAY_VISIBLE,
        "Stay visible in fullscreen",
        s.stay_visible_fullscreen,
    )?;

    // Monitor ▸ : "Primary monitor", then every attached display.
    let mut monitor_items = vec![check(
        &format!("{MONITOR_PREFIX}{MONITOR_PRIMARY}"),
        "Primary monitor",
        s.monitor == MONITOR_PRIMARY,
    )?];
    for m in monitors::list().iter().filter(|m| m.number != 0) {
        monitor_items.push(check(
            &format!("{MONITOR_PREFIX}{}", m.number),
            &monitors::label(m),
            s.monitor == m.number,
        )?);
    }
    let refs: Vec<&dyn IsMenuItem<Wry>> = monitor_items
        .iter()
        .map(|i| i as &dyn IsMenuItem<Wry>)
        .collect();
    let monitor = Submenu::with_items(app, "Monitor", true, &refs)?;

    let settings = MenuItem::with_id(app, ID_SETTINGS, "Open settings…", true, None::<&str>)?;
    let autostart_item = check(ID_AUTOSTART, "Run on startup", autostart::is_enabled())?;
    let hide_tray = MenuItem::with_id(app, ID_HIDE_TRAY, "Hide tray icon", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "Exit VibeWave", true, None::<&str>)?;
    Menu::with_items(
        app,
        &[
            &active,
            &over_taskbar,
            &stay_visible,
            &monitor,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &autostart_item,
            &hide_tray,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )
}

fn on_menu(app: &AppHandle, id: &str) {
    let live = app.state::<Arc<SharedSettings>>();
    let now = live.get();
    match id {
        ID_ACTIVE => set_active(app, !now.active),
        ID_OVER_TASKBAR => {
            live.set_over_taskbar(!now.settings.over_taskbar);
            save_and_refresh(app);
        }
        ID_STAY_VISIBLE => {
            live.set_stay_visible_fullscreen(!now.settings.stay_visible_fullscreen);
            save_and_refresh(app);
        }
        ID_SETTINGS => open_settings(app),
        ID_AUTOSTART => {
            if let Err(e) = set_autostart(app, !autostart::is_enabled()) {
                log::warn(&format!("tray: {e}"));
            }
        }
        ID_HIDE_TRAY => set_tray_visible(app, false),
        ID_QUIT => quit(app),
        other => {
            if let Some(n) = other
                .strip_prefix(MONITOR_PREFIX)
                .and_then(|n| n.parse::<u32>().ok())
            {
                live.set_monitor(n);
                log::info(&format!("tray: monitor {n}"));
                save_and_refresh(app);
            }
        }
    }
}

fn save_and_refresh(app: &AppHandle) {
    config::save_logged(&app.state::<Arc<SharedSettings>>().get().settings);
    refresh(app);
}

/// Switch the glow on or off without quitting. Runtime only: a restart always
/// starts with the glow on.
pub fn set_active(app: &AppHandle, on: bool) {
    app.state::<Arc<SharedSettings>>().set_active(on);
    log::info(&format!("glow {}", if on { "on" } else { "off" }));
    refresh(app);
}

/// Show or hide the tray icon and remember the choice. While hidden, launching
/// VibeWave again opens the settings window (single instance), where the icon
/// can be turned back on.
pub fn set_tray_visible(app: &AppHandle, on: bool) {
    app.state::<Arc<SharedSettings>>().set_show_tray_icon(on);
    config::save_logged(&app.state::<Arc<SharedSettings>>().get().settings);
    match app.tray_by_id(TRAY_ID) {
        Some(tray) => {
            if let Err(e) = tray.set_visible(on) {
                log::warn(&format!("tray: cannot change visibility: {e}"));
            }
        }
        None => log::warn("tray: icon not found"),
    }
    log::info(&format!(
        "tray icon {}",
        if on { "shown" } else { "hidden" }
    ));
}

/// Turn "Start with Windows" on or off. The registry is the source of truth,
/// so the menu re-reads it afterwards.
pub fn set_autostart(app: &AppHandle, on: bool) -> Result<(), String> {
    let result = autostart::set(on).map_err(|e| format!("cannot change autostart: {e}"));
    if result.is_ok() {
        log::info(&format!("autostart {}", if on { "on" } else { "off" }));
    }
    refresh(app);
    result
}

/// Exit the app. FR-20: settings stay saved; autostart is left as it is.
pub fn quit(app: &AppHandle) {
    config::save_logged(&app.state::<Arc<SharedSettings>>().get().settings);
    log::info("quit");
    app.exit(0);
}

fn open_settings(app: &AppHandle) {
    if let Err(e) = settings_ui::open_settings_window(app) {
        log::warn(&format!("cannot open settings window: {e}"));
    }
}
