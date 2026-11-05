//! Deterministic xorshift32 PRNG.
//!
//! Treble sparkles need pseudo-random positions each frame, but the composition
//! must stay testable and reproducible, so we use a tiny seeded generator
//! instead of pulling in a crate. `xorshift32` is the classic Marsaglia
//! algorithm: cheap, no allocation, and good enough for visual jitter.

/// A 32-bit xorshift generator. Clone it to replay the same sequence.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u32,
}

impl Rng {
    /// `seed == 0` is remapped to a fixed non-zero constant, because xorshift
    /// is stuck at zero forever if started there.
    pub fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    /// Next raw 32-bit value.
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Next value in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        // Top 24 bits → [0, 1) with 24-bit resolution, matching f32 mantissa.
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Next value in `[lo, hi)`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.next_f32() * (hi - lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn zero_seed_is_remapped_and_still_runs() {
        let mut a = Rng::new(0);
        let mut b = Rng::new(0);
        assert_eq!(a.next_u32(), b.next_u32());
        // Must not be stuck at zero.
        assert_ne!(a.next_u32(), 0);
    }

    #[test]
    fn next_f32_in_unit_interval() {
        let mut r = Rng::new(7);
        for _ in 0..10_000 {
            let v = r.next_f32();
            assert!((0.0..1.0).contains(&v), "value out of range: {v}");
        }
    }

    #[test]
    fn range_stays_within_bounds() {
        let mut r = Rng::new(123);
        for _ in 0..10_000 {
            let v = r.range(-5.0, 3.0);
            assert!((-5.0..3.0).contains(&v), "range value out of bounds: {v}");
        }
    }
}
