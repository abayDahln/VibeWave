//! Live settings shared between the settings window (Tauri), the media watcher,
//! and the overlay render thread.
//!
//! The overlay must react to a settings change immediately (FR-15). The render
//! thread runs its own loop, so the two sides communicate through this small
//! lock-protected snapshot rather than messages. Writes are rare, so a mutex is
//! fine — it is never touched from the audio hot loop.
//!
//! The resolved [`Palette`] is computed here (see [`resolve_palette`]) from the
//! colour mode, the custom colour, and the latest cover-art palette, so the
//! render loop and the settings window always agree on what colour the glow is.

use std::sync::Mutex;

use vibewave_core::bars::Position;
use vibewave_core::palette::Palette;
use vibewave_core::settings::{ColorMode, Settings, Theme, BARS_FLOOR_MAX, BARS_FLOOR_MIN};
use vibewave_core::visual::Direction;

/// A settings snapshot plus the resolved colour palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveSettings {
    pub settings: Settings,
    /// The palette actually painted: cover art, custom, or the default look.
    pub palette: Palette,
    /// True when the palette currently comes from cover art (for the UI badge).
    pub palette_from_cover: bool,
    /// False when the user switched the glow off from the tray (FR-14). Runtime
    /// only: a restart always starts active.
    pub active: bool,
}

/// Allowed sensitivity range: 0.25 (subtle) to 3.0 (reacts to quiet detail).
pub const SENSITIVITY_MIN: f32 = 0.25;
pub const SENSITIVITY_MAX: f32 = 3.0;

/// Resolve the palette to paint, following PRD FR-10:
/// cover art → custom colour → built-in default look.
///
/// In `Custom` mode the custom colour always wins. In `Cover` mode the cover
/// palette is used when available; otherwise the custom colour, then the
/// default look.
pub fn resolve_palette(
    mode: ColorMode,
    custom: [f32; 3],
    cover: Option<Palette>,
) -> (Palette, bool) {
    match mode {
        ColorMode::Custom => (Palette::single(custom), false),
        ColorMode::Cover => match cover {
            Some(p) if !p.is_empty() => (p, true),
            _ => (Palette::single(custom), false),
        },
    }
}

/// Thread-safe holder the render loop reads each frame and commands write.
pub struct SharedSettings {
    inner: Mutex<Inner>,
}

struct Inner {
    settings: Settings,
    /// Latest cover-art palette, or `None` when no session has a thumbnail.
    cover: Option<Palette>,
    active: bool,
}

impl SharedSettings {
    pub fn new(settings: Settings) -> Self {
        Self {
            inner: Mutex::new(Inner {
                settings,
                cover: None,
                active: true,
            }),
        }
    }

    /// Snapshot for the render loop (cheap copy).
    pub fn get(&self) -> LiveSettings {
        match self.inner.lock() {
            Ok(g) => {
                let (palette, from_cover) =
                    resolve_palette(g.settings.color_mode, g.settings.custom_color, g.cover);
                LiveSettings {
                    settings: g.settings,
                    palette,
                    palette_from_cover: from_cover,
                    active: g.active,
                }
            }
            // A poisoned lock means a writer panicked; fall back to defaults
            // rather than panicking the render thread.
            Err(_) => {
                let s = Settings::default();
                let (palette, from_cover) = resolve_palette(s.color_mode, s.custom_color, None);
                LiveSettings {
                    settings: s,
                    palette,
                    palette_from_cover: from_cover,
                    active: true,
                }
            }
        }
    }

    /// Apply a change; the render loop picks it up on its next frame.
    pub fn update<F: FnOnce(&mut Settings)>(&self, f: F) {
        if let Ok(mut g) = self.inner.lock() {
            f(&mut g.settings);
        }
    }

    /// Publish a new cover-art palette (called by the media watcher). `None`
    /// means "no cover art available right now".
    pub fn set_cover_palette(&self, cover: Option<Palette>) {
        if let Ok(mut g) = self.inner.lock() {
            g.cover = cover;
        }
    }

    pub fn set_direction(&self, direction: Direction) {
        self.update(|s| s.direction = direction);
    }

    pub fn set_color_mode(&self, mode: ColorMode) {
        self.update(|s| s.color_mode = mode);
    }

    pub fn set_custom_color(&self, color: [f32; 3]) {
        self.update(|s| s.custom_color = color);
    }

    pub fn set_thickness(&self, px: f32) {
        let px = if px.is_finite() {
            px.clamp(4.0, 96.0)
        } else {
            24.0
        };
        self.update(|s| s.thickness_px = px);
    }

