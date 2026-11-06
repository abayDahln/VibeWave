//! Glow composition: turn band levels into the geometry the renderer draws.
//!
//! This is pure maths. Given the overlay rectangle, the current band levels,
//! the elapsed time, a fade multiplier and the chosen [`Shape`], it produces a
//! [`GlowFrame`]: a flat list of [`Segment`]s (short strips along the four
//! edges) plus a few [`Sparkle`]s. The renderer only has to draw them — it
//! makes no layout decisions.
//!
//! How the three bands map to three motion characters (research R-6, FR-008):
//! - **bass** → global thickness and opacity: a slow, heavy pulse.
//! - **mid** → a travelling sine that modulates thickness along the perimeter.
//! - **treble** → short-lived bright sparkles near the inner edge.
//!
//! Everything travels around the perimeter in one direction
//! ([`Direction::Clockwise`] or [`Direction::CounterClockwise`]). The speed is
//! the shape's base speed boosted by the music (bass + mid), so a louder
//! passage sweeps faster. The travel phase is accumulated frame by frame in
//! [`GlowFrame::phase`], which keeps the motion continuous as the speed varies.
//!
//! The [`Palette`] then provides the colours: one colour gives a solid glow,
//! several give a smooth gradient that flows around the frame.
//!
//! Everything travels around the perimeter in one direction
//! ([`Direction::Clockwise`] or [`Direction::CounterClockwise`]). The speed is
//! boosted by the music (bass + mid), so a louder passage sweeps faster. The
//! travel phase is accumulated frame by frame in [`GlowFrame::phase`], which
//! keeps the motion continuous as the speed varies.

use crate::palette::Palette;
use crate::visual::{self, Direction};

/// Which screen edge a segment belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// One strip of glow along an edge.
///
/// `start`/`end` are pixel positions along the edge (0 = left for top/bottom,
/// 0 = top for left/right). `thickness` is how far it reaches inward from the
/// edge; `opacity` is its alpha; `color` is its linear-sRGB value (0–1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub edge: Edge,
    pub start: f32,
    pub end: f32,
    pub thickness: f32,
    pub opacity: f32,
    pub color: [f32; 3],
}

/// A short bright point (treble) at `(x, y)` in overlay-local pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sparkle {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub opacity: f32,
    pub color: [f32; 3],
}

/// Maximum number of sparkles kept at once (fixed capacity, no per-frame growth).
pub const MAX_SPARKLES: usize = 64;
/// Target length of one perimeter segment in pixels.
const SEGMENT_PX: f32 = 16.0;

/// The composed frame. Reused across frames so steady-state drawing allocates
/// nothing: `compose` clears and refills the existing `Vec`s.
///
/// `phase` is the accumulated travel position around the perimeter, in turns
/// (`0..1`). It is stored here rather than derived from wall-clock time because
/// the rotation speed changes with the music: accumulating `speed * dt` each
/// frame keeps the motion continuous even as the speed varies, which a
/// `time * speed` formula could not.
#[derive(Debug, Default)]
pub struct GlowFrame {
    pub segments: Vec<Segment>,
    pub sparkles: Vec<Sparkle>,
    /// Travel phase around the perimeter, in turns (`0..1`), wrapped.
    pub phase: f32,
}

impl GlowFrame {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            sparkles: Vec::with_capacity(MAX_SPARKLES),
            phase: 0.0,
        }
    }
}

/// Tunable composition parameters (subset of `Settings` in play this feature).
#[derive(Debug, Clone, Copy)]
pub struct ComposeParams {
    /// Nominal glow thickness in pixels at 96 DPI (before DPI scaling).
    pub thickness_px: f32,
    /// Global intensity multiplier on opacity.
    pub intensity: f32,
    /// Colours to paint the glow with (cover art, custom, or default look).
    pub palette: Palette,
    /// Travel direction around the perimeter.
    pub direction: Direction,
}

