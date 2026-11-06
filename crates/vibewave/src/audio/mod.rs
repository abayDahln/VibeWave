//! The audio thread: capture system audio and publish band levels.
//!
//! Runs on its own thread (COM multithreaded). It polls the loopback stream
//! every 10 ms, feeds samples to the [`BandAnalyzer`], and publishes the latest
//! levels to a lock-free [`LevelSlot`] the render thread reads. It never blocks
//! the render thread and never allocates in the hot loop.
//!
//! Recovery (device change, unplug, service restart) is added by task US5; for
//! now a failed open retries on a fixed short delay.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vibewave_core::backoff::Backoff;
use vibewave_core::bands::{BandAnalyzer, BandLevels};
use vibewave_core::level_slot::LevelSlot;
use vibewave_core::silence::{SilenceDetector, SilenceEvent};

use crate::log;

use capture::{Capture, CaptureError};

pub mod capture;

/// How long the loop sleeps between polls (research R-3/S-2).
const POLL: Duration = Duration::from_millis(10);
/// How often to check whether the default output device changed (FR-016).
const DEVICE_CHECK: Duration = Duration::from_secs(1);

/// Shared state the audio thread writes and the render thread reads.
pub struct Shared {
    pub levels: Arc<LevelSlot>,
    /// Set while the thread is running; cleared on shutdown.
    pub running: AtomicBool,
    /// Band sensitivity as `f32` bits. The settings window writes it; the audio
    /// loop reads it every poll. An atomic (not a lock) keeps the hot loop
    /// non-blocking; `Relaxed` is enough because it is a lone value with no
    /// other data that must be seen in order with it.
    sensitivity: AtomicU32,
}

impl Shared {
    pub fn new(sensitivity: f32) -> Self {
        Self {
            levels: Arc::new(LevelSlot::new()),
            running: AtomicBool::new(true),
            sensitivity: AtomicU32::new(sensitivity.to_bits()),
        }
    }

    /// Change the sensitivity; the audio loop applies it on its next poll.
    pub fn set_sensitivity(&self, value: f32) {
        self.sensitivity.store(value.to_bits(), Ordering::Relaxed);
    }

    fn sensitivity(&self) -> f32 {
        f32::from_bits(self.sensitivity.load(Ordering::Relaxed))
    }
}

/// Spawn the audio thread and return the shared handle.
pub fn spawn(sensitivity: f32) -> Arc<Shared> {
    let shared = Arc::new(Shared::new(sensitivity));
    let handle = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("vibewave-audio".into())
        .spawn(move || run_loop(&handle))
        .expect("spawn audio thread");
    shared
}

