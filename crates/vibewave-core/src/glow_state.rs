//! Glow lifecycle state on the render thread.
//!
//! Turning the glow off the instant audio stops would look like a glitch, and
//! leaving it drawing forever wastes CPU. So we fade: `Active` while audio
//! plays, `Fading` for a short tail after silence begins, then `Stopped` — at
//! which point the renderer presents one blank frame and sleeps (0 FPS).
//!
//! Coming back is immediate: any non-silent signal snaps to `Active`, giving
//! the "instant return" the spec requires (FR-021).

/// How long the fade-out lasts.
pub const FADE_S: f32 = 1.5;
/// How fast fade snaps back up when audio returns.
pub const RESUME_S: f32 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlowState {
    Active,
    Fading,
    Stopped,
}

/// Owns the current fade level and state machine.
#[derive(Debug, Clone)]
pub struct Glow {
    state: GlowState,
    fade: f32,
}

impl Default for Glow {
    fn default() -> Self {
        Self {
            state: GlowState::Active,
            fade: 1.0,
        }
    }
}

impl Glow {
    pub fn state(&self) -> GlowState {
        self.state
    }

    /// Current fade multiplier in `[0, 1]`.
    pub fn fade(&self) -> f32 {
        self.fade
    }

    /// Must the renderer draw this frame?
    pub fn wants_render(&self) -> bool {
        self.state != GlowState::Stopped
    }

    /// Advance the machine by `dt` seconds given the latest silence flag.
    ///
    /// Returns the vector of transitions that happened, so the caller can log
    /// them (logging is kept out of the hot render loop itself).
    pub fn update(&mut self, silent: bool, dt: f32) -> Vec<Transition> {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        let before = self.state;
        match self.state {
            GlowState::Active => {
                if silent {
                    // Begin fading this very frame so the first quiet frame is
                    // not wasted; then run the fade maths below.
                    self.state = GlowState::Fading;
                    self.fade -= dt / FADE_S;
                    if self.fade <= 0.0 {
                        self.fade = 0.0;
                        self.state = GlowState::Stopped;
                    }
                } else {
                    self.fade = 1.0;
                }
            }
            GlowState::Fading => {
                if !silent {
                    self.state = GlowState::Active;
                    self.fade = (self.fade + dt / RESUME_S).min(1.0);
                } else {
                    self.fade -= dt / FADE_S;
                    if self.fade <= 0.0 {
                        self.fade = 0.0;
                        self.state = GlowState::Stopped;
                    }
                }
            }
            GlowState::Stopped => {
                if !silent {
                    self.state = GlowState::Active;
                    self.fade = (dt / RESUME_S).min(1.0).max(self.fade);
                }
            }
        }
        if before == self.state {
            Vec::new()
        } else {
            vec![Transition {
                from: before,
                to: self.state,
            }]
        }
    }
}

/// A state change worth logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: GlowState,
    pub to: GlowState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_active_and_fully_faded_in() {
        let g = Glow::default();
        assert_eq!(g.state(), GlowState::Active);
        assert_eq!(g.fade(), 1.0);
        assert!(g.wants_render());
    }

    #[test]
    fn silence_starts_fading_then_stops() {
        let mut g = Glow::default();
        let t = g.update(true, 0.0);
        assert_eq!(
            t,
            vec![Transition {
                from: GlowState::Active,
                to: GlowState::Fading
            }]
        );
        assert_eq!(g.state(), GlowState::Fading);
        // After FADE_S the fade reaches zero and we stop.
        g.update(true, FADE_S);
        assert_eq!(g.state(), GlowState::Stopped);
        assert_eq!(g.fade(), 0.0);
        assert!(!g.wants_render());
    }

    #[test]
    fn fade_decreases_monotonically() {
        let mut g = Glow::default();
        g.update(true, 0.0);
        let mut last = 1.0;
        for _ in 0..10 {
            g.update(true, FADE_S / 10.0);
            assert!(g.fade() <= last + 1e-6);
            last = g.fade();
        }
    }

    #[test]
    fn resumed_snaps_back_to_active_quickly() {
        let mut g = Glow::default();
        g.update(true, FADE_S); // → Stopped
        assert_eq!(g.state(), GlowState::Stopped);
        g.update(false, RESUME_S);
        assert_eq!(g.state(), GlowState::Active);
        assert!((g.fade() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn only_stopped_stops_rendering() {
        let mut g = Glow::default();
        assert!(g.wants_render());
        g.update(true, FADE_S / 2.0);
        assert!(g.wants_render()); // Fading still renders
        g.update(true, FADE_S / 2.0 + 0.001);
        assert!(!g.wants_render()); // Stopped
    }

    #[test]
    fn non_finite_dt_is_safe() {
        let mut g = Glow::default();
        g.update(true, f32::NAN);
        assert_eq!(g.state(), GlowState::Fading);
        assert!(g.fade().is_finite());
    }
}
