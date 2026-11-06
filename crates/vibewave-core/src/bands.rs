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

/// Floor for the bar-spectrum normaliser: below this the per-slot peak is
/// treated as silence, so noise cannot be amplified into full-height bars.
const PEAK_FLOOR: f32 = 0.05;

/// Bar-spectrum attack time in seconds: how fast a bar rises toward the music.
const SPECTRUM_ATTACK_S: f32 = 0.030;
/// Bar-spectrum release time in seconds: how gently a bar falls. Longer than the
/// attack so the motion reads as smooth rather than jittery.
const SPECTRUM_RELEASE_S: f32 = 0.150;

/// How long the bars are held after the music pauses before they start to fall.
/// Short, so a pause reads as responsive rather than "stuck".
const PAUSE_HOLD_S: f32 = 1.0;
/// How fast the bars then collapse to nothing once the hold has elapsed. Fast,
/// so the bars visibly shrink right after the hold and are gone within a few
/// seconds (well inside the user's ~5 s expectation).
const PAUSE_FALL_S: f32 = 0.15;

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
    /// Smoothed bar-spectrum value per slot (the attack/release state).
    bar_smooth: [f32; crate::bars::SPECTRUM_BINS],
    /// Seconds of continuous quiet/no-audio, driving the pause behaviour.
    quiet_s: f32,
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
            bar_smooth: [0.0; crate::bars::SPECTRUM_BINS],
            quiet_s: 0.0,
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
    /// mapping slot `i` of `out.len()` to a log-spread frequency band (≈40 Hz to
    /// ≈16 kHz, so bass does not dominate).
    ///
    /// To keep the bars steady instead of jittery ("grasak-grusuk"), each slot is
    /// smoothed twice:
    /// 1. **Band averaging** — the magnitude is averaged over a short run of bins
    ///    around the target, so a single noisy FFT bin cannot swing a bar.
    /// 2. **Adaptive peak with a floor** — the level is normalised against a slow
    ///    per-slot peak (`max(PEAK_FLOOR, peak)`) so quiet and loud passages both
    ///    fill the bars, but a near-silent peak cannot amplify noise.
    /// 3. **Attack/release envelope** per slot (`dt`-based, frame-rate
    ///    independent): bars rise quickly and fall gently.
    ///
    /// `out` is written in place (no allocation), so the audio thread can call it.
    /// `dt` is the seconds since the previous call.
    pub fn spectrum(&mut self, out: &mut [f32], dt: f32) {
        let n = out.len();
        if n == 0 {
            return;
        }
        let nyq = self.sample_rate * 0.5;
        let bins = self.buffer.len();
        let peak_len = self.bar_peaks.len();
        let dt = if dt.is_finite() && dt > 0.0 {
            dt
        } else {
            1.0 / 60.0
        };
        // Audio is present: clear the pause timer.
        self.quiet_s = 0.0;
        for (i, slot) in out.iter_mut().enumerate() {
            let t = i as f32 / (n - 1).max(1) as f32;
            // 40 Hz → 16 kHz, log-spread.
            let f = 40.0 * (16_000.0f32 / 40.0).powf(t.clamp(0.0, 1.0));
            let bin = (f / nyq * bins as f32).round() as usize;
            // Average a small band of bins around the target so a single noisy
            // bin cannot make the bar jump. The band widens with frequency so
            // high (sparse-energy) bins are averaged over more of them.
            let half = (1 + (bin / 48)).min(6);
            let lo = bin.saturating_sub(half).max(1);
            let hi = (bin + half + 1).min(bins);
            let mag = if lo < hi {
                let sum: f32 = self.buffer[lo..hi].iter().map(|c| c.re.hypot(c.im)).sum();
                sum / (hi - lo) as f32
            } else {
                0.0
            };

            let idx = i % peak_len.max(1);
            // Adaptive peak: jump up immediately, decay slowly; floor keeps a
            // near-silent slot from amplifying noise into a full bar.
            let mut peak = self.bar_peaks[idx];
            peak = if mag > peak {
                mag
            } else {
                (peak * PEAK_DECAY).max(NOISE_FLOOR)
            };
            self.bar_peaks[idx] = peak;
            let scale = peak.max(PEAK_FLOOR);
            let target = (mag / scale * self.sensitivity).clamp(0.0, 1.0);

            // Attack/release envelope: fast rise, gentle fall, frame-rate
            // independent (`1 - exp(-dt / tau)`).
            let tau = if target > self.bar_smooth[idx] {
                SPECTRUM_ATTACK_S
            } else {
                SPECTRUM_RELEASE_S
            };
            let coeff = 1.0 - (-dt / tau).exp();
            let v = self.bar_smooth[idx] + (target - self.bar_smooth[idx]) * coeff;
            let v = if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            };
            self.bar_smooth[idx] = v;
            *slot = v;
        }
    }

    /// Collapse the bar spectrum when the music pauses (quiet packets or no
    /// packets at all).
    ///
    /// Behaviour after the sound stops:
    /// 1. **Hold** for `PAUSE_HOLD_S` (~1 s) — the bars stay put, so a momentary
    ///    dip does not make them flutter.
    /// 2. **Fall fast** with time constant `PAUSE_FALL_S` once the hold elapses,
    ///    so the bars clearly shrink right after the hold and reach zero within a
    ///    few seconds (well inside ~5 s).
    ///
    /// `dt` is the seconds since the previous call. Must only be called while the
    /// music is quiet — [`BandAnalyzer::spectrum`] resets the timer otherwise.
    pub fn decay_spectrum(&mut self, out: &mut [f32], dt: f32) {
        let dt = if dt.is_finite() && dt > 0.0 {
            dt
        } else {
            1.0 / 60.0
        };
        self.quiet_s += dt;
        if self.quiet_s < PAUSE_HOLD_S {
            // Still within the hold: report the current (frozen) values.
            for (slot, state) in out.iter_mut().zip(self.bar_smooth.iter()) {
                *slot = *state;
            }
            return;
        }
        // Past the hold: collapse quickly.
        let coeff = 1.0 - (-dt / PAUSE_FALL_S).exp();
        for (slot, state) in out.iter_mut().zip(self.bar_smooth.iter_mut()) {
            let v = *state * (1.0 - coeff);
            let v = if v.is_finite() && v > 0.0005 { v } else { 0.0 };
            *state = v;
            *slot = v;
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
        let s = sine(1_000.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        let mut spec = [0.0f32; crate::bars::SPECTRUM_BINS];
        // Run several polls so the envelope settles toward the tone.
        for _ in 0..200 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        assert!(spec
            .iter()
            .all(|v| (0.0..=1.0).contains(v) && v.is_finite()));
        // A tone produces some energy somewhere, not a flat zero field.
        assert!(spec.iter().any(|&v| v > 0.2), "{spec:?}");
    }

    #[test]
    fn spectrum_of_silence_is_zero() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        a.push_interleaved(&[0.0; FFT_SIZE], 1);
        a.analyze();
        let mut spec = [1.0f32; crate::bars::SPECTRUM_BINS];
        a.spectrum(&mut spec, 1.0 / 100.0);
        assert!(spec.iter().all(|&v| v < 0.01), "{spec:?}");
    }

    #[test]
    fn spectrum_release_is_smoother_than_attack_kick() {
        // Bars must fall back gently after the tone stops, not snap to zero.
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(1_000.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        let mut spec = [0.0f32; crate::bars::SPECTRUM_BINS];
        for _ in 0..200 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        let peak_before = spec.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak_before > 0.2, "tone should fill some bars");

        // Feed silence and take one short poll: the bars should still be up
        // (released), not already collapsed to zero.
        a.push_interleaved(&[0.0; FFT_SIZE], 1);
        a.analyze();
        a.spectrum(&mut spec, 1.0 / 100.0);
        let after = spec.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            after > peak_before * 0.5,
            "release too abrupt: {after} vs {peak_before}"
        );
        // Several polls later it has fallen near zero.
        for _ in 0..50 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        assert!(spec.iter().cloned().fold(0.0f32, f32::max) < 0.05);
    }

    #[test]
    fn spectrum_is_steady_for_a_constant_tone() {
        // A steady tone must give a steady bar (no large frame-to-frame jumps),
        // which is what stops the "grasak-grusuk" jitter.
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(1_000.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        let mut spec = [0.0f32; crate::bars::SPECTRUM_BINS];
        for _ in 0..30 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        let mut max_jump: f32 = 0.0;
        for _ in 0..40 {
            let prev = spec;
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
            for i in 0..spec.len() {
                max_jump = max_jump.max((spec[i] - prev[i]).abs());
            }
        }
        assert!(max_jump < 0.12, "bars jitter too much: {max_jump}");
    }

    #[test]
    fn decay_holds_then_falls_to_zero() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(1_000.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        let mut spec = [0.0f32; crate::bars::SPECTRUM_BINS];
        for _ in 0..200 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        let peak = spec.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak > 0.2);

        // During the hold (~1 s) the bars stay put.
        for _ in 0..90 {
            a.decay_spectrum(&mut spec, 1.0 / 100.0);
        }
        let during_hold = spec.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            (during_hold - peak).abs() < 0.02,
            "bars must hold during the grace: {during_hold} vs {peak}"
        );

        // After the hold they collapse quickly, and are effectively gone within a
        // few seconds, never below zero.
        for _ in 0..400 {
            a.decay_spectrum(&mut spec, 1.0 / 100.0);
            assert!(spec
                .iter()
                .all(|v| (0.0..=1.0).contains(v) && v.is_finite()));
        }
        assert!(spec.iter().cloned().fold(0.0f32, f32::max) < 0.02);
    }

    #[test]
    fn decay_is_quick_right_after_the_hold() {
        // The user wants the bars to visibly shrink right after the ~1 s delay.
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(1_000.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        let mut spec = [0.0f32; crate::bars::SPECTRUM_BINS];
        for _ in 0..200 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        let peak = spec.iter().cloned().fold(0.0f32, f32::max);
        // Cross the hold (~1 s), then a few more polls should drop it well below
        // half — the shrink is fast once the hold ends.
        for _ in 0..110 {
            a.decay_spectrum(&mut spec, 1.0 / 100.0);
        }
        let after = spec.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            after < peak * 0.6,
            "should shrink fast after hold: {after}/{peak}"
        );
    }

    #[test]
    fn decay_then_resume_starts_from_the_bottom() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(1_000.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        let mut spec = [0.0f32; crate::bars::SPECTRUM_BINS];
        for _ in 0..200 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        // Pause: decay well past the hold so the bars are actually at zero.
        for _ in 0..300 {
            a.decay_spectrum(&mut spec, 1.0 / 100.0);
        }
        // Resume: the first poll must start well below full and build up, not
        // jump straight to the top.
        a.analyze();
        a.spectrum(&mut spec, 1.0 / 100.0);
        let first = spec.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            first < 0.5,
            "bars must rise from the bottom on resume: {first}"
        );
        // After the attack settles, the bar is clearly higher than that first step.
        for _ in 0..100 {
            a.analyze();
            a.spectrum(&mut spec, 1.0 / 100.0);
        }
        let settled = spec.iter().cloned().fold(0.0f32, f32::max);
        assert!(settled > first, "bars should build up after resume");
    }

    #[test]
    fn spectrum_does_not_allocate_or_panic_on_empty_slice() {
        let mut a = BandAnalyzer::new(48_000, 1.0);
        let s = sine(440.0, 48_000, FFT_SIZE);
        a.push_interleaved(&s, 1);
        a.analyze();
        let mut empty: [f32; 0] = [];
        a.spectrum(&mut empty, 1.0 / 60.0); // must not panic
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
