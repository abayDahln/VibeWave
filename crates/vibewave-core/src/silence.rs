//! Silence detection on the audio thread.
//!
//! Loopback capture sends **no packets** while nothing is playing, so silence
//! cannot be detected from packet content alone — we track how long it has
//! been since the last packet loud enough to count. Below the threshold
//! (−80 dBFS RMS) for a full hold period, we declare silence.
//!
//! Time is passed in as a monotonic `now` in seconds, so this type is pure and
//! deterministic: no clock, no thread, no Windows.

/// RMS below this (≈ −80 dBFS) counts as silence.
pub const SILENCE_RMS: f32 = 1e-4;
/// How long audio must stay quiet before we declare silence.
pub const HOLD_S: f32 = 3.0;

/// Returned by [`SilenceDetector::update`] so the caller knows when to act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceEvent {
    /// No state change.
    None,
    /// Went quiet for `HOLD_S`.
    BecameSilent,
    /// Audio returned above the threshold.
    Resumed,
}

/// Wall-clock-free silence detector.
#[derive(Debug, Clone)]
pub struct SilenceDetector {
    /// Monotonic time of the last block above the threshold.
    last_loud_s: f32,
    silent: bool,
}

impl SilenceDetector {
    /// Start in the silent state with `now` as the reference time.
    pub fn new(now_s: f32) -> Self {
        Self {
            last_loud_s: now_s - HOLD_S,
            silent: true,
        }
    }

    pub fn is_silent(&self) -> bool {
        self.silent
    }

    /// Feed one audio block's RMS at time `now_s`.
    ///
    /// A loud block always resumes immediately (FR-021). Otherwise silence is
    /// declared once the hold elapses — which is why `tick` exists too, for the
    /// zero-packet case.
    pub fn update(&mut self, rms: f32, now_s: f32) -> SilenceEvent {
        if rms.is_finite() && rms > SILENCE_RMS {
            self.last_loud_s = now_s;
            if self.silent {
                self.silent = false;
                return SilenceEvent::Resumed;
            }
            return SilenceEvent::None;
        }
        self.tick(now_s)
    }

    /// Advance time without a packet (loopback delivered nothing).
    pub fn tick(&mut self, now_s: f32) -> SilenceEvent {
        if !self.silent && now_s - self.last_loud_s >= HOLD_S {
            self.silent = true;
            return SilenceEvent::BecameSilent;
        }
        SilenceEvent::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_silent() {
        let d = SilenceDetector::new(0.0);
        assert!(d.is_silent());
    }

    #[test]
    fn loud_block_resumes_immediately() {
        let mut d = SilenceDetector::new(0.0);
        let ev = d.update(0.5, 1.0);
        assert_eq!(ev, SilenceEvent::Resumed);
        assert!(!d.is_silent());
    }

    #[test]
    fn just_under_hold_stays_awake() {
        let mut d = SilenceDetector::new(0.0);
        d.update(0.5, 1.0);
        // 2.9 s later, still not silent.
        assert_eq!(d.tick(3.9), SilenceEvent::None);
        assert!(!d.is_silent());
    }

    #[test]
    fn hold_elapsed_declares_silence_exactly_once() {
        let mut d = SilenceDetector::new(0.0);
        d.update(0.5, 1.0);
        assert_eq!(d.tick(4.0), SilenceEvent::BecameSilent); // 3.0 s after last loud
        assert!(d.is_silent());
        // Repeated ticks do not re-fire.
        assert_eq!(d.tick(5.0), SilenceEvent::None);
        assert_eq!(d.tick(6.0), SilenceEvent::None);
    }

    #[test]
    fn short_dips_below_threshold_never_trigger() {
        // Audio oscillating around the threshold with gaps < hold must not flap
        // (FR-022).
        let mut d = SilenceDetector::new(0.0);
        let mut t = 0.0f32;
        for _ in 0..10 {
            d.update(0.01, t); // loud
            t += 2.0;
            assert_eq!(d.update(0.0, t), SilenceEvent::None); // quiet, under hold
            t += 0.5;
        }
        assert!(!d.is_silent());
    }

    #[test]
    fn nan_rms_is_treated_as_quiet() {
        let mut d = SilenceDetector::new(0.0);
        d.update(0.5, 1.0);
        d.update(f32::NAN, 1.1);
        assert_eq!(d.tick(4.1), SilenceEvent::BecameSilent);
    }

    #[test]
    fn resumes_after_being_silent() {
        let mut d = SilenceDetector::new(0.0);
        d.update(0.5, 1.0);
        assert_eq!(d.tick(4.0), SilenceEvent::BecameSilent);
        assert_eq!(d.update(0.2, 5.0), SilenceEvent::Resumed);
        assert!(!d.is_silent());
    }
}
