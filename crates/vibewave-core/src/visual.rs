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

/// Envelope time multiplier. Fixed (was the "character" slider, now removed):
/// the snappy value the roughest setting used. `1.0` would be neutral; `0.5`
/// makes the band envelopes follow the music twice as fast.
pub const SMOOTHING_SCALE: f32 = 0.5;

/// Peak depth of the mid wave relative to the thickness. Fixed (roughest
/// setting of the old slider): the wave carves deeper into the glow.
pub const WAVE_AMPLITUDE: f32 = 0.6 * (0.6 + 0.8);

/// Weight of the jagged detail riding on the wave. Fixed (roughest setting):
/// full jagged detail mixed into the smooth sine.
pub const ROUGHNESS: f32 = 1.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_wave_values_are_the_roughest_setting() {
        // These were `smoothing_scale(1.0)`, `wave_amplitude(1.0)`,
        // `roughness(1.0)` before the character slider was removed.
        assert_eq!(SMOOTHING_SCALE, 0.5);
        assert!((WAVE_AMPLITUDE - 0.84).abs() < 1e-6);
        assert_eq!(ROUGHNESS, 1.0);
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
