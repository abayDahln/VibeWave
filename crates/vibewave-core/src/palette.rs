//! Colour maths for the glow.
//!
//! `hsv_to_rgb` is the low-level conversion; [`Palette`] is the ordered set of
//! colours the glow is painted with (from cover art, a custom colour, or the
//! default look). The renderer turns a `Palette` into a fixed set of gradient
//! brushes (research R-7); the decision of *which* colour each segment uses
//! lives in `compose`.

/// Convert HSV to linear sRGB. `h` is degrees (wrapped), `s` and `v` are 0–1.
/// Returns components in 0–1.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let sector = h / 60.0;
    let x = c * (1.0 - ((sector % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match sector as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r1 + m, g1 + m, b1 + m]
}

/// Maximum colours a palette holds.
pub const MAX_PALETTE: usize = 4;

/// An ordered set of 1–4 colours (linear sRGB, 0–1) the glow cycles through
/// around the perimeter.
///
/// With one colour the frame is a single hue. With more, the colours are
/// spread evenly around the perimeter and interpolated **circularly**, so the
/// last colour blends back into the first — no seam where the perimeter joins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    colors: [[f32; 3]; MAX_PALETTE],
    len: usize,
}

impl Palette {
    /// A palette from one to four colours. Extra colours are ignored; an empty
    /// slice yields the default magenta.
    pub fn new(colors: &[[f32; 3]]) -> Self {
        let mut out = [[0.0; 3]; MAX_PALETTE];
        let len = colors.len().clamp(1, MAX_PALETTE);
        for (dst, src) in out.iter_mut().zip(colors.iter()) {
            *dst = sanitize_color(*src);
        }
        if colors.is_empty() {
            out[0] = hsv_to_rgb(crate::visual::BASE_HUE, 0.85, 1.0);
        }
        Self { colors: out, len }
    }

    /// A single-colour palette.
    pub fn single(color: [f32; 3]) -> Self {
        Self::new(&[color])
    }

    /// The default magenta/pink look, used when neither cover art nor a custom
    /// colour is available.
    pub fn default_look() -> Self {
        Self::single(hsv_to_rgb(crate::visual::BASE_HUE, 0.85, 1.0))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The colour at a position along the perimeter (`pos01`, wrapped to `0..1`).
    ///
    /// Interpolates between adjacent palette colours, and between the last and
    /// first across the wrap point, so the result is seamless.
    pub fn at(&self, pos01: f32) -> [f32; 3] {
        if self.len == 1 {
            return self.colors[0];
        }
        let pos = pos01.rem_euclid(1.0);
        let scaled = pos * self.len as f32;
        let i = scaled.floor() as usize % self.len;
        let j = (i + 1) % self.len;
        let t = scaled - scaled.floor();
        lerp3(self.colors[i], self.colors[j], t)
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::default_look()
    }
}

/// Linear interpolation between two colours.
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Clamp a colour into `[0,1]` and replace non-finite components with 0.
fn sanitize_color(c: [f32; 3]) -> [f32; 3] {
    let f = |v: f32| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    [f(c[0]), f(c[1]), f(c[2])]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "expected {b}, got {a}");
    }

    #[test]
    fn primaries() {
        let red = hsv_to_rgb(0.0, 1.0, 1.0);
        approx(red[0], 1.0);
        approx(red[1], 0.0);
        approx(red[2], 0.0);

        let green = hsv_to_rgb(120.0, 1.0, 1.0);
        approx(green[0], 0.0);
        approx(green[1], 1.0);
        approx(green[2], 0.0);

        let blue = hsv_to_rgb(240.0, 1.0, 1.0);
        approx(blue[0], 0.0);
        approx(blue[1], 0.0);
        approx(blue[2], 1.0);
    }

    #[test]
    fn wrapping_and_greyscale() {
        let a = hsv_to_rgb(360.0, 1.0, 1.0);
        let b = hsv_to_rgb(0.0, 1.0, 1.0);
        for i in 0..3 {
            approx(a[i], b[i]);
        }
        let grey = hsv_to_rgb(200.0, 0.0, 0.5);
        for c in grey {
            approx(c, 0.5);
        }
    }

    #[test]
    fn single_colour_palette_is_constant() {
        let p = Palette::single([0.2, 0.4, 0.6]);
        for pos in [0.0, 0.25, 0.5, 0.99] {
            assert_eq!(p.at(pos), [0.2, 0.4, 0.6]);
        }
    }

    #[test]
    fn two_colour_palette_interpolates_and_wraps() {
        let p = Palette::new(&[[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]]);
        approx(p.at(0.0)[0], 0.0);
        approx(p.at(0.5)[0], 1.0); // second colour sits at the half-way point
        approx(p.at(0.25)[0], 0.5); // mid-way between the two
                                    // Seamless across the wrap: position just below 1 meets position 0.
        let near_wrap = p.at(0.999)[0];
        let at_zero = p.at(0.0)[0];
        assert!(
            (near_wrap - at_zero).abs() < 0.01,
            "{near_wrap} vs {at_zero}"
        );
    }

    #[test]
    fn empty_palette_is_the_default_look() {
        let p = Palette::new(&[]);
        assert_eq!(p.len(), 1);
        assert_eq!(p.at(0.3), Palette::default_look().at(0.0));
    }

    #[test]
    fn colours_are_clamped_and_sanitized() {
        let p = Palette::single([f32::NAN, 5.0, -1.0]);
        assert_eq!(p.at(0.0), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn palette_holds_at_most_four_colours() {
        let p = Palette::new(&[
            [0.1, 0.1, 0.1],
            [0.2, 0.2, 0.2],
            [0.3, 0.3, 0.3],
            [0.4, 0.4, 0.4],
            [0.5, 0.5, 0.5],
        ]);
        assert_eq!(p.len(), MAX_PALETTE);
    }
}
