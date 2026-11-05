//! VibeWave pure logic.
//!
//! Everything in this crate is deterministic and free of Windows/Tauri
//! dependencies, so it can be unit-tested on any machine. The binary crate
//! `vibewave` owns all FFI; this crate owns the math.
//!
//! Pipeline: `bands` (audio → 3 band levels) → `envelope` (per-frame smoothing)
//! → `compose` (levels → `GlowFrame` the renderer draws). The `bars` module is
//! the second visual language (spectrum bars around the edges). Colour comes from
//! `palette` / `cover_color`; the fixed look lives in `visual`. Supporting
//! pieces: `geometry`, `settings`, `backoff`, `rng`, `level_slot`, `silence`,
//! `glow_state`.

pub mod backoff;
pub mod bands;
pub mod bars;
pub mod compose;
pub mod cover_color;
pub mod envelope;
pub mod geometry;
pub mod glow_state;
pub mod level_slot;
pub mod palette;
pub mod rng;
pub mod settings;
pub mod silence;
pub mod visual;
