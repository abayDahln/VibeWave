//! Render statistics for the `--stats` flag.
//!
//! Every five seconds the render loop prints FPS, the worst frame time, and the
//! audio→visual latency distribution. Latency is `now − packet_timestamp` plus
//! one DWM composition frame (16.7 ms), matching research R-11: the packet
//! timestamp is when the sound left the mixer, and DWM adds a frame before the
//! pixels are visible.

use std::time::Instant;

use crate::clock::{qpc_freq, qpc_now_100ns};

/// One reporting window's numbers.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatsSnapshot {
    pub fps: f32,
    pub worst_frame_ms: f32,
    pub latency_p50_ms: f32,
    pub latency_p95_ms: f32,
    pub latency_max_ms: f32,
    pub frames: u32,
}

/// Extra latency added by DWM composition before pixels are on screen.
const DWM_MS: f32 = 16.7;
/// Reporting period.
const REPORT_S: f32 = 5.0;
/// Cap on retained latency samples (≈ one report window at 240 FPS).
const MAX_SAMPLES: usize = 2048;

/// Accumulates per-frame samples and produces a snapshot every `REPORT_S`.
pub struct Stats {
    freq: i64,
    window_start: Instant,
    frames: u32,
    worst_frame_ms: f32,
    latencies: Vec<f32>,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            freq: qpc_freq(),
            window_start: Instant::now(),
            frames: 0,
            worst_frame_ms: 0.0,
            latencies: Vec::with_capacity(MAX_SAMPLES),
        }
    }

    /// Record one frame. `packet_qpc` is the last audio packet's timestamp in
    /// 100 ns units (0 if no packet yet); `dt` is the frame duration in seconds.
    pub fn on_frame(&mut self, packet_qpc: u64, dt: f32) {
        self.frames += 1;
        if dt.is_finite() && dt > 0.0 {
            self.worst_frame_ms = self.worst_frame_ms.max(dt * 1000.0);
        }
        if packet_qpc > 0 && self.latencies.len() < MAX_SAMPLES {
            let now = qpc_now_100ns(self.freq);
            let lat_ms = now.saturating_sub(packet_qpc) as f32 / 10_000.0 + DWM_MS;
            self.latencies.push(lat_ms);
        }
    }

    /// Has the reporting period elapsed?
    pub fn due(&self) -> bool {
        self.window_start.elapsed().as_secs_f32() >= REPORT_S
    }

    /// Take the current window's snapshot and start a new window.
    pub fn take_snapshot(&mut self) -> StatsSnapshot {
        let elapsed = self.window_start.elapsed().as_secs_f32().max(1e-3);
        let fps = self.frames as f32 / elapsed;

        let mut sorted = std::mem::take(&mut self.latencies);
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let pick = |q: f32| -> f32 {
            if sorted.is_empty() {
                0.0
            } else {
                let idx = (((sorted.len() - 1) as f32) * q).round() as usize;
                sorted[idx]
            }
        };

        let snap = StatsSnapshot {
            fps,
            worst_frame_ms: self.worst_frame_ms,
            latency_p50_ms: pick(0.5),
            latency_p95_ms: pick(0.95),
            latency_max_ms: sorted.last().copied().unwrap_or(0.0),
            frames: self.frames,
        };

        self.frames = 0;
        self.worst_frame_ms = 0.0;
        self.latencies = Vec::with_capacity(MAX_SAMPLES);
        self.window_start = Instant::now();
        snap
    }
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_frame_count_and_worst_frame() {
        let mut s = Stats::new();
        for _ in 0..300 {
            s.on_frame(0, 1.0 / 60.0);
        }
        let snap = s.take_snapshot();
        assert_eq!(snap.frames, 300);
        assert!(snap.fps > 0.0);
        assert!(snap.worst_frame_ms >= 16.0 && snap.worst_frame_ms <= 17.0);
    }

    #[test]
    fn snapshot_resets_the_window() {
        let mut s = Stats::new();
        for _ in 0..10 {
            s.on_frame(0, 0.016);
        }
        let _ = s.take_snapshot();
        let second = s.take_snapshot();
        assert_eq!(second.frames, 0);
        assert_eq!(second.worst_frame_ms, 0.0);
    }

    #[test]
    fn latency_samples_are_ordered() {
        let mut s = Stats::new();
        // packet_qpc far in the past → large, finite latency.
        for _ in 0..100 {
            s.on_frame(1, 1.0 / 60.0);
        }
        let snap = s.take_snapshot();
        assert!(snap.latency_p50_ms >= DWM_MS);
        assert!(snap.latency_p50_ms <= snap.latency_p95_ms);
        assert!(snap.latency_p95_ms <= snap.latency_max_ms);
    }

    #[test]
    fn no_packets_no_latency_samples() {
        let mut s = Stats::new();
        for _ in 0..10 {
            s.on_frame(0, 1.0 / 60.0);
        }
        let snap = s.take_snapshot();
        assert_eq!(snap.latency_max_ms, 0.0);
    }
}
