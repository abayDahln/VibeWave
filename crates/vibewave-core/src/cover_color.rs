//! Extract a small palette of dominant colours from cover-art pixels.
//!
//! The bytes come from Windows' thumbnail decode (see the `media` module in the
//! binary): a small square of BGRA pixels. This module is pure so the colour
//! maths is unit-tested without any image codec.
//!
//! Approach: bucket pixels by coarse hue, weight each bucket by how saturated
//! and how frequent its pixels are, then pick the strongest buckets and average
//! each. Grey/near-black/near-white pixels are skipped because they make for a
//! dull glow. If the image has no colourful pixels, we fall back to a single
//! averaged colour so the caller always gets something usable.

use crate::palette::{Palette, MAX_PALETTE};

/// Coarse hue buckets (0–360° in steps).
const HUE_BUCKETS: usize = 12;
/// Ignore pixels below this saturation: they are greys.
const MIN_SATURATION: f32 = 0.18;
/// Ignore pixels outside this value band: they are near-black or blown-out.
const MIN_VALUE: f32 = 0.15;
const MAX_VALUE: f32 = 0.98;

/// BGR(A) bytes per pixel as Windows supplies them.
pub const BYTES_PER_PIXEL: usize = 4;

/// Extract up to `max` dominant colours (linear sRGB 0–1) from BGRA pixels.
///
/// Returns a non-empty vector unless there are no pixels at all.
pub fn dominant_colors(bgra: &[u8], max: usize) -> Vec<[f32; 3]> {
    let max = max.clamp(1, MAX_PALETTE);
    let mut buckets: [Bucket; HUE_BUCKETS] = Default::default();
    let mut total = [0.0f32; 3];
    let mut count = 0u32;

    for px in bgra.chunks(BYTES_PER_PIXEL) {
        if px.len() < BYTES_PER_PIXEL {
            break;
        }
        let b = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let r = px[2] as f32 / 255.0;
        total[0] += r;
        total[1] += g;
        total[2] += b;
        count += 1;

        let (h, s, v) = rgb_to_hsv([r, g, b]);
        if s < MIN_SATURATION || !(MIN_VALUE..=MAX_VALUE).contains(&v) {
            continue;
        }
        let idx = ((h / 360.0) * HUE_BUCKETS as f32) as usize % HUE_BUCKETS;
        // Weight by saturation × value so vivid, bright colours win.
        let weight = s * v;
        buckets[idx].add([r, g, b], weight);
    }

    let mut coloured: Vec<(f32, [f32; 3])> = buckets
        .iter()
        .filter(|b| b.weight > 0.0)
        .map(|b| (b.weight, b.average()))
        .collect();
    coloured.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    coloured.truncate(max);
    let colors: Vec<[f32; 3]> = coloured.into_iter().map(|(_, c)| c).collect();

    if colors.is_empty() {
        // No vivid pixels: use the average so the glow still changes with the art.
        if count == 0 {
            return vec![];
        }
        let avg = [
            total[0] / count as f32,
            total[1] / count as f32,
            total[2] / count as f32,
        ];
        return vec![avg];
    }
    colors
}

/// Build a `Palette` from BGRA pixels, falling back to `crate::visual`'s default.
pub fn palette_from_bgra(bgra: &[u8]) -> Palette {
    let colors = dominant_colors(bgra, MAX_PALETTE);
    if colors.is_empty() {
        Palette::default_look()
    } else {
        Palette::new(&colors)
    }
}

#[derive(Default, Clone, Copy)]
struct Bucket {
    weight: f32,
    sum: [f32; 3],
}

impl Bucket {
    fn add(&mut self, color: [f32; 3], weight: f32) {
        self.weight += weight;
        for (slot, &c) in self.sum.iter_mut().zip(color.iter()) {
            *slot += c * weight;
        }
    }

    fn average(&self) -> [f32; 3] {
        if self.weight <= 0.0 {
            [0.0, 0.0, 0.0]
        } else {
            [
                self.sum[0] / self.weight,
                self.sum[1] / self.weight,
                self.sum[2] / self.weight,
            ]
        }
    }
}

/// RGB (0–1) → `(hue degrees, saturation, value)`.
fn rgb_to_hsv([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let s = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (h.rem_euclid(360.0), s, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build BGRA pixels from RGB triples (alpha ignored).
    fn bgra(pixels: &[[u8; 3]]) -> Vec<u8> {
        pixels
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect()
    }

    #[test]
    fn solid_strong_colour_is_detected() {
        let px = bgra(&[[10, 40, 220]; 100]);
        let colors = dominant_colors(&px, 4);
        assert_eq!(colors.len(), 1);
        // Roughly blue.
        assert!(colors[0][2] > colors[0][0], "{:?}", colors[0]);
    }

    #[test]
    fn grey_image_falls_back_to_average() {
        let px = bgra(&[[128, 128, 128]; 50]);
        let colors = dominant_colors(&px, 4);
        assert_eq!(colors.len(), 1);
        // The average grey (not empty).
        assert!((colors[0][0] - 0.5).abs() < 0.02, "{:?}", colors[0]);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(dominant_colors(&[], 4).is_empty());
    }

    #[test]
    fn two_distinct_hues_are_both_found() {
        // Half vivid red, half vivid green.
        let mut px = vec![[220u8, 20, 20]; 64];
        px.extend(vec![[20u8, 220, 20]; 64]);
        let colors = dominant_colors(&bgra(&px), 4);
        assert_eq!(colors.len(), 2, "{colors:?}");
    }

    #[test]
    fn never_returns_more_than_max() {
        let mut px = Vec::new();
        for i in 0..HUE_BUCKETS {
            let hue = i as f32 * 30.0;
            let rgb = crate::palette::hsv_to_rgb(hue, 1.0, 1.0);
            let bytes = [
                (rgb[0] * 255.0) as u8,
                (rgb[1] * 255.0) as u8,
                (rgb[2] * 255.0) as u8,
            ];
            px.extend(vec![bytes; 16]);
        }
        let colors = dominant_colors(&bgra(&px), 3);
        assert!(colors.len() <= 3);
    }

    #[test]
    fn palette_from_bgra_is_never_empty() {
        let p = palette_from_bgra(&bgra(&[[220, 20, 120]; 50]));
        assert!(!p.is_empty());
        let p = palette_from_bgra(&[]);
        assert!(!p.is_empty()); // default look
    }

    #[test]
    fn bgr_order_is_respected() {
        // Pure blue in BGR = [255, 0, 0].
        let colors = dominant_colors(&[255, 0, 0, 255], 1);
        assert!(colors[0][2] > 0.9 && colors[0][0] < 0.1, "{:?}", colors[0]);
    }

    #[test]
    fn hsv_conversion_matches_primaries() {
        let (h, s, v) = rgb_to_hsv([1.0, 0.0, 0.0]);
        assert!(h.abs() < 1e-3 && (s - 1.0).abs() < 1e-3 && (v - 1.0).abs() < 1e-3);
        let (h, _, _) = rgb_to_hsv([0.0, 1.0, 0.0]);
        assert!((h - 120.0).abs() < 1e-3);
        let (h, _, _) = rgb_to_hsv([0.0, 0.0, 1.0]);
        assert!((h - 240.0).abs() < 1e-3);
    }
}
