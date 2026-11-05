//! Lock-free hand-off of the latest band levels (and bar spectrum) from the audio
//! thread to the render thread.
//!
//! The renderer only ever needs the *most recent* levels, so a queue would be
//! wasted work: a plain set of atomics is enough. `f32` values are stored as
//! their bit patterns in `AtomicU32`. Cross-band tearing (bass from block *n*,
//! treble from *n+1*) is invisible at 60 FPS, so relaxed ordering is fine for
//! the levels. The `silent` flag uses release/acquire because it coordinates a
//! real state transition (and the wake-up event) between threads.
//!
//! The bar spectrum is a larger array, so it uses a seqlock: the writer bumps a
//! version counter (odd = writing), copies the values, then bumps it again (even
//! = stable). The reader retries while the counter is odd or changed, so it never
//! sees a half-written frame. No allocation, no locking, so the audio thread is
//! never blocked.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::bars::SPECTRUM_BINS;

/// The three band levels plus a timestamp and silence flag, and the bar spectrum.
#[derive(Debug)]
pub struct LevelSlot {
    bass: AtomicU32,
    mid: AtomicU32,
    treble: AtomicU32,
    /// QPC timestamp (100 ns units) of the audio packet these levels came from.
    packet_qpc: AtomicU64,
    silent: AtomicBool,
    /// Seqlock version: odd while a spectrum write is in flight.
    spectrum_seq: AtomicU32,
    /// Bar spectrum, stored as `f32` bit patterns.
    spectrum: [AtomicU32; SPECTRUM_BINS],
}

impl Default for LevelSlot {
    fn default() -> Self {
        Self {
            bass: AtomicU32::new(0.0f32.to_bits()),
            mid: AtomicU32::new(0.0f32.to_bits()),
            treble: AtomicU32::new(0.0f32.to_bits()),
            packet_qpc: AtomicU64::new(0),
            // Before any audio arrives the system is, semantically, silent.
            silent: AtomicBool::new(true),
            spectrum_seq: AtomicU32::new(0),
            spectrum: std::array::from_fn(|_| AtomicU32::new(0.0f32.to_bits())),
        }
    }
}

