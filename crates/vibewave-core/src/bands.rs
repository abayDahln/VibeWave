//! Three-band audio analysis with an FFT.
//!
//! The audio thread pushes interleaved float samples here; `analyze` returns
//! how much energy sits in the bass, mid and treble frequency ranges, each
//! normalised to `[0, 1]`.
//!
//! How it works, in plain terms:
//! 1. Samples are downmixed to mono and pushed into a ring buffer holding the
//!    most recent 1024 samples (~21 ms at 48 kHz).
//! 2. A Hann window tapers the block so the FFT does not see fake sharp edges.
//! 3. The forward FFT turns those 1024 samples into 512 magnitude bins; bin
//!    `k` covers frequency `k * sample_rate / 1024`.
//! 4. We sum the power `|X(k)|²` over each band's bins.
//! 5. Each band's energy is divided by a shared adaptive peak so quiet and
//!    loud songs both react fully, then clamped to `[0, 1]`. The peak is shared
//!    across bands, so a lone bass tone lights only the bass band.
//!
//! All buffers are allocated once in `new`; `push_interleaved` and `analyze`
//! allocate nothing, which keeps the real-time thread allocation-free.

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

/// FFT size in samples. 1024 @48 kHz ≈ 21 ms window, 47 Hz per bin.
pub const FFT_SIZE: usize = 1024;

/// Band edges in Hz (research R-3).
const BASS_LO: f32 = 20.0;
const BASS_HI: f32 = 250.0;
const MID_HI: f32 = 4000.0;
const TREBLE_HI: f32 = 16000.0;

/// Below this normalised energy a band is treated as silent (keeps the floor
/// from being amplified into a full glow on near-silence).
const NOISE_FLOOR: f32 = 2e-4;
/// Adaptive peak decay per `analyze` call: the peak falls 0.5% each block, so a
/// loud passage keeps the glow scaled down for a moment instead of flickering.
const PEAK_DECAY: f32 = 0.995;

/// One analysed block: three levels in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BandLevels {
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
}

impl BandLevels {
    /// Overall loudness used where a single value is needed.
    pub fn combined(&self) -> f32 {
        (self.bass + self.mid + self.treble) / 3.0
    }
}

/// Stateful 3-band analyzer. One per capture stream.
pub struct BandAnalyzer {
    sample_rate: f32,
    ring: [f32; FFT_SIZE],
    write: usize,
    filled: bool,
    window: [f32; FFT_SIZE],
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    bass_bins: (usize, usize),
    mid_bins: (usize, usize),
    treble_bins: (usize, usize),
    /// Shared adaptive peak across all bands, so bands stay comparable: a
    /// single tone lights only its own band instead of every band's leak.
    peak: f32,
    sensitivity: f32,
    /// Adaptive peak per bar-spectrum slot (see [`BandAnalyzer::spectrum`]).
    bar_peaks: [f32; crate::bars::SPECTRUM_BINS],
}

impl BandAnalyzer {
    /// Create an analyzer for `sample_rate` Hz. `sensitivity` scales all bands
    /// before clamping (1.0 = neutral).
    pub fn new(sample_rate: u32, sensitivity: f32) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let scratch_len = fft.get_inplace_scratch_len();

        // Hann window: 0.5 - 0.5 cos(2πn/(N-1)); tapers block edges to zero.
        let mut window = [0.0f32; FFT_SIZE];
        for (n, w) in window.iter_mut().enumerate() {
            let x = 2.0 * std::f32::consts::PI * n as f32 / (FFT_SIZE as f32 - 1.0);
            *w = 0.5 - 0.5 * x.cos();
        }

        let bin_of =
            |hz: f32| ((hz * FFT_SIZE as f32 / sample_rate).round() as usize).min(FFT_SIZE / 2);
        let bass_lo = bin_of(BASS_LO).max(1);
        let bass_hi = bin_of(BASS_HI).max(bass_lo + 1);
        let mid_hi = bin_of(MID_HI).max(bass_hi + 1);
        let treble_hi = bin_of(TREBLE_HI).max(mid_hi + 1);

