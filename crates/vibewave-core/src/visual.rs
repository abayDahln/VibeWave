//! Fixed look of the glow: the "neon" edge frame.
//!
//! The glow is a tight, high-contrast tube: a bright thin rim right at the edge
//! with a short saturated bloom behind it. The middle of the screen stays fully
//! transparent. All the numbers that describe that look live here as plain
//! constants and functions, so `compose` and the renderer share one definition.
//!
//! Colour is handled separately (`palette` / the cover-art colour service): this
//! module only fixes the *shape* of the light, not which colour it has.

/// Direction the glow travels around the screen perimeter.
///
/// The perimeter is ordered top → right → bottom → left. `Clockwise` moves the
/// travelling wave and the colour flow in that order (clockwise around the
/// screen); `CounterClockwise` reverses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Clockwise,
    CounterClockwise,
}

impl Direction {
    /// A short stable identifier used by the settings file and logs.
    pub fn id(self) -> &'static str {
        match self {
            Direction::Clockwise => "cw",
            Direction::CounterClockwise => "ccw",
        }
    }

    /// Parse a settings value; unknown values return `None`.
    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cw" | "clockwise" => Some(Direction::Clockwise),
            "ccw" | "counterclockwise" | "counter-clockwise" => Some(Direction::CounterClockwise),
            _ => None,
        }
    }

    /// `+1` for clockwise, `-1` for counter-clockwise. Multiplying a travel
    /// term by this flips the direction without duplicating the maths.
    pub fn sign(self) -> f32 {
        match self {
            Direction::Clockwise => 1.0,
            Direction::CounterClockwise => -1.0,
        }
    }
}

/// How the light fades from the edge (position 0) inward (position 1).
///
/// Three gradient stops as `(position, alpha)` pairs. Alpha stays near full
/// through the first quarter (the bright rim), then drops steeply, which is what
/// gives the crisp line plus a short bloom.
pub const FALLOFF: [(f32, f32); 3] = [(0.0, 1.0), (0.22, 0.55), (1.0, 0.0)];

/// Multiplier on the nominal thickness. Keeps the glow reading as a "line"
/// rather than a diffuse wash.
pub const THICKNESS_SCALE: f32 = 0.7;

/// Base hue (degrees) of the default magenta/pink look, used when no cover-art
/// or custom colour applies.
pub const BASE_HUE: f32 = 315.0;

/// Relative sparkle (treble) brightness.
pub const SPARKLE_GAIN: f32 = 1.0;

/// Base rotation speed in perimeter-turns per second at rest (before the
/// music-driven boost).
pub const BASE_SPEED: f32 = 0.08;

/// How much bass+mid boost the rotation speed. At full drive the speed is
/// `BASE_SPEED * (1 + SPEED_DRIVE)`.
pub const SPEED_DRIVE: f32 = 3.5;

/// Wave character: `0` = soft, `1` = rough. The middle (`0.5`) is the
/// original look, so a settings file without the key behaves as before.
///
/// One slider drives three things at once, so "soft" and "rough" feel
/// different everywhere rather than in one detail:
/// - [`smoothing_scale`]: how fast the band envelopes follow the music.
/// - [`wave_amplitude`]: how deep the mid wave carves into the glow.
/// - [`roughness`]: how much jagged fine detail rides on the wave.
pub const DEFAULT_CHARACTER: f32 = 0.5;

/// Clamp a character value into `0..=1`; garbage becomes the default.
pub fn clamp_character(c: f32) -> f32 {
    if c.is_finite() {
        c.clamp(0.0, 1.0)
    } else {
        DEFAULT_CHARACTER
    }
}

/// Envelope time multiplier: `2^(1 - 2c)`, i.e. 2× slower when fully soft,
/// unchanged at the middle, 2× faster when fully rough.
pub fn smoothing_scale(character: f32) -> f32 {
    2f32.powf(1.0 - 2.0 * clamp_character(character))
}

/// Peak depth of the mid wave relative to the thickness: 0.36 (soft) →
/// 0.6 (middle, original) → 0.84 (rough).
pub fn wave_amplitude(character: f32) -> f32 {
    0.6 * (0.6 + 0.8 * clamp_character(character))
}

/// Weight of the jagged detail on the wave: 0 up to the middle, rising to 1
/// when fully rough. The soft half only changes speed and depth.
pub fn roughness(character: f32) -> f32 {
    (2.0 * clamp_character(character) - 1.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_character_is_the_original_look() {
        assert_eq!(smoothing_scale(DEFAULT_CHARACTER), 1.0);
        assert!((wave_amplitude(DEFAULT_CHARACTER) - 0.6).abs() < 1e-6);
        assert_eq!(roughness(DEFAULT_CHARACTER), 0.0);
    }

    #[test]
    fn character_extremes_and_bad_input() {
        assert_eq!(smoothing_scale(0.0), 2.0);
        assert_eq!(smoothing_scale(1.0), 0.5);
        assert_eq!(roughness(0.0), 0.0);
        assert_eq!(roughness(1.0), 1.0);
        assert!(wave_amplitude(1.0) > wave_amplitude(0.0));
        assert_eq!(clamp_character(f32::NAN), DEFAULT_CHARACTER);
        assert_eq!(clamp_character(7.0), 1.0);
    }

    #[test]
    fn direction_default_is_clockwise() {
        assert_eq!(Direction::default(), Direction::Clockwise);
        assert_eq!(Direction::Clockwise.sign(), 1.0);
        assert_eq!(Direction::CounterClockwise.sign(), -1.0);
    }

    #[test]
    fn direction_id_round_trips() {
        for d in [Direction::Clockwise, Direction::CounterClockwise] {
            assert_eq!(Direction::from_id(d.id()), Some(d));
        }
        assert_eq!(Direction::from_id("CCW"), Some(Direction::CounterClockwise));
        assert_eq!(Direction::from_id("clockwise"), Some(Direction::Clockwise));
        assert_eq!(Direction::from_id("sideways"), None);
    }

    #[test]
    fn falloff_starts_opaque_and_ends_transparent() {
        assert_eq!(FALLOFF[0].1, 1.0);
        assert_eq!(FALLOFF[2].1, 0.0);
        assert!(FALLOFF[0].0 < FALLOFF[1].0 && FALLOFF[1].0 < FALLOFF[2].0);
        assert!((0.0..=1.0).contains(&FALLOFF[0].0));
        assert!((0.0..=1.0).contains(&FALLOFF[2].0));
    }
}
