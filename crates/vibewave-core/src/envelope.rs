//! Per-band smoothing (attack/release envelope).
//!
//! Raw band levels jump around frame to frame, which would make the glow
//! flicker. Each band is smoothed with a one-pole filter whose speed depends on
//! whether the signal is rising (attack) or falling (release), and on the real
//! frame time `dt` — so the feel is identical at 60 or 120 FPS.
//!
//! The coefficient `1 - exp(-dt / tau)` is the exact discrete solution of a
//! first-order lag with time constant `tau`, so it is frame-rate independent.

/// Smoothing time constants in seconds, from research R-6.
///
/// - bass: slow and thick (30 ms attack, 300 ms release) → a heavy pulse.
/// - mid: medium (60/250 ms) → a flowing wave.
/// - treble: fast (10/80 ms) → a quick sparkle.
#[derive(Debug, Clone, Copy)]
pub struct BandTimes {
    pub attack_s: f32,
    pub release_s: f32,
}

impl BandTimes {
    pub const BASS: BandTimes = BandTimes {
        attack_s: 0.030,
        release_s: 0.300,
    };
    pub const MID: BandTimes = BandTimes {
        attack_s: 0.060,
        release_s: 0.250,
    };
    pub const TREBLE: BandTimes = BandTimes {
        attack_s: 0.010,
        release_s: 0.080,
    };
}

/// One-pole follower for a single value.
#[derive(Debug, Clone)]
pub struct Envelope {
    times: BandTimes,
    /// Multiplier on both time constants (>1 slower and smoother, <1 snappier).
    scale: f32,
    value: f32,
}

impl Envelope {
    pub fn new(times: BandTimes) -> Self {
        Self {
            times,
            scale: 1.0,
            value: 0.0,
        }
    }

    /// Stretch (`> 1`, slower and smoother) or shrink (`< 1`, snappier) the
    /// attack and release times. Non-finite or non-positive values reset to 1.
    pub fn set_time_scale(&mut self, scale: f32) {
        self.scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Advance toward `target` by `dt` seconds and return the new value.
    pub fn update(&mut self, target: f32, dt: f32) -> f32 {
        let target = if target.is_finite() {
            target.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let tau = self.scale
            * if target > self.value {
                self.times.attack_s
            } else {
                self.times.release_s
            };
        // Guard against non-positive dt (paused clocks) and tiny tau.
        let coeff = if dt > 0.0 && tau > 0.0 {
            1.0 - (-dt / tau).exp()
        } else {
            0.0
        };
        self.value += (target - self.value) * coeff;
        if !self.value.is_finite() {
            self.value = 0.0;
        }
        self.value.clamp(0.0, 1.0)
    }
}

/// Convenience bundle of the three band envelopes.
#[derive(Debug, Clone)]
pub struct BandEnvelopes {
    pub bass: Envelope,
    pub mid: Envelope,
    pub treble: Envelope,
}

impl Default for BandEnvelopes {
    fn default() -> Self {
        Self {
            bass: Envelope::new(BandTimes::BASS),
            mid: Envelope::new(BandTimes::MID),
            treble: Envelope::new(BandTimes::TREBLE),
        }
    }
}

impl BandEnvelopes {
    /// Apply the same time scale to all three bands (see
    /// [`Envelope::set_time_scale`]).
    pub fn set_time_scale(&mut self, scale: f32) {
        self.bass.set_time_scale(scale);
        self.mid.set_time_scale(scale);
        self.treble.set_time_scale(scale);
    }

    /// Smooth all three bands for a `dt`-second frame.
    pub fn update(&mut self, bass: f32, mid: f32, treble: f32, dt: f32) -> [f32; 3] {
        [
            self.bass.update(bass, dt),
            self.mid.update(mid, dt),
            self.treble.update(treble, dt),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_reaches_63_percent_after_one_tau() {
        let mut e = Envelope::new(BandTimes {
            attack_s: 0.1,
            release_s: 0.1,
        });
        let v = e.update(1.0, 0.1);
        assert!((v - 0.632).abs() < 0.01, "expected ~0.632, got {v}");
    }

    #[test]
    fn release_is_slower_than_attack() {
        let times = BandTimes {
            attack_s: 0.05,
            release_s: 0.5,
        };
        let mut rise = Envelope::new(times);
        let mut fall = Envelope::new(times);
        // Bring one up to 1.0 fast.
        for _ in 0..100 {
            rise.update(1.0, 0.01);
        }
        assert!(rise.value() > 0.95);
        fall.update(1.0, 10.0); // jump to 1.0
        let after_one_frame_rise = {
            let mut r = Envelope::new(times);
            r.update(1.0, 0.05)
        };
        let after_one_frame_fall = fall.update(0.0, 0.05);
        // Falling from 1.0 in 50 ms should drop less than rising 0→in 50 ms rises.
        assert!((1.0 - after_one_frame_fall) < after_one_frame_rise);
    }

    #[test]
    fn frame_rate_independent() {
        let times = BandTimes {
            attack_s: 0.08,
            release_s: 0.2,
        };
        let mut a = Envelope::new(times);
        let mut b = Envelope::new(times);
        for _ in 0..60 {
            a.update(1.0, 1.0 / 60.0);
        }
        for _ in 0..120 {
            b.update(1.0, 1.0 / 120.0);
        }
        assert!(
            (a.value() - b.value()).abs() < 0.01,
            "{} vs {}",
            a.value(),
            b.value()
        );
    }

    #[test]
    fn non_finite_and_negative_dt_are_safe() {
        let mut e = Envelope::new(BandTimes::BASS);
        e.update(f32::NAN, 0.016);
        assert!(e.value().is_finite());
        let v = e.update(1.0, -1.0);
        assert!(v.is_finite());
        assert_eq!(v, 0.0);
    }

    #[test]
    fn larger_time_scale_is_slower() {
        let mut soft = Envelope::new(BandTimes::MID);
        let mut rough = Envelope::new(BandTimes::MID);
        soft.set_time_scale(2.0);
        rough.set_time_scale(0.5);
        let s = soft.update(1.0, 0.03);
        let r = rough.update(1.0, 0.03);
        assert!(r > s, "rough {r} should rise faster than soft {s}");
        // Bad input resets to the normal speed.
        let mut e = Envelope::new(BandTimes::MID);
        e.set_time_scale(f32::NAN);
        let mut n = Envelope::new(BandTimes::MID);
        assert_eq!(e.update(1.0, 0.03), n.update(1.0, 0.03));
    }

    #[test]
    fn clamps_to_unit_interval() {
        let mut e = Envelope::new(BandTimes::TREBLE);
        for _ in 0..1000 {
            e.update(5.0, 0.016);
        }
        assert!(e.value() <= 1.0);
    }
}
