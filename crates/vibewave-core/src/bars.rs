//! Spectrum-bar composition: turn a per-bar audio spectrum into bar geometry.
//!
//! This is the second visual language beside the neon [`crate::compose`] glow: a
//! frame of thin bars growing inward from the screen edges, each bar driven by its
//! own frequency bin, the screen's middle left transparent. It mirrors the browser
//! prototype in `spikes/visual-proto/index.html`.
//!
//! Pure maths, like `compose`: given the overlay size, a per-bar spectrum (one
//! value in `[0, 1]` per slot), and the [`BarsParams`], it produces a flat list of
//! [`BarSegment`]s. The renderer only paints them.
//!
//! The silhouette is **static**: bar `i` has a fixed relative length from a seeded
//! hash of `(i, seed)`, so given the same spectrum the frame draws identically.
//! Bars move only because the spectrum changes — there is no time term, so this
//! module needs no clock.

use crate::compose::Edge;
use crate::palette::Palette;

/// Maximum number of bars per edge slot table (enough for a 4K long edge).
pub const SPECTRUM_BINS: usize = 96;

/// Which screen edges the bars are drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// All four edges.
    #[default]
    All,
    /// Top and bottom edges only.
    Horizontal,
    /// Left and right edges only.
    Vertical,
}

impl Position {
    pub fn id(self) -> &'static str {
        match self {
            Position::All => "all",
            Position::Horizontal => "horizontal",
            Position::Vertical => "vertical",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "frame" | "full" => Some(Position::All),
            "horizontal" | "top-bottom" | "top_bottom" | "h" | "atas-bawah" => {
                Some(Position::Horizontal)
            }
            "vertical" | "left-right" | "left_right" | "v" | "kiri-kanan" => {
                Some(Position::Vertical)
            }
            _ => None,
        }
    }

    fn edges(self) -> &'static [Edge] {
        match self {
            Position::All => &[Edge::Top, Edge::Bottom, Edge::Left, Edge::Right],
            Position::Horizontal => &[Edge::Top, Edge::Bottom],
            Position::Vertical => &[Edge::Left, Edge::Right],
        }
    }
}

/// One bar to paint: a rectangle of constant thickness spanning `outer`..`inner`
/// along the inward axis from `edge`.
///
/// `pos` is the position along the edge in pixels (0 = left for top/bottom, 0 =
/// top for left/right); `outer`/`inner` are signed offsets from the near edge
/// along the inward axis (inward-positive), so `outer` may be negative for an
/// outward overhang; `thickness` is the width across the edge; `opacity` is its
/// alpha; `color` is its linear-sRGB value (0–1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarSegment {
    pub edge: Edge,
    pub pos: f32,
    pub outer: f32,
    pub inner: f32,
    pub thickness: f32,
    pub opacity: f32,
    pub color: [f32; 3],
}

impl BarSegment {
    /// The signed span along the inward axis (`inner - outer`).
    pub fn length(&self) -> f32 {
        self.inner - self.outer
    }
}

/// Tunable bar-composition parameters (the Bars-theme subset of `Settings`).
#[derive(Debug, Clone, Copy)]
pub struct BarsParams {
    /// Bar thickness across the edge, in physical pixels (already DPI-scaled).
    pub thickness_px: f32,
    /// Peak bar length as a fraction of the screen's shortest side.
    pub max_len_frac: f32,
    /// Baseline offset from the edge as a fraction of the shortest side; a bar is
    /// drawn from this offset inward. Negative overhangs the bar outward past the
    /// edge.
    pub floor_frac: f32,
    /// Center bias: 0 = every bar reacts equally, higher = the middle of each
    /// edge reacts more than its ends.
    pub center_profile: f32,
    /// How strongly bar height follows the spectrum (a multiplier on the level).
    pub beat_response: f32,
    /// Global opacity multiplier (the Intensity control).
    pub opacity: f32,
    /// Colour source; bars are painted with `palette.at(0)`.
    pub palette: Palette,
    /// Which edges to draw.
    pub position: Position,
}