        Self {
            sample_rate,
            ring: [0.0; FFT_SIZE],
            write: 0,
            filled: false,
            window,
            fft,
            buffer: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            bass_bins: (bass_lo, bass_hi),
            mid_bins: (bass_hi, mid_hi),
            treble_bins: (mid_hi, treble_hi),
            peak: NOISE_FLOOR,
            sensitivity: valid_sensitivity(sensitivity),
            bar_peaks: [NOISE_FLOOR; crate::bars::SPECTRUM_BINS],
        }
    }

    /// Change the sensitivity without rebuilding the analyzer (no allocation,
    /// so the audio thread can call it). Non-finite or non-positive → 1.0.
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = valid_sensitivity(sensitivity);
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate as u32
    }

    /// Push interleaved samples (`channels` per frame), downmixing to mono.
    /// Allocation-free; safe to call from the real-time thread.
    pub fn push_interleaved(&mut self, samples: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let inv = 1.0 / channels as f32;
        for frame in samples.chunks_exact(channels) {
            let mut mono = 0.0;
            for &s in frame {
                mono += if s.is_finite() { s } else { 0.0 };
            }
            mono *= inv;
            self.ring[self.write] = mono;
            self.write = (self.write + 1) % FFT_SIZE;
            if self.write == 0 {
                self.filled = true;
            }
        }
    }

    /// Analyse the most recent block. Returns the three normalised levels.
    pub fn analyze(&mut self) -> BandLevels {
        // Copy the ring in time order (oldest first) and apply the window.
        let (a, b) = self.ring.split_at(self.write);
        for (dst, &src) in self.buffer.iter_mut().zip(b.iter().chain(a.iter())) {
            *dst = Complex::new(src, 0.0);
        }
        for (c, w) in self.buffer.iter_mut().zip(self.window.iter()) {
            c.re *= w;
        }

        self.fft
            .process_with_scratch(&mut self.buffer, &mut self.scratch);

        let energy = [
            self.band_energy(self.bass_bins),
            self.band_energy(self.mid_bins),
            self.band_energy(self.treble_bins),
        ];

        // Update the shared adaptive peak: jump up immediately, decay slowly.
        let loudest = energy.iter().cloned().fold(0.0f32, f32::max);
        if loudest > self.peak {
            self.peak = loudest;
        } else {
            self.peak = (self.peak * PEAK_DECAY).max(NOISE_FLOOR);
        }

        let mut out = BandLevels::default();
        let levels = [&mut out.bass, &mut out.mid, &mut out.treble];
        for i in 0..3 {
            *levels[i] = self.normalize(energy[i]);
        }
        out
    }

    /// Sum `|X(k)|²` over the half-open bin range, normalised by bin count so
    /// wider bands are not favoured.
    fn band_energy(&self, (lo, hi): (usize, usize)) -> f32 {
        let hi = hi.min(self.buffer.len());
        if lo >= hi {
            return 0.0;
        }
        let mut sum = 0.0;
        for c in &self.buffer[lo..hi] {
            sum += c.re * c.re + c.im * c.im;
        }
        sum / (hi - lo) as f32
    }

    /// Fill `out` with a per-bar spectrum: one level in `[0, 1]` per slot,
    /// mapping slot `i` of `out.len()` to a log-spread frequency bin (≈40 Hz to
    /// ≈16 kHz, so bass does not dominate). Each slot is normalised against its own
    /// adaptive peak so quiet and loud passages both fill the bars; the peaks decay
    /// slowly so the bars do not flicker.
    ///
    /// `out` is written in place (no allocation), so the audio thread can call it.
    pub fn spectrum(&mut self, out: &mut [f32]) {
        let n = out.len();
        if n == 0 {
            return;
        }
        let nyq = self.sample_rate * 0.5;
        let bins = self.buffer.len();
        let peak_len = self.bar_peaks.len();
        for (i, slot) in out.iter_mut().enumerate() {
            let t = i as f32 / (n - 1).max(1) as f32;
            // 40 Hz → 16 kHz, log-spread.
            let f = 40.0 * (16_000.0f32 / 40.0).powf(t.clamp(0.0, 1.0));
            let bin = ((f / nyq * bins as f32).round() as usize).clamp(1, bins.saturating_sub(1));
            let mag = self.buffer.get(bin).map_or(0.0, |c| c.re.hypot(c.im));
            // Adaptive peak per slot (the slot count is fixed at construction).
            let mut peak = self.bar_peaks[if peak_len == 0 { 0 } else { i % peak_len }];
            if mag > peak {
                peak = mag;
            } else {
                peak = (peak * PEAK_DECAY).max(NOISE_FLOOR);
            }
            if peak_len > 0 {
                self.bar_peaks[i % peak_len] = peak;
            }
            let norm = if peak > NOISE_FLOOR { mag / peak } else { 0.0 };
            *slot = (norm * self.sensitivity).clamp(0.0, 1.0);
        }
    }

    /// Turn raw band energy into a `[0, 1]` level relative to the shared peak.
    fn normalize(&self, energy: f32) -> f32 {
        let energy = if energy.is_finite() { energy } else { 0.0 };
        let norm = if self.peak > NOISE_FLOOR {
            energy / self.peak
        } else {
            0.0
        };
        (norm * self.sensitivity).clamp(0.0, 1.0)
    }

    /// True once at least one full block has been pushed.
    pub fn has_data(&self) -> bool {
        self.filled
    }
}

