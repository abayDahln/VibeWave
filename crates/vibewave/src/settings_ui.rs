//! The settings window, hosted by Tauri (FR-15, FR-17).
//!
//! The webview is created when the user opens settings and destroyed when it
//! closes, so background RAM returns to the overlay-only level (FR-17). The
//! window never opens automatically at startup (FR-18) — it is created on
//! demand by [`open_settings_window`].
//!
//! The frontend (`ui/index.html`) calls the commands below. Each command writes
//! to `SharedSettings`, which the overlay render thread reads every frame, so
//! changes appear immediately (FR-15).

use std::sync::Arc;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::audio;
use crate::config;
use crate::monitors;
use crate::settings_state::SharedSettings;
use vibewave_core::bars::{BarColorMode, Position};
use vibewave_core::settings::{to_hex_color, ColorMode, Theme, MONITOR_PRIMARY};
use vibewave_core::visual::Direction;

/// Label of the settings window (also its identifier for `get_webview_window`).
pub const SETTINGS_LABEL: &str = "settings";

/// A settings value sent to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SettingsDto {
    pub direction: String,
    pub thickness_px: f32,
    pub intensity: f32,
    pub sensitivity: f32,
    pub over_taskbar: bool,
    pub color_mode: String,
    pub custom_color: String,
    /// Hex of the palette currently painted (cover art, custom, or default).
    pub active_color: String,
    /// True when the active colour comes from cover art (for the UI hint).
    pub active_from_cover: bool,
    /// FR-13: keep the glow over fullscreen apps.
    pub stay_visible_fullscreen: bool,
    /// Target display number; 0 = primary.
    pub monitor: u32,
    /// Glow switched on (runtime only; the tray "Glow on" item).
    pub active: bool,
    /// Tray icon shown.
    pub show_tray_icon: bool,
    /// "Start with Windows" (read from the registry).
    pub autostart: bool,
    /// Visual theme: `glow` or `bars`.
    pub theme: String,
    /// Bars theme: `all`, `horizontal`, or `vertical`.
    pub bars_position: String,
    pub bars_thickness_px: f32,
    pub bars_max_len_frac: f32,
    pub bars_center_profile: f32,
    pub bars_beat_response: f32,
    /// Bars theme: `off`, `perbar`, or `position`.
    pub bars_color_mode: String,
    /// Bars theme: rounded bar ends.
    pub bars_rounded: bool,
}

/// One entry of the monitor dropdown.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MonitorDto {
    /// Display number (0 = "Primary monitor").
    pub number: u32,
    pub label: String,
}

impl SettingsDto {
    fn from_live(live: crate::settings_state::LiveSettings) -> Self {
        Self {
            direction: live.settings.direction.id().to_string(),
            thickness_px: live.settings.thickness_px,
            intensity: live.settings.intensity,
            sensitivity: live.settings.sensitivity,
            over_taskbar: live.settings.over_taskbar,
            color_mode: live.settings.color_mode.id().to_string(),
            custom_color: to_hex_color(live.settings.custom_color),
            active_color: to_hex_color(live.palette.at(0.0)),
            active_from_cover: live.palette_from_cover,
            stay_visible_fullscreen: live.settings.stay_visible_fullscreen,
            monitor: live.settings.monitor,
            active: live.active,
            show_tray_icon: live.settings.show_tray_icon,
            autostart: crate::autostart::is_enabled(),
            theme: live.settings.theme.id().to_string(),
            bars_position: live.settings.bars_position.id().to_string(),
            bars_thickness_px: live.settings.bars_thickness_px,
            bars_max_len_frac: live.settings.bars_max_len_frac,
            bars_center_profile: live.settings.bars_center_profile,
            bars_beat_response: live.settings.bars_beat_response,
            bars_color_mode: live.settings.bars_color_mode.id().to_string(),
            bars_rounded: live.settings.bars_rounded,
        }
    }
}

/// Read the current settings for the frontend.
#[tauri::command]
fn get_settings(state: tauri::State<'_, Arc<SharedSettings>>) -> SettingsDto {
    SettingsDto::from_live(state.get())
}