/// The audio thread body.
fn run_loop(shared: &Shared) {
    capture::init_com();
    let en = match capture::enumerator() {
        Ok(en) => en,
        Err(e) => {
            log::error(&format!("audio: cannot create device enumerator: {e}"));
            shared.levels.store(0.0, 0.0, 0.0, 0, true);
            return;
        }
    };

    let mut backoff = Backoff::default();
    let mut next_retry = Instant::now();
    let mut cap: Option<Capture> = None;
    let mut analyzer: Option<BandAnalyzer> = None;
    // Reused bar-spectrum buffer (no per-poll allocation).
    let mut spectrum = [0.0f32; vibewave_core::bars::SPECTRUM_BINS];
    // Time of the previous spectrum update, for the envelope's `dt`.
    let mut last_spectrum = Instant::now();
    let mut silence = SilenceDetector::new(0.0);
    let start = Instant::now();
    let mut last_device_check = Instant::now();
    let mut last_logged_retry = false;

    while shared.running.load(Ordering::Relaxed) {
        let now_s = start.elapsed().as_secs_f32();

        // (Re)open the stream if needed and the backoff has elapsed.
        if cap.is_none() && Instant::now() >= next_retry {
            match Capture::open(&en) {
                Ok(c) => {
                    log::info(&format!(
                        "audio: capture opened device={} rate={} channels={}",
                        c.info.id, c.info.sample_rate, c.info.channels
                    ));
                    analyzer = Some(BandAnalyzer::new(c.info.sample_rate, shared.sensitivity()));
                    cap = Some(c);
                    backoff.reset();
                    last_logged_retry = false;
                }
                Err(e) => {
                    let delay = backoff.next_delay();
                    // Log only the first failure of a run so a long outage does
                    // not spam the log (FR-024).
                    if !last_logged_retry {
                        log::warn(&format!("audio: open failed: {e}; retrying"));
                        last_logged_retry = true;
                    }
                    next_retry = Instant::now() + Duration::from_secs_f32(delay);
                    shared.levels.store(0.0, 0.0, 0.0, 0, true);
                }
            }
        }

        // Drain all ready packets; feed samples to the analyzer.
        let mut failed: Option<CaptureError> = None;
        if let (Some(c), Some(a)) = (&cap, analyzer.as_mut()) {
            // Plain field write; cheap enough to do every poll (FR-15 live).
            a.set_sensitivity(shared.sensitivity());
            let mut last_qpc = 0u64;
            let mut levels = BandLevels::default();
            let mut had_packet = false;
            let mut loudest = 0.0f32;
            let mut resumed = false;
            let step = c.drain(&mut |p: capture::Packet<'_>| {
                if p.frames == 0 {
                    return;
                }
                had_packet = true;
                last_qpc = p.qpc_100ns;
                loudest = loudest.max(p.rms);
                if !p.samples.is_empty() {
                    a.push_interleaved(p.samples, c.info.channels);
                }
            });
            if let Err(e) = step {
                failed = Some(e);
            }

            // "Silent" means no packet arrived, or the packets are quiet. WASAPI
            // loopback keeps sending silent packets while paused, so the packet
            // flag alone is not enough to detect a pause.
            let quiet = !had_packet || loudest <= vibewave_core::silence::SILENCE_RMS;
            if had_packet && !quiet && a.has_data() {
                levels = a.analyze();
                // Per-bar spectrum for the Bars theme, published alongside the
                // three band levels (lock-free, no allocation). `dt` paces the
                // per-slot attack/release envelope.
                let dt = last_spectrum.elapsed().as_secs_f32();
                last_spectrum = Instant::now();
                a.spectrum(&mut spectrum, dt);
                shared.levels.store_spectrum(&spectrum);
            } else {
                // Paused/stopped (no audio): hold briefly, then collapse the bars
                // so the beat does not freeze and starts from the bottom when
                // audio returns.
                let dt = last_spectrum.elapsed().as_secs_f32();
                last_spectrum = Instant::now();
                a.decay_spectrum(&mut spectrum, dt);
                shared.levels.store_spectrum(&spectrum);
            }
            // Silence transition (resume fires immediately on loud packets).
            let ev = if had_packet {
                silence.update(loudest, now_s)
            } else {
                silence.tick(now_s)
            };
            if ev == SilenceEvent::Resumed {
                resumed = true;
            }
            if resumed {
                log::info("audio: resumed");
            }
            shared.levels.store(
                levels.bass,
                levels.mid,
                levels.treble,
                last_qpc,
                silence.is_silent(),
            );
        }

        if let Some(e) = failed {
            log::warn(&format!("audio: capture error: {e}; recovering"));
            cap = None;
            analyzer = None;
            next_retry = Instant::now();
            shared.levels.store(0.0, 0.0, 0.0, 0, true);
        }

        // Follow default output-device changes by polling its id.
        if last_device_check.elapsed() >= DEVICE_CHECK {
            last_device_check = Instant::now();
            if let (Some(c), Ok(id)) = (&cap, capture::default_device_id(&en)) {
                if id != c.info.id {
                    log::info("audio: default device changed; reopening");
                    cap = None;
                    analyzer = None;
                    next_retry = Instant::now();
                    shared.levels.store(0.0, 0.0, 0.0, 0, true);
                }
            }
        }

        std::thread::sleep(POLL);
    }

    log::info("audio: thread stopped");
}