    pub fn set_intensity(&self, v: f32) {
        let v = if v.is_finite() {
            v.clamp(0.0, 2.0)
        } else {
            1.0
        };
        self.update(|s| s.intensity = v);
    }

    pub fn set_over_taskbar(&self, on: bool) {
        self.update(|s| s.over_taskbar = on);
    }

    /// Clamp and store the sensitivity; returns the value actually stored so
    /// the caller can forward the same number to the audio thread.
    pub fn set_sensitivity(&self, v: f32) -> f32 {
        let v = if v.is_finite() {
            v.clamp(SENSITIVITY_MIN, SENSITIVITY_MAX)
        } else {
            1.0
        };
        self.update(|s| s.sensitivity = v);
        v
    }

    /// Switch the glow on or off without quitting (tray "Active").
    pub fn set_active(&self, on: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.active = on;
        }
    }

    /// Wave character, clamped to `0..=1` (0 = soft, 1 = rough).
    pub fn set_character(&self, v: f32) {
        let v = vibewave_core::visual::clamp_character(v);
        self.update(|s| s.character = v);
    }

    /// FR-13: keep the glow visible over fullscreen apps.
    pub fn set_stay_visible_fullscreen(&self, on: bool) {
        self.update(|s| s.stay_visible_fullscreen = on);
    }

    /// Target monitor: a display number, or `MONITOR_PRIMARY`. The overlay
    /// thread moves to it on its next frame.
    pub fn set_monitor(&self, number: u32) {
        self.update(|s| s.monitor = number);
    }

    /// Show or hide the tray icon (persisted).
    pub fn set_show_tray_icon(&self, on: bool) {
        self.update(|s| s.show_tray_icon = on);
    }

    /// Switch the visual theme (Glow / Bars).
    pub fn set_theme(&self, theme: Theme) {
        self.update(|s| s.theme = theme);
    }

    /// Bars theme: which edges carry bars.
    pub fn set_bars_position(&self, position: Position) {
        self.update(|s| s.bars_position = position);
    }

    /// Bars theme: bar thickness in 96-DPI pixels, clamped to a sane range.
    pub fn set_bars_thickness(&self, px: f32) {
        let px = if px.is_finite() {
            px.clamp(1.0, 24.0)
        } else {
            3.0
        };
        self.update(|s| s.bars_thickness_px = px);
    }

    /// Bars theme: peak bar length as a fraction of the shortest screen side.
    pub fn set_bars_max_len_frac(&self, v: f32) {
        let v = if v.is_finite() {
            v.clamp(0.05, 0.6)
        } else {
            0.28
        };
        self.update(|s| s.bars_max_len_frac = v);
    }

    /// Bars theme: baseline offset (fraction of the shortest side); negative
    /// overhangs outward.
    pub fn set_bars_floor_frac(&self, v: f32) {
        let v = if v.is_finite() {
            v.clamp(BARS_FLOOR_MIN, BARS_FLOOR_MAX)
        } else {
            0.12
        };
        self.update(|s| s.bars_floor_frac = v);
    }

    /// Bars theme: center bias, 0–1.
    pub fn set_bars_center_profile(&self, v: f32) {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.6
        };
        self.update(|s| s.bars_center_profile = v);
    }

    /// Bars theme: how strongly bar height follows the spectrum.
    pub fn set_bars_beat_response(&self, v: f32) {
        let v = if v.is_finite() {
            v.clamp(0.1, 3.0)
        } else {
            1.0
        };
        self.update(|s| s.bars_beat_response = v);
    }
}

impl Default for SharedSettings {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_readable() {
        let s = SharedSettings::default();
        let live = s.get();
        assert_eq!(live.settings.color_mode, ColorMode::Cover);
        assert!(!live.palette_from_cover);
        // Cover mode with no cover art falls back to the default custom colour.
        assert_eq!(live.palette.at(0.3), live.settings.custom_color);
    }

    #[test]
    fn updates_are_visible_to_readers() {
        let s = SharedSettings::default();
        s.set_direction(Direction::CounterClockwise);
        s.set_thickness(40.0);
        s.set_intensity(0.5);
        s.set_over_taskbar(true);
        let live = s.get();
        assert_eq!(live.settings.direction, Direction::CounterClockwise);
        assert_eq!(live.settings.thickness_px, 40.0);
        assert_eq!(live.settings.intensity, 0.5);
        assert!(live.settings.over_taskbar);
    }

