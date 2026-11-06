//! Settings file parsing (`%APPDATA%\VibeWave\settings.ini`).
//!
//! A deliberately tiny `key = value` text format: a handful of scalars do not
//! justify a serialization crate. Parsing is pure so it is fully unit-tested;
//! reading the file (env var, I/O) lives in the binary crate.
//!
//! The file's existence doubles as the "not first run" marker (PRD FR-18):
//! the app writes it at the end of startup on the first run.

use crate::bars::{BarColorMode, Position};
use crate::visual::Direction;

/// Which visual theme is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// The neon edge glow (the original look).
    #[default]
    Glow,
    /// Spectrum bars growing inward from the screen edges.
    Bars,
}

impl Theme {
    pub fn id(self) -> &'static str {
        match self {
            Theme::Glow => "glow",
            Theme::Bars => "bars",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "glow" | "neon" => Some(Theme::Glow),
            "bars" | "bar" | "spectrum" => Some(Theme::Bars),
            _ => None,
        }
    }
}

/// Where the glow's colour comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// Follow the playing track's cover art; fall back to the custom colour,
    /// then to the built-in look (PRD FR-9/FR-10).
    #[default]
    Cover,
    /// Always use the custom colour.
    Custom,
}

impl ColorMode {
    pub fn id(self) -> &'static str {
        match self {
            ColorMode::Cover => "cover",
            ColorMode::Custom => "custom",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cover" | "auto" => Some(ColorMode::Cover),
            "custom" | "manual" => Some(ColorMode::Custom),
            _ => None,
        }
    }
}

/// Parsed settings with documented defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub over_taskbar: bool,
    pub thickness_px: f32,
    pub intensity: f32,
    pub sensitivity: f32,
    pub direction: Direction,
    pub color_mode: ColorMode,
    /// Custom colour as linear sRGB `[r, g, b]` in 0–1.
    pub custom_color: [f32; 3],
    /// Keep the glow visible while a fullscreen app covers the target monitor
    /// (PRD FR-13). Off by default: fullscreen hides the glow.
    pub stay_visible_fullscreen: bool,
    /// Target monitor: [`MONITOR_PRIMARY`] or a Windows display number `n`
    /// (the `n` in `\\.\DISPLAYn`).
    pub monitor: u32,
    /// Show the notification-area (tray) icon. When hidden, launching the app
    /// again opens the settings window, where it can be turned back on.
    pub show_tray_icon: bool,
    /// Which visual theme is painted ([`Theme`]).
    pub theme: Theme,
    /// Bars theme: which edges carry bars.
    pub bars_position: Position,
    /// Bars theme: bar thickness across the edge, in 96-DPI pixels.
    pub bars_thickness_px: f32,
    /// Bars theme: peak bar length as a fraction of the shortest screen side
    /// (UI range 0–0.20).
    pub bars_max_len_frac: f32,
    /// Bars theme: center bias (middle of each edge reacts most), 0–1.
    pub bars_center_profile: f32,
    /// Bars theme: how strongly bar height follows the spectrum.
    pub bars_beat_response: f32,
    /// Bars theme: how the palette maps onto the bars.
    pub bars_color_mode: BarColorMode,
    /// Bars theme: round the bar ends instead of square.
    pub bars_rounded: bool,
}

/// `monitor` value meaning "whichever monitor is primary right now".
pub const MONITOR_PRIMARY: u32 = 0;

impl Default for Settings {
    fn default() -> Self {
        Self {
            over_taskbar: false,
            thickness_px: 24.0,
            intensity: 1.0,
            sensitivity: 1.0,
            direction: Direction::Clockwise,
            color_mode: ColorMode::Cover,
            custom_color: DEFAULT_CUSTOM_COLOR,
            stay_visible_fullscreen: false,
            monitor: MONITOR_PRIMARY,
            show_tray_icon: true,
            theme: Theme::Glow,
            bars_position: Position::All,
            bars_thickness_px: 3.0,
            bars_max_len_frac: 0.10,
            bars_center_profile: 0.6,
            bars_beat_response: 1.0,
            bars_color_mode: BarColorMode::Off,
            bars_rounded: false,
        }
    }
}