impl LevelSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a new sample. Called only by the audio thread.
    pub fn store(&self, bass: f32, mid: f32, treble: f32, packet_qpc: u64, silent: bool) {
        self.bass.store(bass.to_bits(), Ordering::Relaxed);
        self.mid.store(mid.to_bits(), Ordering::Relaxed);
        self.treble.store(treble.to_bits(), Ordering::Relaxed);
        self.packet_qpc.store(packet_qpc, Ordering::Relaxed);
        // Release so a render thread that observes `false` also sees the
        // levels written above.
        self.silent.store(silent, Ordering::Release);
    }

    /// Publish the bar spectrum. Called only by the audio thread.
    pub fn store_spectrum(&self, spectrum: &[f32]) {
        let seq = self.spectrum_seq.load(Ordering::Relaxed);
        self.spectrum_seq
            .store(seq.wrapping_add(1), Ordering::Relaxed); // odd
        for (dst, &src) in self.spectrum.iter().zip(spectrum.iter()) {
            dst.store(src.to_bits(), Ordering::Relaxed);
        }
        // Slots beyond the written slice read 0.
        for dst in self.spectrum.iter().skip(spectrum.len()) {
            dst.store(0.0f32.to_bits(), Ordering::Relaxed);
        }
        self.spectrum_seq
            .store(seq.wrapping_add(2), Ordering::Release); // even
    }

    /// Read the latest `(bass, mid, treble, packet_qpc)`.
    pub fn load_levels(&self) -> (f32, f32, f32, u64) {
        (
            f32::from_bits(self.bass.load(Ordering::Relaxed)),
            f32::from_bits(self.mid.load(Ordering::Relaxed)),
            f32::from_bits(self.treble.load(Ordering::Relaxed)),
            self.packet_qpc.load(Ordering::Relaxed),
        )
    }

    /// Copy the latest bar spectrum into `out` (up to `out.len()` slots).
    ///
    /// Returns `false` if a writer was in flight every attempt (the caller keeps
    /// its previous spectrum). Never blocks the audio thread.
    pub fn load_spectrum(&self, out: &mut [f32]) -> bool {
        for _ in 0..8 {
            let seq = self.spectrum_seq.load(Ordering::Acquire);
            if seq & 1 != 0 {
                continue;
            }
            for (dst, src) in out.iter_mut().zip(self.spectrum.iter()) {
                *dst = f32::from_bits(src.load(Ordering::Relaxed));
            }
            if self.spectrum_seq.load(Ordering::Acquire) == seq {
                return true;
            }
        }
        false
    }

    /// Is the audio thread reporting silence?
    pub fn is_silent(&self) -> bool {
        self.silent.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_values_across_threads() {
        let slot = LevelSlot::new();
        std::thread::scope(|scope| {
            // Writer keeps overwriting; reader must always see a consistent,
            // in-range triple, and the final read must equal the last write.
            scope.spawn(|| {
                for i in 0..10_000u32 {
                    let v = i as f32 / 10_000.0;
                    slot.store(v, 1.0 - v, 0.5, i as u64, false);
                }
                slot.store(0.25, 0.5, 0.75, 99, true);
            });
            scope.spawn(|| {
                for _ in 0..10_000 {
                    let (b, m, t, _) = slot.load_levels();
                    assert!((0.0..=1.0).contains(&b));
                    assert!((0.0..=1.0).contains(&m));
                    assert!((0.0..=1.0).contains(&t));
                }
            });
        });
        let (b, m, t, q) = slot.load_levels();
        assert_eq!((b, m, t, q), (0.25, 0.5, 0.75, 99));
        assert!(slot.is_silent());
    }

    #[test]
    fn defaults_are_zero_and_silent() {
        let slot = LevelSlot::new();
        assert_eq!(slot.load_levels(), (0.0, 0.0, 0.0, 0));
        assert!(slot.is_silent());
        let mut spec = [1.0f32; SPECTRUM_BINS];
        assert!(slot.load_spectrum(&mut spec));
        assert!(spec.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn spectrum_round_trips() {
        let slot = LevelSlot::new();
        let mut spec = [0.0f32; SPECTRUM_BINS];
        for (i, v) in spec.iter_mut().enumerate() {
            *v = i as f32 / SPECTRUM_BINS as f32;
        }
        slot.store_spectrum(&spec);
        let mut out = [0.0f32; SPECTRUM_BINS];
        assert!(slot.load_spectrum(&mut out));
        assert_eq!(out, spec);
    }

    #[test]
    fn spectrum_reader_slice_shorter_than_slot() {
        let slot = LevelSlot::new();
        let spec = [0.5f32; SPECTRUM_BINS];
        slot.store_spectrum(&spec);
        let mut small = [0.0f32; 8];
        assert!(slot.load_spectrum(&mut small));
        assert!(small.iter().all(|&v| v == 0.5));
    }

    #[test]
    fn spectrum_store_pads_short_slices_with_zero() {
        let slot = LevelSlot::new();
        slot.store_spectrum(&[0.5f32; SPECTRUM_BINS]);
        // A shorter write must zero the remaining slots, not leave stale values.
        slot.store_spectrum(&[0.25, 0.25]);
        let mut out = [1.0f32; SPECTRUM_BINS];
        assert!(slot.load_spectrum(&mut out));
        assert_eq!(out[0], 0.25);
        assert_eq!(out[1], 0.25);
        assert_eq!(out[2], 0.0);
    }

    #[test]
    fn spectrum_is_consistent_under_concurrent_writes() {
        let slot = LevelSlot::new();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for round in 0..2000u32 {
                    let v = round as f32 / 2000.0;
                    slot.store_spectrum(&[v; SPECTRUM_BINS]);
                }
            });
            scope.spawn(|| {
                let mut out = [0.0f32; SPECTRUM_BINS];
                for _ in 0..5000 {
                    if slot.load_spectrum(&mut out) {
                        // Every slot in one read must come from the same write.
                        let first = out[0];
                        assert!(out.iter().all(|&v| (v - first).abs() < 1e-6));
                    }
                }
            });
        });
    }
}