    #[test]
    fn custom_mode_uses_custom_colour() {
        let s = SharedSettings::default();
        s.set_custom_color([0.1, 0.2, 0.3]);
        s.set_color_mode(ColorMode::Custom);
        let live = s.get();
        assert_eq!(live.palette.at(0.0), [0.1, 0.2, 0.3]);
        assert!(!live.palette_from_cover);
    }

    #[test]
    fn cover_mode_uses_cover_palette_when_available() {
        let s = SharedSettings::default();
        let cover = Palette::new(&[[0.0, 1.0, 0.0], [1.0, 1.0, 0.0]]);
        s.set_cover_palette(Some(cover));
        let live = s.get();
        assert!(live.palette_from_cover);
        assert_eq!(live.palette, cover);
    }

    #[test]
    fn cover_mode_falls_back_to_custom_then_default() {
        let s = SharedSettings::default();
        s.set_custom_color([0.5, 0.0, 0.5]);
        // No cover → custom.
        assert_eq!(s.get().palette.at(0.0), [0.5, 0.0, 0.5]);
        // Cover arrives → cover wins.
        s.set_cover_palette(Some(Palette::single([0.0, 1.0, 1.0])));
        assert_eq!(s.get().palette.at(0.0), [0.0, 1.0, 1.0]);
        // Cover disappears → back to custom.
        s.set_cover_palette(None);
        assert_eq!(s.get().palette.at(0.0), [0.5, 0.0, 0.5]);
    }

    #[test]
    fn cover_mode_without_custom_uses_default_custom_colour() {
        let s = SharedSettings::default();
        // No cover, no explicit custom → the built-in default custom colour.
        assert_eq!(
            s.get().palette.at(0.0),
            vibewave_core::settings::DEFAULT_CUSTOM_COLOR
        );
    }

    #[test]
    fn values_are_clamped_against_bad_input() {
        let s = SharedSettings::default();
        s.set_thickness(f32::NAN);
        assert_eq!(s.get().settings.thickness_px, 24.0);
        s.set_thickness(-100.0);
        assert_eq!(s.get().settings.thickness_px, 4.0);
        s.set_thickness(9999.0);
        assert_eq!(s.get().settings.thickness_px, 96.0);
        s.set_intensity(99.0);
        assert_eq!(s.get().settings.intensity, 2.0);
        assert_eq!(s.set_sensitivity(10.0), SENSITIVITY_MAX);
        assert_eq!(s.set_sensitivity(0.0), SENSITIVITY_MIN);
        assert_eq!(s.set_sensitivity(f32::NAN), 1.0);
        assert_eq!(s.get().settings.sensitivity, 1.0);
    }

    #[test]
    fn character_fullscreen_and_monitor_setters() {
        let s = SharedSettings::default();
        assert_eq!(s.get().settings.character, 0.5);
        s.set_character(2.0);
        assert_eq!(s.get().settings.character, 1.0);
        s.set_character(f32::NAN);
        assert_eq!(s.get().settings.character, 0.5);
        s.set_stay_visible_fullscreen(true);
        assert!(s.get().settings.stay_visible_fullscreen);
        s.set_monitor(2);
        assert_eq!(s.get().settings.monitor, 2);
        assert!(s.get().settings.show_tray_icon);
        s.set_show_tray_icon(false);
        assert!(!s.get().settings.show_tray_icon);
    }

    #[test]
    fn bars_theme_setters_clamp_and_store() {
        let s = SharedSettings::default();
        assert_eq!(s.get().settings.theme, Theme::Glow);
        s.set_theme(Theme::Bars);
        assert_eq!(s.get().settings.theme, Theme::Bars);
        s.set_bars_position(Position::Vertical);
        assert_eq!(s.get().settings.bars_position, Position::Vertical);

        s.set_bars_thickness(999.0);
        assert_eq!(s.get().settings.bars_thickness_px, 24.0);
        s.set_bars_thickness(f32::NAN);
        assert_eq!(s.get().settings.bars_thickness_px, 3.0);

        s.set_bars_max_len_frac(5.0);
        assert_eq!(s.get().settings.bars_max_len_frac, 0.6);

        s.set_bars_floor_frac(-5.0);
        assert_eq!(s.get().settings.bars_floor_frac, BARS_FLOOR_MIN);
        s.set_bars_center_profile(2.0);
        assert_eq!(s.get().settings.bars_center_profile, 1.0);
        s.set_bars_beat_response(0.0);
        assert_eq!(s.get().settings.bars_beat_response, 0.1);
    }

    #[test]
    fn active_defaults_on_and_toggles() {
        let s = SharedSettings::default();
        assert!(s.get().active);
        s.set_active(false);
        assert!(!s.get().active);
        s.set_active(true);
        assert!(s.get().active);
    }
}
