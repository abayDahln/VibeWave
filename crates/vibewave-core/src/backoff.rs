//! Exponential retry backoff for audio-capture recovery.
//!
//! When capture fails (device unplugged, audio service restarting), we retry
//! with a growing delay so a broken device does not spin the CPU. The delay
//! doubles from 500 ms up to a 5 s cap and resets on success.

/// Retry delay schedule: 0.5 s, 1 s, 2 s, 4 s, 5 s, 5 s, …
#[derive(Debug, Clone)]
pub struct Backoff {
    current: f32,
    max: f32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            current: 0.5,
            max: 5.0,
        }
    }
}

impl Backoff {
    /// Current delay in seconds, before the next retry.
    pub fn delay_s(&self) -> f32 {
        self.current
    }

    /// Call after a failed attempt: returns the delay to wait, then doubles it.
    pub fn next_delay(&mut self) -> f32 {
        let d = self.current;
        self.current = (self.current * 2.0).min(self.max);
        d
    }

    /// Call after a successful attempt to return to the base delay.
    pub fn reset(&mut self) {
        self.current = 0.5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_doubles_then_caps() {
        let mut b = Backoff::default();
        assert_eq!(b.delay_s(), 0.5);
        let seq: Vec<f32> = (0..6).map(|_| b.next_delay()).collect();
        assert_eq!(seq, vec![0.5, 1.0, 2.0, 4.0, 5.0, 5.0]);
    }

    #[test]
    fn reset_returns_to_base() {
        let mut b = Backoff::default();
        for _ in 0..4 {
            b.next_delay();
        }
        assert!(b.delay_s() > 0.5);
        b.reset();
        assert_eq!(b.delay_s(), 0.5);
        assert_eq!(b.next_delay(), 0.5);
    }
}