/// Default custom colour: the neon magenta, so the field is never unset.
pub const DEFAULT_CUSTOM_COLOR: [f32; 3] = [0.85, 0.15, 0.62];

impl Settings {
    /// Parse the file text. Returns the settings plus a list of human-readable
    /// warnings for values that could not be understood (each such key keeps
    /// its default). Never fails: a broken file yields all defaults.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut s = Settings::default();
        let mut warnings = Vec::new();

        // A UTF-8 BOM (added by some editors, e.g. Notepad) would otherwise glue
        // itself to the first key and make it unrecognised.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);

        for (line_no, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                warnings.push(format!("line {}: missing '='", line_no + 1));
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "over_taskbar" => match parse_bool(value) {
                    Some(b) => s.over_taskbar = b,
                    None => warnings.push(format!(
                        "over_taskbar: invalid value '{value}', using default {}",
                        s.over_taskbar
                    )),
                },
                "thickness_px" => match parse_positive(value) {
                    Some(v) => s.thickness_px = v,
                    None => warnings.push(format!("thickness_px: invalid value '{value}'")),
                },
                "intensity" => match parse_positive(value) {
                    Some(v) => s.intensity = v,
                    None => warnings.push(format!("intensity: invalid value '{value}'")),
                },
                "sensitivity" => match parse_positive(value) {
                    Some(v) => s.sensitivity = v,
                    None => warnings.push(format!("sensitivity: invalid value '{value}'")),
                },
                "color_mode" => match ColorMode::from_id(value) {
                    Some(mode) => s.color_mode = mode,
                    None => warnings.push(format!(
                        "color_mode: unknown value '{value}', using default {}",
                        s.color_mode.id()
                    )),
                },
                "custom_color" => match parse_hex_color(value) {
                    Some(c) => s.custom_color = c,
                    None => warnings.push(format!(
                        "custom_color: invalid value '{value}', expected #rrggbb"
                    )),
                },
                "direction" => match Direction::from_id(value) {
                    Some(dir) => s.direction = dir,
                    None => warnings.push(format!(
                        "direction: unknown value '{value}', using default {}",
                        s.direction.id()
                    )),
                },
                "stay_visible_fullscreen" => match parse_bool(value) {
                    Some(b) => s.stay_visible_fullscreen = b,
                    None => warnings.push(format!(
                        "stay_visible_fullscreen: invalid value '{value}', using default {}",
                        s.stay_visible_fullscreen
                    )),
                },
                "monitor" => match parse_monitor(value) {
                    Some(m) => s.monitor = m,
                    None => warnings.push(format!(
                        "monitor: invalid value '{value}', expected 'primary' or a display number"
                    )),
                },
                "show_tray_icon" => match parse_bool(value) {
                    Some(b) => s.show_tray_icon = b,
                    None => warnings.push(format!(
                        "show_tray_icon: invalid value '{value}', using default {}",
                        s.show_tray_icon
                    )),
                },
                "theme" => match Theme::from_id(value) {
                    Some(t) => s.theme = t,
                    None => warnings.push(format!(
                        "theme: unknown value '{value}', using default {}",
                        s.theme.id()
                    )),
                },
                "bars_position" => match Position::from_id(value) {
                    Some(p) => s.bars_position = p,
                    None => warnings.push(format!(
                        "bars_position: unknown value '{value}', using default {}",
                        s.bars_position.id()
                    )),
                },
                "bars_thickness_px" => match parse_positive(value) {
                    Some(v) => s.bars_thickness_px = v,
                    None => warnings.push(format!("bars_thickness_px: invalid value '{value}'")),
                },
                "bars_max_len_frac" => match parse_positive(value) {
                    Some(v) => s.bars_max_len_frac = v,
                    None => warnings.push(format!("bars_max_len_frac: invalid value '{value}'")),
                },
                "bars_center_profile" => match parse_unit(value) {
                    Some(v) => s.bars_center_profile = v,
                    None => warnings.push(format!(
                        "bars_center_profile: invalid value '{value}', expected 0..1"
                    )),
                },
                "bars_beat_response" => match parse_positive(value) {
                    Some(v) => s.bars_beat_response = v,
                    None => warnings.push(format!("bars_beat_response: invalid value '{value}'")),
                },
                "bars_color_mode" => match BarColorMode::from_id(value) {
                    Some(m) => s.bars_color_mode = m,
                    None => warnings.push(format!(
                        "bars_color_mode: unknown value '{value}', using default {}",
                        s.bars_color_mode.id()
                    )),
                },
                "bars_rounded" => match parse_bool(value) {
                    Some(b) => s.bars_rounded = b,
                    None => warnings.push(format!(
                        "bars_rounded: invalid value '{value}', using default {}",
                        s.bars_rounded
                    )),
                },
                // Unknown keys are ignored on purpose: later features add keys
                // and must not break older binaries.
                _ => {}
            }
        }

        (s, warnings)
    }

    /// Serialise to the same `key = value` format [`Settings::parse`] reads.
    ///
    /// `{}` on an `f32` prints the shortest text that parses back to the exact
    /// same value, so numbers round-trip without drift. Comments and unknown
    /// keys from a hand-edited file are not preserved.
    pub fn to_ini(&self) -> String {
        format!(
            "# VibeWave settings. Written by the app; edit while it is closed.\n\
             over_taskbar = {}\n\
             thickness_px = {}\n\
             intensity = {}\n\
             sensitivity = {}\n\
             direction = {}\n\
             color_mode = {}\n\
             custom_color = {}\n\
             stay_visible_fullscreen = {}\n\
             monitor = {}\n\
             show_tray_icon = {}\n\
             theme = {}\n\
             bars_position = {}\n\
             bars_thickness_px = {}\n\
             bars_max_len_frac = {}\n\
             bars_center_profile = {}\n\
             bars_beat_response = {}\n\
             bars_color_mode = {}\n\
             bars_rounded = {}\n",
            self.over_taskbar,
            self.thickness_px,
            self.intensity,
            self.sensitivity,
            self.direction.id(),
            self.color_mode.id(),
            to_hex_color(self.custom_color),
            self.stay_visible_fullscreen,
            monitor_id(self.monitor),
            self.show_tray_icon,
            self.theme.id(),
            self.bars_position.id(),
            self.bars_thickness_px,
            self.bars_max_len_frac,
            self.bars_center_profile,
            self.bars_beat_response,
            self.bars_color_mode.id(),
            self.bars_rounded,
        )
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_positive(v: &str) -> Option<f32> {
    let n: f32 = v.parse().ok()?;
    if n.is_finite() && n > 0.0 {
        Some(n)
    } else {
        None
    }
}

/// A number in `0..=1` (a unit-range slider).
fn parse_unit(v: &str) -> Option<f32> {
    let n: f32 = v.parse().ok()?;
    (n.is_finite() && (0.0..=1.0).contains(&n)).then_some(n)
}

/// `primary` / `0` → [`MONITOR_PRIMARY`]; `2` or `\\.\DISPLAY2` → 2.
fn parse_monitor(v: &str) -> Option<u32> {
    if v.eq_ignore_ascii_case("primary") {
        return Some(MONITOR_PRIMARY);
    }
    v.parse::<u32>()
        .ok()
        .or_else(|| crate::geometry::display_number(v))
}

/// Settings-file text for a `monitor` value.
fn monitor_id(m: u32) -> String {
    if m == MONITOR_PRIMARY {
        "primary".to_string()
    } else {
        m.to_string()
    }
}

/// Parse `#rrggbb` (or `rrggbb`) into linear sRGB `[r, g, b]` in 0–1.
pub fn parse_hex_color(v: &str) -> Option<[f32; 3]> {
    let s = v.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    let r = byte(0)?;
    let g = byte(2)?;
    let b = byte(4)?;
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}

/// Format a linear sRGB colour as `#rrggbb`.
pub fn to_hex_color(c: [f32; 3]) -> String {
    let b = |v: f32| {
        if v.is_finite() {
            (v.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            0
        }
    };
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_all_defaults() {
        let (s, w) = Settings::parse("");
        assert_eq!(s, Settings::default());
        assert!(w.is_empty());
        assert!(!s.over_taskbar);
        assert_eq!(s.color_mode, ColorMode::Cover);
    }

    #[test]
    fn parses_over_taskbar_variants() {
        for text in [
            "over_taskbar = true",
            "over_taskbar=TRUE",
            "  over_taskbar=true  ",
        ] {
            let (s, _) = Settings::parse(text);
            assert!(s.over_taskbar, "failed for {text:?}");
        }
        let (s, _) = Settings::parse("over_taskbar = false");
        assert!(!s.over_taskbar);
    }

    #[test]
    fn parses_color_mode() {
        let (s, w) = Settings::parse("color_mode = custom");
        assert_eq!(s.color_mode, ColorMode::Custom);
        assert!(w.is_empty());
        let (s, _) = Settings::parse("color_mode = cover");
        assert_eq!(s.color_mode, ColorMode::Cover);
        let (s, _) = Settings::parse("color_mode = auto");
        assert_eq!(s.color_mode, ColorMode::Cover);
    }

    #[test]
    fn unknown_color_mode_warns_and_keeps_default() {
        let (s, w) = Settings::parse("color_mode = rainbow");
        assert_eq!(s.color_mode, ColorMode::Cover);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("color_mode"));
    }

    #[test]
    fn parses_custom_color_hex() {
        let (s, w) = Settings::parse("custom_color = #ff8000");
        assert!(w.is_empty());
        assert!((s.custom_color[0] - 1.0).abs() < 0.01);
        assert!((s.custom_color[1] - 0.5).abs() < 0.01);
        assert!(s.custom_color[2].abs() < 0.01);
        // Without the leading '#' too.
        let (s, _) = Settings::parse("custom_color = 00ff00");
        assert!(s.custom_color[0].abs() < 0.01 && (s.custom_color[1] - 1.0).abs() < 0.01);
    }

    #[test]
    fn invalid_custom_color_warns_and_keeps_default() {
        let (s, w) = Settings::parse("custom_color = notacolor");
        assert_eq!(s.custom_color, DEFAULT_CUSTOM_COLOR);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("custom_color"));
    }

    #[test]
    fn hex_color_round_trips() {
        for hex in ["#ff8000", "#00ff00", "#123456", "#ffffff", "#000000"] {
            let c = parse_hex_color(hex).unwrap();
            assert_eq!(to_hex_color(c), hex);
        }
        assert!(parse_hex_color("#xyzxyz").is_none());
        assert!(parse_hex_color("#fff").is_none());
    }

    #[test]
    fn parses_direction() {
        let (s, w) = Settings::parse("direction = ccw");
        assert_eq!(s.direction, Direction::CounterClockwise);
        assert!(w.is_empty());
        let (s, _) = Settings::parse("direction = cw");
        assert_eq!(s.direction, Direction::Clockwise);
    }

    #[test]
    fn unknown_direction_warns_and_keeps_default() {
        let (s, w) = Settings::parse("direction = sideways");
        assert_eq!(s.direction, Direction::Clockwise);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("direction"));
    }

    #[test]
    fn ignores_comments_blank_lines_and_unknown_keys() {
        let text = "\
# VibeWave
over_taskbar = true

future_key = whatever
";
        let (s, w) = Settings::parse(text);
        assert!(s.over_taskbar);
        assert!(w.is_empty());
    }

    #[test]
    fn invalid_value_warns_and_keeps_default() {
        let (s, w) = Settings::parse("over_taskbar = maybe");
        assert!(!s.over_taskbar);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("over_taskbar"));
    }

    #[test]
    fn parses_numeric_keys_and_rejects_bad_ones() {
        let (s, w) = Settings::parse("thickness_px = 30\nintensity=0.5\nsensitivity = nope");
        assert_eq!(s.thickness_px, 30.0);
        assert_eq!(s.intensity, 0.5);
        assert_eq!(s.sensitivity, 1.0); // default kept
        assert_eq!(w.len(), 1);

        // Non-positive and non-finite are rejected.
        let (s, _) = Settings::parse("thickness_px = -5\nintensity = 0");
        assert_eq!(s.thickness_px, 24.0);
        assert_eq!(s.intensity, 1.0);
    }

    #[test]
    fn to_ini_round_trips_through_parse() {
        let original = Settings {
            over_taskbar: true,
            thickness_px: 37.5,
            intensity: 0.75,
            sensitivity: 2.25,
            direction: Direction::CounterClockwise,
            color_mode: ColorMode::Custom,
            custom_color: [1.0, 0.0, 128.0 / 255.0],
            stay_visible_fullscreen: true,
            monitor: 2,
            show_tray_icon: false,
            theme: Theme::Bars,
            bars_position: Position::Horizontal,
            bars_thickness_px: 5.0,
            bars_max_len_frac: 0.18,
            bars_center_profile: 0.4,
            bars_beat_response: 1.5,
            bars_color_mode: BarColorMode::ByPosition,
            bars_rounded: true,
        };
        let (parsed, w) = Settings::parse(&original.to_ini());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(parsed.over_taskbar, original.over_taskbar);
        assert_eq!(parsed.thickness_px, original.thickness_px);
        assert_eq!(parsed.intensity, original.intensity);
        assert_eq!(parsed.sensitivity, original.sensitivity);
        assert_eq!(parsed.direction, original.direction);
        assert_eq!(parsed.color_mode, original.color_mode);
        assert_eq!(
            parsed.stay_visible_fullscreen,
            original.stay_visible_fullscreen
        );
        assert_eq!(parsed.monitor, original.monitor);
        assert_eq!(parsed.show_tray_icon, original.show_tray_icon);
        assert_eq!(parsed.theme, original.theme);
        assert_eq!(parsed.bars_position, original.bars_position);
        assert_eq!(parsed.bars_thickness_px, original.bars_thickness_px);
        assert_eq!(parsed.bars_max_len_frac, original.bars_max_len_frac);
        assert_eq!(parsed.bars_center_profile, original.bars_center_profile);
        assert_eq!(parsed.bars_beat_response, original.bars_beat_response);
        assert_eq!(parsed.bars_color_mode, original.bars_color_mode);
        assert_eq!(parsed.bars_rounded, original.bars_rounded);
        assert_eq!(
            to_hex_color(parsed.custom_color),
            to_hex_color(original.custom_color)
        );

        // Defaults round-trip too. The default custom colour is not exactly
        // representable in 8-bit hex, so compare it as hex.
        let (d, w) = Settings::parse(&Settings::default().to_ini());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(
            Settings {
                custom_color: DEFAULT_CUSTOM_COLOR,
                ..d
            },
            Settings::default()
        );
        assert_eq!(
            to_hex_color(d.custom_color),
            to_hex_color(DEFAULT_CUSTOM_COLOR)
        );
    }

    #[test]
    fn legacy_character_key_is_ignored() {
        // The Character option was removed (the wave is fixed at its roughest
        // look); an older file's key must not warn or break parsing.
        let (s, w) = Settings::parse("character = 0.2\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn parses_stay_visible_fullscreen() {
        let (s, w) = Settings::parse("stay_visible_fullscreen = true");
        assert!(s.stay_visible_fullscreen);
        assert!(w.is_empty());
        assert!(!Settings::default().stay_visible_fullscreen);
    }

    #[test]
    fn parses_show_tray_icon() {
        assert!(Settings::default().show_tray_icon);
        let (s, w) = Settings::parse("show_tray_icon = false");
        assert!(!s.show_tray_icon);
        assert!(w.is_empty());
        let (s, w) = Settings::parse("show_tray_icon = hidden");
        assert!(s.show_tray_icon);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn parses_monitor_forms() {
        for (text, want) in [
            ("monitor = primary", MONITOR_PRIMARY),
            ("monitor = PRIMARY", MONITOR_PRIMARY),
            ("monitor = 0", MONITOR_PRIMARY),
            ("monitor = 2", 2),
            (r"monitor = \\.\DISPLAY3", 3),
        ] {
            let (s, w) = Settings::parse(text);
            assert_eq!(s.monitor, want, "{text}");
            assert!(w.is_empty(), "{text}: {w:?}");
        }
        let (s, w) = Settings::parse("monitor = left");
        assert_eq!(s.monitor, MONITOR_PRIMARY);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn leading_bom_is_ignored() {
        let (s, w) = Settings::parse("\u{feff}theme = bars\n");
        assert_eq!(s.theme, Theme::Bars);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn line_without_equals_warns() {
        let (s, w) = Settings::parse("this is not a setting");
        assert_eq!(s, Settings::default());
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn parses_theme() {
        assert_eq!(Settings::default().theme, Theme::Glow);
        let (s, w) = Settings::parse("theme = bars");
        assert_eq!(s.theme, Theme::Bars);
        assert!(w.is_empty());
        let (s, _) = Settings::parse("theme = neon");
        assert_eq!(s.theme, Theme::Glow);
        let (s, w) = Settings::parse("theme = checkerboard");
        assert_eq!(s.theme, Theme::Glow);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn parses_bars_keys() {
        let text = "\
theme = bars
bars_position = vertical
bars_thickness_px = 4
bars_max_len_frac = 0.18
bars_center_profile = 0.9
bars_beat_response = 1.8
bars_color_mode = perbar
bars_rounded = true
";
        let (s, w) = Settings::parse(text);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(s.theme, Theme::Bars);
        assert_eq!(s.bars_position, Position::Vertical);
        assert_eq!(s.bars_thickness_px, 4.0);
        assert_eq!(s.bars_max_len_frac, 0.18);
        assert_eq!(s.bars_center_profile, 0.9);
        assert_eq!(s.bars_beat_response, 1.8);
        assert_eq!(s.bars_color_mode, BarColorMode::PerBar);
        assert!(s.bars_rounded);
    }

    #[test]
    fn invalid_bars_values_warn_and_keep_defaults() {
        let (s, w) = Settings::parse(
            "bars_position = diagonal\nbars_thickness_px = -1\nbars_center_profile = 2\n\
             bars_color_mode = rainbow\nbars_rounded = maybe\n",
        );
        let d = Settings::default();
        assert_eq!(s.bars_position, d.bars_position);
        assert_eq!(s.bars_thickness_px, d.bars_thickness_px);
        assert_eq!(s.bars_center_profile, d.bars_center_profile);
        assert_eq!(s.bars_color_mode, d.bars_color_mode);
        assert_eq!(s.bars_rounded, d.bars_rounded);
        assert_eq!(w.len(), 5, "{w:?}");
    }

    #[test]
    fn legacy_bars_floor_key_is_ignored() {
        // The Baseline option was removed; an older file's key must not warn or
        // break parsing.
        let (s, w) = Settings::parse("bars_floor_frac = -0.08\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(s, Settings::default());
    }
}