/// RMS of a slice (ignoring non-finite samples). Used for silence detection.
pub fn rms_of(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples
        .iter()
        .map(|&s| if s.is_finite() { s * s } else { 0.0 })
        .sum();
    (sum / samples.len() as f32).sqrt()
}

/// Sensitivity must be a positive finite multiplier; anything else is neutral.
fn valid_sensitivity(s: f32) -> f32 {
    if s.is_finite() && s > 0.0 {
        s
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build interleaved mono sine samples.
    fn sine(freq: f32, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (2.0 * std::f32::consts::PI * freq * t).sin()
            })
            .collect()
    }

    fn dominant(levels: BandLevels) -> &'static str {
        let mut best = ("bass", levels.bass);
        if levels.mid > best.1 {
            best = ("mid", levels.mid);
        }
        if levels.treble > best.1 {
            best = ("treble", levels.treble);
        }
        best.0
    }

    fn analyze_tone(freq: f32, rate: u32) -> BandLevels {
        let mut a = BandAnalyzer::new(rate, 1.0);
        let s = sine(freq, rate, FFT_SIZE * 4);
        a.push_interleaved(&s, 1);
        // Analyse a few times so the adaptive peak settles.
        let mut last = BandLevels::default();
        for _ in 0..5 {
            last = a.analyze();
        }
        last
    }

    #[test]
    fn bass_tone_is_dominant_in_bass() {
        let l = analyze_tone(100.0, 48_000);
        assert_eq!(dominant(l), "bass", "{l:?}");
        assert!(l.bass > 0.5, "{l:?}");
    }

    #[test]
    fn mid_tone_is_dominant_in_mid() {
        let l = analyze_tone(1_000.0, 48_000);
        assert_eq!(dominant(l), "mid", "{l:?}");
    }

    #[test]
    fn treble_tone_is_dominant_in_treble() {
        let l = analyze_tone(8_000.0, 48_000);
        assert_eq!(dominant(l), "treble", "{l:?}");
    }

    #[test]
    fn silence_gives_zero_levels() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        a.push_interleaved(&[0.0; FFT_SIZE], 1);
        let l = a.analyze();
        assert!(l.bass < 0.01 && l.mid < 0.01 && l.treble < 0.01, "{l:?}");
    }

    #[test]
    fn nan_input_is_safe() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let mut s = sine(440.0, 48_000, FFT_SIZE);
        s[0] = f32::NAN;
        a.push_interleaved(&s, 1);
        let l = a.analyze();
        assert!(l.bass.is_finite() && l.mid.is_finite() && l.treble.is_finite());
    }

    #[test]
    fn stereo_identical_equals_mono() {
        let mono = analyze_tone(1_000.0, 48_000);

        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(1_000.0, 48_000, FFT_SIZE * 4);
        let interleaved: Vec<f32> = s.iter().flat_map(|&x| [x, x]).collect();
        a.push_interleaved(&interleaved, 2);
        let mut stereo = BandLevels::default();
        for _ in 0..5 {
            stereo = a.analyze();
        }

        assert!(
            (mono.mid - stereo.mid).abs() < 0.05,
            "{mono:?} vs {stereo:?}"
        );
    }

    #[test]
    fn same_dominant_band_at_44k_and_96k() {
        for rate in [44_100u32, 96_000] {
            let l = analyze_tone(100.0, rate);
            assert_eq!(dominant(l), "bass", "rate {rate}: {l:?}");
            let l = analyze_tone(1_000.0, rate);
            assert_eq!(dominant(l), "mid", "rate {rate}: {l:?}");
        }
    }

    #[test]
    fn levels_are_clamped_to_unit_interval() {
        let mut a = BandAnalyzer::new(48_000, 20.0); // absurd sensitivity
        let s = sine(100.0, 48_000, FFT_SIZE * 8);
        a.push_interleaved(&s, 1);
        for _ in 0..10 {
            let l = a.analyze();
            assert!((0.0..=1.0).contains(&l.bass));
            assert!((0.0..=1.0).contains(&l.mid));
            assert!((0.0..=1.0).contains(&l.treble));
        }
    }

    #[test]
    fn set_sensitivity_scales_levels_live() {
        // A quiet mid tone next to a loud bass tone: mid sits well below 1, so
        // scaling it is visible (the loudest band always normalises to ~1).
        let rate = 48_000;
        let bass = sine(100.0, rate, FFT_SIZE * 4);
        let mid = sine(1_000.0, rate, FFT_SIZE * 4);
        let mix: Vec<f32> = bass.iter().zip(&mid).map(|(b, m)| b + 0.3 * m).collect();

        let mut a = BandAnalyzer::new(rate, 1.0);
        a.push_interleaved(&mix, 1);
        let mut base = BandLevels::default();
        for _ in 0..5 {
            base = a.analyze();
        }

        a.set_sensitivity(0.5);
        let half = a.analyze();
        assert!(
            (half.mid - base.mid * 0.5).abs() < 0.02,
            "{base:?} vs {half:?}"
        );

        // Invalid values fall back to neutral instead of breaking the output.
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            a.set_sensitivity(bad);
            let l = a.analyze();
            assert!((l.mid - base.mid).abs() < 0.02, "bad={bad}: {l:?}");
        }
    }

    #[test]
    fn spectrum_is_bounded_and_filled() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(1_000.0, 48_000, FFT_SIZE * 4);
        a.push_interleaved(&s, 1);
        for _ in 0..5 {
            a.analyze();
        }
        let mut spec = [0.0f32; crate::bars::SPECTRUM_BINS];
        a.spectrum(&mut spec);
        assert!(spec
            .iter()
            .all(|v| (0.0..=1.0).contains(v) && v.is_finite()));
        // A tone produces some energy somewhere, not a flat zero field.
        assert!(spec.iter().any(|&v| v > 0.1), "{spec:?}");
    }

    #[test]
    fn spectrum_of_silence_is_zero() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        a.push_interleaved(&[0.0; FFT_SIZE], 1);
        a.analyze();
        let mut spec = [1.0f32; crate::bars::SPECTRUM_BINS];
        a.spectrum(&mut spec);
        assert!(spec.iter().all(|&v| v < 0.01), "{spec:?}");
    }

    #[test]
    fn spectrum_does_not_allocate_or_panic_on_empty_slice() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(440.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        a.analyze();
        let mut empty: [f32; 0] = [];
        a.spectrum(&mut empty); // must not panic
    }

    #[test]
    fn rms_of_unit_sine_is_about_0_707() {
        // A full-scale sine has RMS = 1/√2 ≈ 0.707.
        let s = sine(1_000.0, 48_000, FFT_SIZE);
        let r = rms_of(&s);
        assert!(
            (r - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01,
            "rms {r}"
        );
    }

    #[test]
    fn rms_of_half_amplitude_sine_is_about_0_35() {
        let s: Vec<f32> = sine(1_000.0, 48_000, FFT_SIZE)
            .iter()
            .map(|x| x * 0.5)
            .collect();
        let r = rms_of(&s);
        assert!(
            (r - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01,
            "rms {r}"
        );
    }

    #[test]
    fn rms_ignores_nan() {
        let mut s = sine(1_000.0, 48_000, FFT_SIZE);
        s[10] = f32::NAN;
        assert!(rms_of(&s).is_finite());
    }
}