/// Fixed persistence for sparkles: each sparkle has a life of at most this long.
const SPARKLE_LIFE_S: f32 = 0.15;

/// Everything that varies frame to frame.
#[derive(Debug, Clone, Copy)]
pub struct FrameInput {
    /// Smoothed band levels `(bass, mid, treble)` in `[0, 1]`.
    pub levels: [f32; 3],
    /// Elapsed seconds since start (used for the sparkle life cycle).
    pub t_s: f32,
    /// Real frame time in seconds, used to advance the travel phase.
    pub dt: f32,
    /// Glow-state multiplier in `[0, 1]` (1 = fully visible).
    pub fade: f32,
}

/// Compose one frame into `frame`.
///
/// `width`/`height` are the overlay size in physical pixels; `params` are the
/// slow-changing settings; `input` holds the per-frame values. The frame's
/// `Vec`s are cleared and refilled, so after a warm-up no allocation happens.
pub fn compose(
    frame: &mut GlowFrame,
    width: f32,
    height: f32,
    params: &ComposeParams,
    input: &FrameInput,
) {
    let [bass, mid, treble] = sanitize(input.levels);
    let fade = if input.fade.is_finite() {
        input.fade.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let t_s = if input.t_s.is_finite() {
        input.t_s
    } else {
        0.0
    };
    let dt = if input.dt.is_finite() && input.dt > 0.0 {
        input.dt
    } else {
        0.0
    };

    frame.segments.clear();
    frame.sparkles.clear();

    if width <= 0.0 || height <= 0.0 || fade <= 0.0 {
        return;
    }

    let palette = params.palette;
    let direction = params.direction;
    let base_thickness = (params.thickness_px * visual::THICKNESS_SCALE).max(1.0);
    // Bass drives a slow, heavy pulse in overall thickness (0.4×..1.4×).
    let bass_gain = 0.4 + bass * 1.0;
    let global_thickness = base_thickness * bass_gain;
    let intensity = if params.intensity.is_finite() {
        params.intensity.max(0.0)
    } else {
        1.0
    };

    // Advance the travel phase. Speed = the base speed boosted by the music
    // (bass and mid together), with a floor so the glow keeps drifting even
    // during quiet passages. The direction flips the sign. Accumulating
    // `speed * dt` (instead of `time * speed`) keeps motion continuous when the
    // speed changes.
    let drive = (bass + mid) * 0.5;
    let speed = visual::BASE_SPEED * (1.0 + visual::SPEED_DRIVE * drive) * direction.sign();
    frame.phase = (frame.phase + speed * dt).rem_euclid(1.0);

    // Perimeter order matters: top → right → bottom → left, each traversed in
    // increasing coordinate order. Clockwise follows that order; the phase sign
    // carries the direction, so the wave and colour travel the other way for
    // CounterClockwise.
    let perimeter = 2.0 * (width + height);
    let edges = [
        (Edge::Top, width, 0.0),
        (Edge::Right, height, width),
        (Edge::Bottom, width, width + height),
        (Edge::Left, height, 2.0 * width + height),
    ];

    let flow = Flow {
        phase: frame.phase,
        direction,
        palette,
        t_s,
    };

    for (edge, len, offset) in edges {
        let ctx = EdgeContext {
            perimeter,
            thickness: global_thickness,
            mid,
            intensity,
            fade,
            phase: flow.phase,
            palette,
            wave_amp: visual::WAVE_AMPLITUDE,
            roughness: visual::ROUGHNESS,
        };
        compose_edge(frame, edge, len, offset, &ctx);
    }

    compose_sparkles(frame, width, height, treble, fade, &flow);
}

/// Per-frame values shared by every edge.
struct EdgeContext {
    perimeter: f32,
    thickness: f32,
    mid: f32,
    intensity: f32,
    fade: f32,
    /// Travel phase in turns (`0..1`).
    phase: f32,
    palette: Palette,
    /// Mid-wave depth (see [`visual::WAVE_AMPLITUDE`]).
    wave_amp: f32,
    /// Jagged-detail weight (0 = smooth sine; see [`visual::ROUGHNESS`]).
    roughness: f32,
}

/// Mid-wave shape at angle `theta`, in `[-1, 1]`.
///
/// `roughness == 0` is a plain sine. As roughness rises, a 5× finer sine is
/// mixed in (small ripples riding on the big wave) and the peaks are pushed
/// toward ±1 with a power below 1, which flattens crests into harder edges.
/// Dividing by `1 + 0.5 * r` keeps the sum inside `[-1, 1]`.
fn wave_shape(theta: f32, roughness: f32) -> f32 {
    let r = roughness;
    let w = (theta.sin() + r * 0.5 * (5.0 * theta).sin()) / (1.0 + 0.5 * r);
    w.signum() * w.abs().powf(1.0 - 0.5 * r)
}

/// Fill one edge with segments.
fn compose_edge(frame: &mut GlowFrame, edge: Edge, len: f32, offset: f32, ctx: &EdgeContext) {
    if len <= 0.0 {
        return;
    }
    let EdgeContext {
        perimeter,
        thickness: base_thickness,
        mid,
        intensity,
        fade,
        phase,
        palette,
        wave_amp,
        roughness,
    } = *ctx;
    // Number of segments: at least 1, proportional to edge length.
    let count = (len / SEGMENT_PX).ceil().max(1.0) as usize;
    let seg_len = len / count as f32;
    // Mid wave: two crests travel around the perimeter, offset by the phase.
    let wave_k = std::f32::consts::TAU * 2.0;
    let phase_rad = phase * std::f32::consts::TAU;

    for i in 0..count {
        let start = i as f32 * seg_len;
        let end = start + seg_len;
        let center = offset + start + seg_len * 0.5;
        // Mid wave multiplier in [1 - amp, 1 + amp].
        let amp = mid * wave_amp;
        let wave = wave_shape(center / perimeter * wave_k - phase_rad, roughness);
        let mid_mult = 1.0 + amp * wave;
        let thickness = (base_thickness * mid_mult).max(0.0);

        // Colour position travels with the same phase, so the gradient itself
        // sweeps around the frame in the chosen direction.
        let travel = (center / perimeter - phase).rem_euclid(1.0);
        let color = palette.at(travel);
        let opacity = (fade * intensity).clamp(0.0, 1.0);

        frame.segments.push(Segment {
            edge,
            start,
            end,
            thickness,
            opacity,
            color,
        });
    }
}

/// The shared travel state every moving element uses: where the flow is, which
/// way it goes, and the palette that colours it.
#[derive(Debug, Clone, Copy)]
struct Flow {
    /// Travel phase in turns (`0..1`).
    phase: f32,
    direction: Direction,
    palette: Palette,
    /// Elapsed seconds (for the sparkle life cycle).
    t_s: f32,
}

/// Treble sparkles: short-lived bright points that travel around the perimeter.
///
/// Sparkles are spread evenly around the frame and all advance with the same
/// travel phase as the glow, so they read as a regular sweep in the chosen
/// direction rather than random jumps. Their position is deterministic: a fixed
/// offset per sparkle (derived from its index) plus the shared phase.
fn compose_sparkles(
    frame: &mut GlowFrame,
    width: f32,
    height: f32,
    treble: f32,
    fade: f32,
    flow: &Flow,
) {
    let Flow {
        phase,
        direction,
        palette,
        t_s,
    } = *flow;
    if treble <= 0.05 || fade <= 0.0 {
        return;
    }
    // Number of sparkles scales with treble, capped by capacity.
    let count = ((treble * MAX_SPARKLES as f32) as usize).min(MAX_SPARKLES);
    if count == 0 {
        return;
    }
    let thickness = (24.0 * visual::THICKNESS_SCALE).max(1.0);
    let dir = direction.sign();

    for i in 0..count {
        // Even spacing around the perimeter, plus a small fixed jitter from the
        // index so the row does not look like a dotted ruler. No per-frame
        // randomness.
        let even = i as f32 / count as f32;
        let jitter = ((i as f32 * 0.618_034).fract() - 0.5) * (0.5 / count as f32);
        // Advance together with the glow phase, in the chosen direction.
        let pos = (even + jitter + dir * phase).rem_euclid(1.0);

        // Depth varies per sparkle (fixed, biased toward the edge): some sit
        // right on the rim, some a little inward.
        let depth = (i as f32 * 0.381_966).fract().powi(2);
        // Each sparkle flickers on its own schedule, spread across the life.
        let life_phase = (t_s / SPARKLE_LIFE_S + i as f32 / count as f32).fract();
        let life = (1.0 - (2.0 * life_phase - 1.0).abs()) * treble * visual::SPARKLE_GAIN;

        let (x, y) = perimeter_point(pos, depth, thickness, width, height);
        let color = palette.at(pos);
        // Radius also fixed per sparkle.
        let radius = 1.5 + (i as f32 * 0.754_877).fract() * 2.0;
        frame.sparkles.push(Sparkle {
            x,
            y,
            radius,
            opacity: (life * fade).clamp(0.0, 1.0),
            color,
        });
    }
}

/// Convert `(pos along perimeter in turns, depth toward the edge)` to pixels.
///
/// The perimeter is top → right → bottom → left, matching `compose_edge`.
fn perimeter_point(pos: f32, depth: f32, thickness: f32, width: f32, height: f32) -> (f32, f32) {
    let d = depth * thickness;
    let w = width;
    let h = height;
    let p = pos.rem_euclid(1.0);
    let top = w / (2.0 * (w + h));
    let right = top + h / (2.0 * (w + h));
    let bottom = right + w / (2.0 * (w + h));
    if p < top {
        ((p / top) * w, d)
    } else if p < right {
        let f = (p - top) / (right - top);
        (w - d, f * h)
    } else if p < bottom {
        let f = (p - right) / (bottom - right);
        (w - f * w, h - d)
    } else {
        let f = (p - bottom) / (1.0 - bottom).max(1e-6);
        (d, h - f * h)
    }
}

/// Clamp levels into `[0,1]`, mapping non-finite to 0.
fn sanitize(levels: [f32; 3]) -> [f32; 3] {
    let f = |v: f32| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    [f(levels[0]), f(levels[1]), f(levels[2])]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ComposeParams {
        ComposeParams {
            thickness_px: 24.0,
            intensity: 1.0,
            palette: Palette::default_look(),
            direction: Direction::Clockwise,
        }
    }

    fn params_dir(direction: Direction) -> ComposeParams {
        ComposeParams {
            direction,
            ..params()
        }
    }

    fn input(levels: [f32; 3], t_s: f32, fade: f32, dt: f32) -> FrameInput {
        FrameInput {
            levels,
            t_s,
            dt,
            fade,
        }
    }

    fn rect() -> (f32, f32) {
        (1920.0, 1168.0)
    }

    /// Advance a frame by `dt` seconds at fixed levels (phase accumulates).
    fn step(frame: &mut GlowFrame, p: &ComposeParams, levels: [f32; 3], dt: f32) {
        compose(frame, 1920.0, 1168.0, p, &input(levels, 1.0, 1.0, dt));
    }

    #[test]
    fn segments_cover_every_edge_without_gaps() {
        let mut frame = GlowFrame::new();
        let (w, h) = rect();
        compose(
            &mut frame,
            w,
            h,
            &params(),
            &input([0.5, 0.0, 0.0], 1.0, 1.0, 1.0 / 60.0),
        );

        for (edge, expected) in [
            (Edge::Top, w),
            (Edge::Bottom, w),
            (Edge::Left, h),
            (Edge::Right, h),
        ] {
            let total: f32 = frame
                .segments
                .iter()
                .filter(|s| s.edge == edge)
                .map(|s| s.end - s.start)
                .sum();
            assert!(
                (total - expected).abs() < 0.5,
                "{edge:?}: {total} vs {expected}"
            );
        }
    }

    #[test]
    fn segments_are_short_and_ordered() {
        let mut frame = GlowFrame::new();
        let (w, h) = rect();
        compose(
            &mut frame,
            w,
            h,
            &params(),
            &input([0.5, 0.5, 0.5], 1.0, 1.0, 1.0 / 60.0),
        );
        for s in &frame.segments {
            assert!(s.end > s.start, "segment not forward: {s:?}");
            assert!(
                s.end - s.start <= SEGMENT_PX + 0.01,
                "segment too long: {s:?}"
            );
        }
    }

    #[test]
    fn invariants_hold_for_sweep_of_inputs() {
        let mut frame = GlowFrame::new();
        let (w, h) = rect();
        for direction in [Direction::Clockwise, Direction::CounterClockwise] {
            for &bass in &[0.0, 0.5, 1.0] {
                for &mid in &[0.0, 0.5, 1.0] {
                    for &treble in &[0.0, 0.5, 1.0] {
                        compose(
                            &mut frame,
                            w,
                            h,
                            &params_dir(direction),
                            &input([bass, mid, treble], 3.2, 1.0, 1.0 / 60.0),
                        );
                        for s in &frame.segments {
                            assert!(s.thickness >= 0.0, "negative thickness");
                            assert!((0.0..=1.0).contains(&s.opacity));
                            for c in s.color {
                                assert!((0.0..=1.0).contains(&c) && c.is_finite(), "color {c}");
                            }
                        }
                        for sp in &frame.sparkles {
                            assert!((0.0..=w).contains(&sp.x), "sparkle x {}", sp.x);
                            assert!((0.0..=h).contains(&sp.y), "sparkle y {}", sp.y);
                            assert!((0.0..=1.0).contains(&sp.opacity));
                        }
                        assert!(frame.sparkles.len() <= MAX_SPARKLES);
                    }
                }
            }
        }
    }

    #[test]
    fn zero_fade_yields_nothing() {
        let mut frame = GlowFrame::new();
        compose(
            &mut frame,
            1920.0,
            1168.0,
            &params(),
            &input([1.0, 1.0, 1.0], 1.0, 0.0, 1.0 / 60.0),
        );
        assert!(frame.segments.is_empty());
        assert!(frame.sparkles.is_empty());
    }

    #[test]
    fn levels_are_clamped_so_full_scale_stays_in_bounds() {
        let mut frame = GlowFrame::new();
        let (w, h) = rect();
        compose(
            &mut frame,
            w,
            h,
            &params(),
            &input([5.0, 5.0, 5.0], 1.0, 1.0, 1.0 / 60.0),
        );
        // Thickness = base × bass gain (≤1.4) × mid multiplier (≤1 + wave amp).
        let max_t = 24.0 * visual::THICKNESS_SCALE * 1.4 * (1.0 + visual::WAVE_AMPLITUDE) + 0.01;
        for s in &frame.segments {
            assert!(
                s.thickness <= max_t,
                "thickness {} exceeds cap {}",
                s.thickness,
                max_t
            );
        }
    }

    #[test]
    fn nan_levels_do_not_produce_nan() {
        let mut frame = GlowFrame::new();
        let (w, h) = rect();
        compose(
            &mut frame,
            w,
            h,
            &params(),
            &input([f32::NAN, f32::INFINITY, -1.0], 1.0, 1.0, 1.0 / 60.0),
        );
        for s in &frame.segments {
            assert!(s.thickness.is_finite() && s.opacity.is_finite());
            assert!(s.color.iter().all(|c| c.is_finite()));
        }
        for sp in &frame.sparkles {
            assert!(sp.x.is_finite() && sp.y.is_finite() && sp.opacity.is_finite());
        }
    }

    #[test]
    fn mid_level_modulates_thickness_along_the_edge() {
        let (w, h) = rect();
        let mut quiet = GlowFrame::new();
        let mut loud = GlowFrame::new();
        compose(
            &mut quiet,
            w,
            h,
            &params(),
            &input([0.5, 0.0, 0.0], 1.0, 1.0, 1.0 / 60.0),
        );
        compose(
            &mut loud,
            w,
            h,
            &params(),
            &input([0.5, 1.0, 0.0], 1.0, 1.0, 1.0 / 60.0),
        );

        let spread = |f: &GlowFrame| {
            let ts: Vec<f32> = f.segments.iter().map(|s| s.thickness).collect();
            let min = ts.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = ts.iter().cloned().fold(0.0, f32::max);
            max - min
        };
        assert!(
            spread(&loud) > spread(&quiet) + 1.0,
            "mid must add thickness variation"
        );
    }

    #[test]
    fn mid_pattern_shifts_over_time() {
        let (w, h) = rect();
        let p = params();
        let mut a = GlowFrame::new();
        compose(&mut a, w, h, &p, &input([0.5, 1.0, 0.0], 0.0, 1.0, 0.05));
        let mut b = GlowFrame::new();
        compose(&mut b, w, h, &p, &input([0.5, 1.0, 0.0], 0.0, 1.0, 0.05));
        compose(&mut b, w, h, &p, &input([0.5, 1.0, 0.0], 0.05, 1.0, 0.05));
        let diff: f32 = a
            .segments
            .iter()
            .zip(&b.segments)
            .map(|(x, y)| (x.thickness - y.thickness).abs())
            .sum();
        assert!(diff > 1.0, "mid wave must move when the phase advances");
    }

    #[test]
    fn treble_makes_sparkles_that_are_bounded() {
        let (w, h) = rect();
        let mut silent = GlowFrame::new();
        let mut sparkly = GlowFrame::new();
        compose(
            &mut silent,
            w,
            h,
            &params(),
            &input([0.5, 0.5, 0.0], 1.0, 1.0, 1.0 / 60.0),
        );
        compose(
            &mut sparkly,
            w,
            h,
            &params(),
            &input([0.5, 0.5, 1.0], 1.0, 1.0, 1.0 / 60.0),
        );
        assert_eq!(silent.sparkles.len(), 0);
        assert!(!sparkly.sparkles.is_empty());
        assert!(sparkly.sparkles.len() <= MAX_SPARKLES);
        for sp in &sparkly.sparkles {
            assert!((0.0..=w).contains(&sp.x));
            assert!((0.0..=h).contains(&sp.y));
        }
    }

    #[test]
    fn sparkles_do_not_grow_the_buffer_after_warmup() {
        let mut frame = GlowFrame::new();
        let (w, h) = rect();
        compose(
            &mut frame,
            w,
            h,
            &params(),
            &input([1.0, 1.0, 1.0], 0.0, 1.0, 1.0 / 60.0),
        );
        let cap_segments = frame.segments.capacity();
        let cap_sparkles = frame.sparkles.capacity();
        for i in 0..50 {
            compose(
                &mut frame,
                w,
                h,
                &params(),
                &input([1.0, 1.0, 1.0], i as f32, 1.0, 1.0 / 60.0),
            );
        }
        assert_eq!(
            frame.segments.capacity(),
            cap_segments,
            "segments reallocated"
        );
        assert_eq!(
            frame.sparkles.capacity(),
            cap_sparkles,
            "sparkles reallocated"
        );
    }

    #[test]
    fn phase_advances_and_wraps() {
        let mut frame = GlowFrame::new();
        let p = params();
        assert_eq!(frame.phase, 0.0);
        step(&mut frame, &p, [0.0, 0.0, 0.0], 0.05);
        let after_one = frame.phase;
        assert!(after_one > 0.0, "phase must advance at rest");
        assert!(after_one < 1.0);
        for _ in 0..200 {
            step(&mut frame, &p, [0.0, 0.0, 0.0], 0.05);
        }
        assert!((0.0..1.0).contains(&frame.phase), "phase must wrap");
    }

    #[test]
    fn music_speeds_up_the_travel() {
        let p = params();
        let mut quiet = GlowFrame::new();
        let mut loud = GlowFrame::new();
        step(&mut quiet, &p, [0.0, 0.0, 0.0], 0.1);
        step(&mut loud, &p, [1.0, 1.0, 0.0], 0.1);
        assert!(
            loud.phase > quiet.phase,
            "louder audio must rotate faster ({:?} vs {:?})",
            quiet.phase,
            loud.phase
        );
    }

    #[test]
    fn direction_reverses_travel() {
        let (w, h) = rect();
        // A two-colour palette makes the travel direction visible in the hue.
        let cw_params = ComposeParams {
            palette: Palette::new(&[[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
            direction: Direction::Clockwise,
            ..params()
        };
        let ccw_params = ComposeParams {
            direction: Direction::CounterClockwise,
            ..cw_params
        };
        let mut cw = GlowFrame::new();
        let mut ccw = GlowFrame::new();
        compose(
            &mut cw,
            w,
            h,
            &cw_params,
            &input([0.5, 0.5, 1.0], 0.0, 1.0, 0.1),
        );
        compose(
            &mut ccw,
            w,
            h,
            &ccw_params,
            &input([0.5, 0.5, 1.0], 0.0, 1.0, 0.1),
        );
        assert!(cw.phase > 0.0);
        assert!(ccw.phase > 0.0); // rem_euclid keeps it in [0,1)
        assert!(
            (cw.phase - ccw.phase).abs() > 1e-4,
            "directions must differ"
        );
        let color_at = |f: &GlowFrame, edge: Edge, start: f32| {
            f.segments
                .iter()
                .find(|s| s.edge == edge && (s.start - start).abs() < 0.5)
                .map(|s| s.color)
        };
        let cw_c = color_at(&cw, Edge::Top, 96.0).unwrap();
        let ccw_c = color_at(&ccw, Edge::Top, 96.0).unwrap();
        let diff: f32 = (0..3).map(|i| (cw_c[i] - ccw_c[i]).abs()).sum();
        assert!(
            diff > 1e-3,
            "colour must travel the other way: {cw_c:?} vs {ccw_c:?}"
        );
    }

    #[test]
    fn palette_colours_the_segments() {
        // A two-colour palette must show up in the segment colours, and the
        // frame must contain more than one distinct colour.
        let p = ComposeParams {
            palette: Palette::new(&[[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
            ..params()
        };
        let mut frame = GlowFrame::new();
        compose(
            &mut frame,
            1920.0,
            1168.0,
            &p,
            &input([0.5, 0.0, 0.0], 0.0, 1.0, 1.0 / 60.0),
        );
        let distinct = frame.segments.iter().filter(|s| s.color[0] > 0.5).count();
        let blue = frame.segments.iter().filter(|s| s.color[2] > 0.5).count();
        assert!(distinct > 0 && blue > 0, "both palette colours must appear");
    }

    #[test]
    fn wave_shape_is_bounded_and_plain_sine_when_smooth() {
        for i in 0..200 {
            let theta = i as f32 * 0.1;
            assert!((wave_shape(theta, 0.0) - theta.sin()).abs() < 1e-5);
            for r in [0.25, 0.5, 1.0] {
                let w = wave_shape(theta, r);
                assert!((-1.0..=1.0).contains(&w), "theta={theta} r={r} w={w}");
            }
        }
    }

    #[test]
    fn non_finite_dt_does_not_move_phase() {
        let mut frame = GlowFrame::new();
        compose(
            &mut frame,
            1920.0,
            1168.0,
            &params(),
            &input([0.5, 0.5, 0.5], 0.0, 1.0, f32::NAN),
        );
        assert_eq!(frame.phase, 0.0);
        assert!(frame.phase.is_finite());
    }
}