/// Change the glow rotation direction.
#[tauri::command]
fn set_direction(
    state: tauri::State<'_, Arc<SharedSettings>>,
    direction: String,
) -> Result<(), String> {
    match Direction::from_id(&direction) {
        Some(d) => {
            state.set_direction(d);
            Ok(())
        }
        None => Err(format!("unknown direction '{direction}'")),
    }
}

/// Change the glow thickness (96-DPI pixels).
#[tauri::command]
fn set_thickness(state: tauri::State<'_, Arc<SharedSettings>>, px: f32) {
    state.set_thickness(px);
}

/// Change the glow intensity.
#[tauri::command]
fn set_intensity(state: tauri::State<'_, Arc<SharedSettings>>, value: f32) {
    state.set_intensity(value);
}

/// Change how strongly the glow reacts to audio. Stored in the settings and
/// forwarded to the audio thread, which applies it on its next poll (~10 ms).
#[tauri::command]
fn set_sensitivity(
    state: tauri::State<'_, Arc<SharedSettings>>,
    audio: tauri::State<'_, Arc<audio::Shared>>,
    value: f32,
) {
    let stored = state.set_sensitivity(value);
    audio.set_sensitivity(stored);
}

/// Persist the current settings (FR-16). The UI calls this when a control
/// is committed (`change`), not on every slider tick.
#[tauri::command]
fn save_settings(state: tauri::State<'_, Arc<SharedSettings>>) -> Result<(), String> {
    config::save(&state.get().settings).map_err(|e| format!("cannot save settings: {e}"))
}

/// Toggle "glow over the taskbar" (FR-4).
#[tauri::command]
fn set_over_taskbar(state: tauri::State<'_, Arc<SharedSettings>>, on: bool) {
    state.set_over_taskbar(on);
}

/// Choose where the colour comes from: `cover` (follow the track) or `custom`.
#[tauri::command]
fn set_color_mode(
    state: tauri::State<'_, Arc<SharedSettings>>,
    mode: String,
) -> Result<(), String> {
    match ColorMode::from_id(&mode) {
        Some(m) => {
            state.set_color_mode(m);
            Ok(())
        }
        None => Err(format!("unknown color mode '{mode}'")),
    }
}

/// Set the custom colour from a `#rrggbb` string.
#[tauri::command]
fn set_custom_color(
    state: tauri::State<'_, Arc<SharedSettings>>,
    hex: String,
) -> Result<(), String> {
    match vibewave_core::settings::parse_hex_color(&hex) {
        Some(c) => {
            state.set_custom_color(c);
            Ok(())
        }
        None => Err(format!("invalid colour '{hex}'")),
    }
}

/// FR-13: keep the glow visible over fullscreen apps.
#[tauri::command]
fn set_stay_visible_fullscreen(state: tauri::State<'_, Arc<SharedSettings>>, on: bool) {
    state.set_stay_visible_fullscreen(on);
}

/// Switch the visual theme (`glow` or `bars`).
#[tauri::command]
fn set_theme(state: tauri::State<'_, Arc<SharedSettings>>, theme: String) -> Result<(), String> {
    match Theme::from_id(&theme) {
        Some(t) => {
            state.set_theme(t);
            Ok(())
        }
        None => Err(format!("unknown theme '{theme}'")),
    }
}

/// Bars theme: which edges carry bars (`all`, `horizontal`, `vertical`).
#[tauri::command]
fn set_bars_position(
    state: tauri::State<'_, Arc<SharedSettings>>,
    position: String,
) -> Result<(), String> {
    match Position::from_id(&position) {
        Some(p) => {
            state.set_bars_position(p);
            Ok(())
        }
        None => Err(format!("unknown bars position '{position}'")),
    }
}

/// Bars theme: bar thickness (96-DPI pixels).
#[tauri::command]
fn set_bars_thickness(state: tauri::State<'_, Arc<SharedSettings>>, px: f32) {
    state.set_bars_thickness(px);
}

/// Bars theme: peak bar length as a fraction of the shortest screen side.
#[tauri::command]
fn set_bars_max_len_frac(state: tauri::State<'_, Arc<SharedSettings>>, value: f32) {
    state.set_bars_max_len_frac(value);
}

/// Bars theme: center bias (middle of each edge reacts most), 0–1.
#[tauri::command]
fn set_bars_center_profile(state: tauri::State<'_, Arc<SharedSettings>>, value: f32) {
    state.set_bars_center_profile(value);
}