/// The composed bar frame. Reused across frames so steady-state drawing allocates
/// nothing: [`compose_bars`] clears and refills the existing `Vec`.
#[derive(Debug, Default)]
pub struct BarsFrame {
    pub bars: Vec<BarSegment>,
}

impl BarsFrame {
    pub fn new() -> Self {
        Self { bars: Vec::new() }
    }
}

/// Static per-bar silhouette shape in `(0, 1]`, a pure function of `(i, seed)`.
///
/// Two integer hashes are combined: a moderate base plus an occasional tall spike,
/// so an edge reads like a plausible spectrum (many mid bars, a few tall ones)
/// rather than a flat block. No time term, so the same input always draws the same.
pub fn bar_shape(i: usize, seed: u32) -> f32 {
    let mix = |mut x: u32| {
        x ^= x >> 13;
        x = x.wrapping_mul(0x4d3c_2b1d);
        x ^= x >> 16;
        x
    };
    let r1 = mix((i as u32)
        .wrapping_mul(374_761_393)
        .wrapping_add(seed.wrapping_mul(668_265_263))) as f32
        / u32::MAX as f32;
    let r2 = mix((i as u32)
        .wrapping_mul(1_013_904_223)
        .wrapping_add(seed.wrapping_mul(2_654_435_761))) as f32
        / u32::MAX as f32;
    let base = 0.18 + 0.42 * r1;
    let spike = 0.55 * r2 * r2 * r1;
    (base + spike).clamp(0.04, 1.0)
}

/// Center weighting along an edge: 1 at the middle (`t = 0.5`), falling to
/// `1 - profile` at the ends. `t` is the normalised position along the edge.
pub fn center_weight(t: f32, profile: f32) -> f32 {
    let d = 2.0 * (t - 0.5).abs();
    1.0 - profile * d * d
}

/// Seed of the silhouette for one edge, so no two edges are identical.
fn seed_of(edge: Edge) -> u32 {
    match edge {
        Edge::Top => 11,
        Edge::Bottom => 37,
        Edge::Left => 53,
        Edge::Right => 71,
    }
}