/// Bars theme: how strongly bar height follows the spectrum.
#[tauri::command]
fn set_bars_beat_response(state: tauri::State<'_, Arc<SharedSettings>>, value: f32) {
    state.set_bars_beat_response(value);
}

/// Bars theme: how the palette maps onto the bars (`off`, `perbar`, `position`).
#[tauri::command]
fn set_bars_color_mode(
    state: tauri::State<'_, Arc<SharedSettings>>,
    mode: String,
) -> Result<(), String> {
    match BarColorMode::from_id(&mode) {
        Some(m) => {
            state.set_bars_color_mode(m);
            Ok(())
        }
        None => Err(format!("unknown bars colour mode '{mode}'")),
    }
}

/// Bars theme: round the bar ends instead of square.
#[tauri::command]
fn set_bars_rounded(state: tauri::State<'_, Arc<SharedSettings>>, on: bool) {
    state.set_bars_rounded(on);
}

/// Monitors for the dropdown: "Primary monitor" first, then each display.
#[tauri::command]
fn list_monitors() -> Vec<MonitorDto> {
    let mut out = vec![MonitorDto {
        number: MONITOR_PRIMARY,
        label: "Primary monitor".to_string(),
    }];
    out.extend(
        monitors::list()
            .iter()
            .filter(|m| m.number != 0)
            .map(|m| MonitorDto {
                number: m.number,
                label: monitors::label(m),
            }),
    );
    out
}

/// Move the glow to another monitor; the tray menu check marks follow.
#[tauri::command]
fn set_monitor(app: AppHandle, state: tauri::State<'_, Arc<SharedSettings>>, number: u32) {
    state.set_monitor(number);
    crate::tray::refresh(&app);
}

/// Switch the glow on or off (same as the tray "Glow on" item).
#[tauri::command]
fn set_active(app: AppHandle, on: bool) {
    crate::tray::set_active(&app, on);
}

/// Show or hide the tray icon (same as the tray "Hide tray icon" item).
#[tauri::command]
fn set_show_tray_icon(app: AppHandle, on: bool) {
    crate::tray::set_tray_visible(&app, on);
}

/// Turn "Start with Windows" on or off.
#[tauri::command]
fn set_autostart(app: AppHandle, on: bool) -> Result<(), String> {
    crate::tray::set_autostart(&app, on)
}

/// Exit VibeWave (same as the tray "Exit VibeWave" item).
#[tauri::command]
fn quit_app(app: AppHandle) {
    crate::tray::quit(&app);
}

/// Close the settings window, releasing the webview (FR-17).
#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(win) = app.get_webview_window(SETTINGS_LABEL) {
        let _ = win.close();
    }
}

/// Create (or focus) the settings window. Never called automatically.
pub fn open_settings_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(win) = app.get_webview_window(SETTINGS_LABEL) {
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(app, SETTINGS_LABEL, WebviewUrl::default())
        .title("VibeWave Settings")
        .inner_size(900.0, 820.0)
        .min_inner_size(640.0, 480.0)
        .resizable(true)
        .build()?;
    Ok(())
}

/// Build the Tauri app with the settings commands registered and the tray
/// icon installed.
pub fn build_app(
    shared: Arc<SharedSettings>,
    audio: Arc<audio::Shared>,
) -> tauri::Result<tauri::App> {
    tauri::Builder::default()
        .manage(shared)
        .manage(audio)
        .invoke_handler(tauri::generate_handler![
            get_settings,
            set_direction,
            set_thickness,
            set_intensity,
            set_sensitivity,
            save_settings,
            set_over_taskbar,
            set_color_mode,
            set_custom_color,
            set_stay_visible_fullscreen,
            set_theme,
            set_bars_position,
            set_bars_thickness,
            set_bars_max_len_frac,
            set_bars_center_profile,
            set_bars_beat_response,
            set_bars_color_mode,
            set_bars_rounded,
            list_monitors,
            set_monitor,
            set_active,
            set_show_tray_icon,
            set_autostart,
            quit_app,
            close_settings,
        ])
        .setup(|app| {
            crate::tray::install(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
}