/// Compose one bar frame into `frame`.
///
/// `width`/`height` are the overlay size in physical pixels; `spectrum` is the
/// per-bar spectrum (one value in `[0, 1]` per slot, from the audio thread);
/// `params` are the slow-changing settings. The frame's `Vec` is cleared and
/// refilled, so after a warm-up no allocation happens.
pub fn compose_bars(
    frame: &mut BarsFrame,
    width: f32,
    height: f32,
    spectrum: &[f32],
    params: &BarsParams,
) {
    frame.bars.clear();
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let thickness = if params.thickness_px.is_finite() {
        params.thickness_px.max(1.0)
    } else {
        1.0
    };
    let max_len = frac(params.max_len_frac, 0.28).max(0.0) * width.min(height);
    let floor = frac(params.floor_frac, 0.0) * width.min(height);
    let profile = frac(params.center_profile, 0.0).clamp(0.0, 1.0);
    let response = if params.beat_response.is_finite() {
        params.beat_response.max(0.0)
    } else {
        1.0
    };
    let opacity = if params.opacity.is_finite() {
        params.opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let color = params.palette.at(0.0);
    let n_spec = spectrum.len().max(1);
    // Bar occupies half its slot; the other half is the gap.
    let spacing_factor = 2.0;

    for &edge in params.position.edges() {
        let len = match edge {
            Edge::Top | Edge::Bottom => width,
            Edge::Left | Edge::Right => height,
        };
        if len <= 0.0 {
            continue;
        }
        let seed = seed_of(edge);
        let count = ((len / (thickness * spacing_factor)).floor() as usize).max(1);
        let slot = len / count as f32;
        let bar_w = thickness.min(slot * 0.75);
        let inset = (slot - bar_w) * 0.5;
        // Height a bar can grow before it would reach the opposite edge.
        let cap = match edge {
            Edge::Top | Edge::Bottom => height,
            Edge::Left | Edge::Right => width,
        };

        for i in 0..count {
            let t_pos = if count > 1 {
                i as f32 / (count - 1) as f32
            } else {
                0.5
            };
            let si = ((t_pos * (n_spec - 1) as f32).round() as usize).min(n_spec - 1);
            let raw = spectrum.get(si).copied().unwrap_or(0.0);
            let level = if raw.is_finite() {
                (raw * response).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let level = (level * center_weight(t_pos, profile)).clamp(0.0, 1.0);
            let grow = (level * max_len * bar_shape(i, seed)).min(max_len);
            // Inner tip (capped at the far edge); outer end (negative = overhang).
            let inner = (floor + grow).clamp(0.0, cap);
            let outer = floor.min(0.0);
            if inner - outer <= 0.0 {
                continue;
            }
            frame.bars.push(BarSegment {
                edge,
                pos: i as f32 * slot + inset,
                outer,
                inner,
                thickness: bar_w,
                opacity,
                color,
            });
        }
    }
}

/// A finite fraction parameter, falling back to `default` on bad input.
fn frac(v: f32, default: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> BarsParams {
        BarsParams {
            thickness_px: 3.0,
            max_len_frac: 0.28,
            floor_frac: 0.12,
            center_profile: 0.6,
            beat_response: 1.0,
            opacity: 1.0,
            palette: Palette::single([1.0, 1.0, 1.0]),
            position: Position::All,
        }
    }

    fn spectrum_of(bins: usize, v: f32) -> Vec<f32> {
        vec![v; bins]
    }

    fn rect_contains(seg: &BarSegment, w: f32, h: f32) -> bool {
        // The span always stays inside the overlay: the inner tip is clamped to
        // the far edge and the outer end never goes past the near edge (an
        // overhang is negative, outside the overlay, so only check inside).
        let (x0, y0, x1, y1) = match seg.edge {
            Edge::Top => (seg.pos, seg.outer, seg.pos + seg.thickness, seg.inner),
            Edge::Bottom => (
                seg.pos,
                h - seg.inner,
                seg.pos + seg.thickness,
                h - seg.outer,
            ),
            Edge::Left => (seg.outer, seg.pos, seg.inner, seg.pos + seg.thickness),
            Edge::Right => (
                w - seg.inner,
                seg.pos,
                w - seg.outer,
                seg.pos + seg.thickness,
            ),
        };
        let inside = |v: f32, hi: f32| v >= -0.01 && v <= hi + 0.01;
        inside(x0, w) && inside(x1, w) && inside(y0, h) && inside(y1, h)
    }

    #[test]
    fn silent_spectrum_collapses_bars_toward_the_floor() {
        let mut frame = BarsFrame::new();
        compose_bars(&mut frame, 1920.0, 1080.0, &spectrum_of(96, 0.0), &params());
        // At silence only the floor stub remains; the inner tip sits at the floor.
        let floor = 0.12 * 1080.0_f32.min(1920.0);
        for seg in &frame.bars {
            assert!(
                (seg.length() - floor).abs() < 0.01,
                "length {} should equal the floor {floor}",
                seg.length()
            );
        }
    }

    #[test]
    fn full_spectrum_stays_in_bounds_and_within_cap() {
        let mut frame = BarsFrame::new();
        let (w, h) = (1920.0, 1080.0);
        compose_bars(&mut frame, w, h, &spectrum_of(96, 1.0), &params());
        let max_len = 0.28 * w.min(h);
        for seg in &frame.bars {
            assert!(rect_contains(seg, w, h), "out of bounds: {seg:?}");
            // A bar's inner reach (length from the near edge) never exceeds floor+cap.
            assert!(
                seg.length() <= 0.12 * w.min(h) + max_len + 0.01,
                "bar too long: {seg:?}"
            );
        }
    }

    #[test]
    fn bar_count_is_sane_for_tiny_and_huge_overlays() {
        for (w, h) in [(64.0, 48.0), (4096.0, 2160.0)] {
            let mut frame = BarsFrame::new();
            compose_bars(&mut frame, w, h, &spectrum_of(96, 0.7), &params());
            assert!(!frame.bars.is_empty(), "{w}x{h} produced no bars");
            assert!(frame.bars.len() <= 8000, "{w}x{h} produced too many bars");
        }
    }

    #[test]
    fn no_time_dependence_is_possible_by_construction() {
        // There is no time parameter; two identical calls are identical.
        let mut a = BarsFrame::new();
        let mut b = BarsFrame::new();
        compose_bars(&mut a, 800.0, 600.0, &spectrum_of(96, 0.6), &params());
        compose_bars(&mut b, 800.0, 600.0, &spectrum_of(96, 0.6), &params());
        assert_eq!(a.bars, b.bars);
    }

    #[test]
    fn center_weight_peaks_at_the_middle() {
        assert!((center_weight(0.5, 0.6) - 1.0).abs() < 1e-6);
        assert!(center_weight(0.5, 0.6) > center_weight(0.0, 0.6));
        assert!(center_weight(0.0, 0.6) < center_weight(0.2, 0.6));
        // No profile → flat.
        assert_eq!(center_weight(0.0, 0.0), center_weight(0.5, 0.0));
    }

    #[test]
    fn center_bias_makes_the_middle_bars_taller() {
        let p = BarsParams {
            center_profile: 0.6,
            ..params()
        };
        let mut frame = BarsFrame::new();
        compose_bars(&mut frame, 1920.0, 1080.0, &spectrum_of(96, 1.0), &p);
        let top: Vec<f32> = frame
            .bars
            .iter()
            .filter(|s| s.edge == Edge::Top)
            .map(|s| s.length())
            .collect();
        let mid = top[top.len() / 2];
        let end = top[0];
        assert!(mid > end, "middle {mid} should exceed the end {end}");
    }

    #[test]
    fn negative_floor_overhangs_outward_by_the_floor_amount() {
        // A negative floor keeps the same growth (bar height) but moves the whole
        // bar outward, so at silence only an outward stub of `|floor|` remains
        // instead of nothing.
        let base = BarsParams {
            floor_frac: 0.0,
            ..params()
        };
        let out = BarsParams {
            floor_frac: -0.10,
            ..base
        };
        let overhang_px = 0.10 * 1080.0;

        let mut silent_inside = BarsFrame::new();
        let mut silent_over = BarsFrame::new();
        compose_bars(
            &mut silent_inside,
            1920.0,
            1080.0,
            &spectrum_of(96, 0.0),
            &base,
        );
        compose_bars(
            &mut silent_over,
            1920.0,
            1080.0,
            &spectrum_of(96, 0.0),
            &out,
        );
        assert!(
            silent_inside.bars.is_empty(),
            "no floor → nothing at silence"
        );
        assert!(!silent_over.bars.is_empty(), "overhang → an outward stub");
        for seg in &silent_over.bars {
            assert!((seg.length() - overhang_px).abs() < 0.05, "{seg:?}");
        }

        // With signal, the growth (span minus the overhang) is unchanged.
        let mut loud_inside = BarsFrame::new();
        let mut loud_over = BarsFrame::new();
        compose_bars(
            &mut loud_inside,
            1920.0,
            1080.0,
            &spectrum_of(96, 1.0),
            &base,
        );
        compose_bars(&mut loud_over, 1920.0, 1080.0, &spectrum_of(96, 1.0), &out);
        assert_eq!(loud_inside.bars.len(), loud_over.bars.len());
        // `length` is the growth, so it is identical whether the floor is inward
        // or outward: the overhang only shifts *where* the bar starts, which is
        // represented by the renderer geometry, not by `length`.
        assert_eq!(loud_inside.bars.len(), loud_over.bars.len());
        let mut compared = 0;
        for (a, b) in loud_inside.bars.iter().zip(&loud_over.bars) {
            // A bar whose tip is pinned at the edge by the outward floor
            // (floor + grow <= 0) renders as a flat outward stub of `|floor|`;
            // skip those, they are a deliberate clamp, not a growth change.
            if b.length() <= overhang_px + 0.05 {
                continue;
            }
            compared += 1;
            assert!(
                (a.length() - b.length()).abs() < 0.05,
                "growth changed: {} vs {}",
                a.length(),
                b.length()
            );
        }
        assert!(compared > 0, "no unclamped bars to compare");
    }

    #[test]
    fn position_selects_only_its_edges() {
        let cases = [
            (
                Position::All,
                vec![Edge::Top, Edge::Bottom, Edge::Left, Edge::Right],
            ),
            (Position::Horizontal, vec![Edge::Top, Edge::Bottom]),
            (Position::Vertical, vec![Edge::Left, Edge::Right]),
        ];
        for (position, want) in cases {
            let p = BarsParams {
                position,
                ..params()
            };
            let mut frame = BarsFrame::new();
            compose_bars(&mut frame, 1920.0, 1080.0, &spectrum_of(96, 1.0), &p);
            let present: Vec<Edge> = want
                .iter()
                .copied()
                .filter(|e| frame.bars.iter().any(|s| s.edge == *e))
                .collect();
            assert_eq!(present, want, "{position:?}");
            let unexpected: Vec<Edge> = frame
                .bars
                .iter()
                .map(|s| s.edge)
                .filter(|e| !want.contains(e))
                .collect();
            assert!(unexpected.is_empty(), "{position:?} drew {unexpected:?}");
        }
    }

    #[test]
    fn beat_response_scales_bar_height() {
        let mut low = BarsFrame::new();
        let mut high = BarsFrame::new();
        compose_bars(
            &mut low,
            1920.0,
            1080.0,
            &spectrum_of(96, 0.5),
            &BarsParams {
                beat_response: 0.5,
                ..params()
            },
        );
        compose_bars(
            &mut high,
            1920.0,
            1080.0,
            &spectrum_of(96, 0.5),
            &BarsParams {
                beat_response: 2.0,
                ..params()
            },
        );
        let total = |f: &BarsFrame| f.bars.iter().map(|s| s.length()).sum::<f32>();
        assert!(total(&high) > total(&low));
    }

    #[test]
    fn nan_spectrum_and_params_do_not_produce_nan() {
        let mut frame = BarsFrame::new();
        let mut spec = spectrum_of(96, 0.5);
        spec[0] = f32::NAN;
        let p = BarsParams {
            thickness_px: f32::NAN,
            max_len_frac: f32::NAN,
            floor_frac: f32::NAN,
            center_profile: f32::NAN,
            beat_response: f32::INFINITY,
            opacity: f32::NAN,
            ..params()
        };
        compose_bars(&mut frame, 1920.0, 1080.0, &spec, &p);
        for seg in &frame.bars {
            assert!(seg.outer.is_finite() && seg.inner.is_finite() && seg.opacity.is_finite());
            assert!(seg.color.iter().all(|c| c.is_finite()));
        }
    }

    #[test]
    fn zero_size_yields_nothing() {
        let mut frame = BarsFrame::new();
        compose_bars(&mut frame, 0.0, 1080.0, &spectrum_of(96, 1.0), &params());
        assert!(frame.bars.is_empty());
    }

    #[test]
    fn buffer_does_not_grow_after_warmup() {
        let mut frame = BarsFrame::new();
        compose_bars(&mut frame, 1920.0, 1080.0, &spectrum_of(96, 1.0), &params());
        let cap = frame.bars.capacity();
        for v in [0.0, 0.5, 1.0, 0.25] {
            compose_bars(&mut frame, 1920.0, 1080.0, &spectrum_of(96, v), &params());
        }
        assert_eq!(frame.bars.capacity(), cap, "buffer reallocated");
    }

    #[test]
    fn bar_shape_is_deterministic_and_static() {
        for i in 0..200 {
            let a = bar_shape(i, 11);
            let b = bar_shape(i, 11);
            assert_eq!(a, b);
            assert!((0.04..=1.0).contains(&a));
        }
        // Different edges get different silhouettes.
        assert_ne!(bar_shape(5, 11), bar_shape(5, 37));
    }

    #[test]
    fn position_id_round_trips() {
        for p in [Position::All, Position::Horizontal, Position::Vertical] {
            assert_eq!(Position::from_id(p.id()), Some(p));
        }
        assert_eq!(Position::from_id("frame"), Some(Position::All));
        assert_eq!(Position::from_id("kiri-kanan"), Some(Position::Vertical));
        assert_eq!(Position::from_id("diagonal"), None);
    }
}
