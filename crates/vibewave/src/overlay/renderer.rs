//! Direct2D renderer for the glow frame.
//!
//! Pixel path (from the S-1 spike): D3D11 device → DXGI swap chain created for
//! composition (premultiplied alpha) → Direct2D device context → shown through
//! a DirectComposition visual.
//!
//! To avoid re-creating GPU resources every frame (research R-7), the renderer
//! pre-builds a fixed set of gradient brushes sampled from the current
//! [`Palette`]. Each brush is a linear gradient whose alpha follows the glow's
//! falloff profile and whose colour is one palette sample. Each frame a segment
//! is painted with its nearest two brushes (the second at a fractional opacity),
//! so the frame reads as one continuous gradient. The brushes are rebuilt only
//! when the palette changes (e.g. a new track), never per frame.

use std::fmt;

use vibewave_core::bars::{BarSegment, BarsFrame};
use vibewave_core::compose::{Edge, GlowFrame};
use vibewave_core::palette::Palette;
use vibewave_core::visual;
use windows::core::{Interface, Result as WinResult};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1, ID2D1LinearGradientBrush,
    ID2D1RadialGradientBrush, ID2D1SolidColorBrush, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_BUFFER_PRECISION_8BPC_UNORM,
    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
    DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Vector2;

/// Number of colour brushes sampled from the palette. With the two-brush blend
/// this is fine enough that the gradient has no visible banding.
const COLOR_STEPS: usize = 48;

/// Failure creating or updating the renderer.
#[derive(Debug)]
pub enum RenderError {
    Win32(windows::core::Error),
    /// The GPU device was lost; the caller should rebuild the renderer.
    DeviceLost,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Win32(e) => write!(f, "render error: {e}"),
            RenderError::DeviceLost => write!(f, "graphics device lost"),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<windows::core::Error> for RenderError {
    fn from(e: windows::core::Error) -> Self {
        RenderError::Win32(e)
    }
}

/// The GPU renderer bound to an overlay window.
pub struct Renderer {
    width: f32,
    height: f32,
    palette: Palette,
    swap_chain: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    _target_bitmap: ID2D1Bitmap1,
    _dcomp: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    /// One linear-gradient brush per colour sample, oriented for `Edge::Top`.
    color_brushes: Vec<ID2D1LinearGradientBrush>,
    /// White radial brush for sparkles.
    sparkle_brush: ID2D1RadialGradientBrush,
    /// Flat brush for the Bars theme, recoloured from the palette.
    bars_brush: ID2D1SolidColorBrush,
}

impl Renderer {
    /// Build the renderer for a window of `rect` size.
    pub fn new(hwnd: HWND, rect: RECT, palette: Palette) -> std::result::Result<Self, RenderError> {
        let width = (rect.right - rect.left).max(1) as u32;
        let height = (rect.bottom - rect.top).max(1) as u32;

        // SAFETY: all calls are Win32/COM FFI with valid, owned arguments; COM
        // objects are reference counted by the `windows` crate wrappers.
        unsafe {
            let mut d3d: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
            let d3d = d3d.ok_or_else(windows::core::Error::from_thread)?;
            let dxgi_device: IDXGIDevice = d3d.cast()?;

            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swap_chain = factory.CreateSwapChainForComposition(&d3d, &desc, None)?;

            let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                Some(&D2D1_FACTORY_OPTIONS::default()),
            )?;
            let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
            let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let surface: IDXGISurface = swap_chain.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                ..Default::default()
            };
            let target_bitmap = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            dc.SetTarget(&target_bitmap);

            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let dcomp_target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap_chain)?;
            dcomp_target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let color_brushes = make_color_brushes(&dc, height as f32, &palette)?;
            let sparkle_brush = make_sparkle_brush(&dc, height as f32)?;
            let bars_brush = make_solid_brush(&dc, palette.at(0.0))?;

            Ok(Self {
                width: width as f32,
                height: height as f32,
                palette,
                swap_chain,
                dc,
                _target_bitmap: target_bitmap,
                _dcomp: dcomp,
                _dcomp_target: dcomp_target,
                _visual: visual,
                color_brushes,
                sparkle_brush,
                bars_brush,
            })
        }
    }

    /// Rebuild the gradient brushes for a new palette (rare; only when the
    /// colour source changes, e.g. a new track plays).
    pub fn set_palette(&mut self, palette: Palette) -> std::result::Result<(), RenderError> {
        if palette == self.palette {
            return Ok(());
        }
        self.palette = palette;
        self.color_brushes = make_color_brushes(&self.dc, self.height, &palette)?;
        self.sparkle_brush = make_sparkle_brush(&self.dc, self.height)?;
        // Recolour the Bars brush (SAFETY: COM call on a valid owned brush).
        let [r, g, b] = palette.at(0.0);
        unsafe {
            self.bars_brush.SetColor(&D2D1_COLOR_F { r, g, b, a: 1.0 });
        }
        Ok(())
    }

    /// Draw one composed frame and present it. `Present(1)` waits for vblank,
    /// which paces the caller to the refresh rate.
    pub fn draw(&self, frame: &GlowFrame) -> std::result::Result<(), RenderError> {
        let (w, h) = (self.width, self.height);
        // SAFETY: COM calls on objects owned by self; BeginDraw/EndDraw paired.
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));

            for seg in &frame.segments {
                let (lo, hi, frac) = nearest_brushes(&self.palette, seg.color);
                // The brush gradient runs from the edge inward over the
                // segment's thickness; set its line per edge.
                let (start, end) = gradient_line(seg.edge, w, h, seg.thickness);
                let rect = segment_rect(seg.edge, seg.start, seg.end, seg.thickness, w, h);
                // Draw the lower brush at full opacity, then the upper brush on
                // top at `frac` opacity. The overlap blends the two adjacent
                // colours, so the frame reads as one continuous gradient.
                let lower = &self.color_brushes[lo];
                lower.SetStartPoint(start);
                lower.SetEndPoint(end);
                lower.SetOpacity(seg.opacity);
                self.dc.FillRectangle(&rect, lower);
                if frac > 0.001 && hi != lo {
                    let upper = &self.color_brushes[hi];
                    upper.SetStartPoint(start);
                    upper.SetEndPoint(end);
                    upper.SetOpacity(seg.opacity * frac);
                    self.dc.FillRectangle(&rect, upper);
                }
            }

            for sp in &frame.sparkles {
                if sp.opacity <= 0.0 {
                    continue;
                }
                let r = sp.radius.max(0.5);
                self.sparkle_brush.SetCenter(Vector2 { X: sp.x, Y: sp.y });
                self.sparkle_brush.SetRadiusX(r);
                self.sparkle_brush.SetRadiusY(r);
                self.sparkle_brush.SetOpacity(sp.opacity);
                let rect = D2D_RECT_F {
                    left: sp.x - r,
                    top: sp.y - r,
                    right: sp.x + r,
                    bottom: sp.y + r,
                };
                self.dc.FillRectangle(&rect, &self.sparkle_brush);
            }

            self.dc.EndDraw(None, None)?;
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Draw one composed Bar frame and present it (the Bars theme).
    ///
    /// Bars are flat rectangles of constant thickness; the palette colour (its
    /// first sample) fills them. `Present(1)` paces the caller to the refresh.
    pub fn draw_bars(&self, frame: &BarsFrame) -> std::result::Result<(), RenderError> {
        let (w, h) = (self.width, self.height);
        // SAFETY: COM calls on objects owned by self; BeginDraw/EndDraw paired.
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));

            for seg in &frame.bars {
                let rect = bar_rect(seg, w, h);
                self.bars_brush.SetOpacity(seg.opacity);
                self.dc.FillRectangle(&rect, &self.bars_brush);
            }

            self.dc.EndDraw(None, None)?;
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Clear to fully transparent and present (used before stopping rendering).
    pub fn clear(&self) -> std::result::Result<(), RenderError> {
        // SAFETY: COM calls on objects owned by self; BeginDraw/EndDraw paired.
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
            self.dc.EndDraw(None, None)?;
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }
}

/// Find the two palette samples nearest `color` and the blend fraction.
///
/// Returns `(lower_index, upper_index, frac)`. The samples are taken evenly
/// around the palette (see [`palette_sample`]); `frac` is how far `color` sits
/// from the nearer sample toward its neighbour, used for the two-layer blend.
fn nearest_brushes(palette: &Palette, color: [f32; 3]) -> (usize, usize, f32) {
    let mut best = 0usize;
    let mut best_d = f32::INFINITY;
    for i in 0..COLOR_STEPS {
        let d = dist2(palette_sample(palette, i), color);
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    // Choose the neighbour on the nearer side and blend by relative distance.
    let prev = (best + COLOR_STEPS - 1) % COLOR_STEPS;
    let next = (best + 1) % COLOR_STEPS;
    let d_prev = dist2(palette_sample(palette, prev), color);
    let d_next = dist2(palette_sample(palette, next), color);
    if d_next <= d_prev {
        let frac = frac_between(best_d, d_next);
        (best, next, frac)
    } else {
        let frac = frac_between(best_d, d_prev);
        (prev, best, frac)
    }
}

/// Blend fraction from two squared distances: 0 when at the near sample, up to
/// 0.5 when exactly between the two.
fn frac_between(d_near: f32, d_far: f32) -> f32 {
    let near = d_near.sqrt();
    let far = d_far.sqrt();
    if near + far <= f32::EPSILON {
        0.0
    } else {
        (near / (near + far)).clamp(0.0, 0.5)
    }
}

/// The colour of palette sample `i` of `COLOR_STEPS`, evenly around the frame.
fn palette_sample(palette: &Palette, i: usize) -> [f32; 3] {
    palette.at(i as f32 / COLOR_STEPS as f32)
}

/// Squared Euclidean distance between two colours.
fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Rectangle covering one segment (pixels along the edge × thickness inward).
fn segment_rect(edge: Edge, start: f32, end: f32, thickness: f32, w: f32, h: f32) -> D2D_RECT_F {
    match edge {
        Edge::Top => D2D_RECT_F {
            left: start,
            top: 0.0,
            right: end,
            bottom: thickness,
        },
        Edge::Bottom => D2D_RECT_F {
            left: start,
            top: h - thickness,
            right: end,
            bottom: h,
        },
        Edge::Left => D2D_RECT_F {
            left: 0.0,
            top: start,
            right: thickness,
            bottom: end,
        },
        Edge::Right => D2D_RECT_F {
            left: w - thickness,
            top: start,
            right: w,
            bottom: end,
        },
    }
}

/// Rectangle for one bar. `seg.pos` is the position along the edge and
/// `seg.thickness` its width across it; `seg.length` is the span reaching inward
/// from the edge (it may overhang outward past the edge, in which case the rect
/// starts at a negative coordinate on the inward axis).
fn bar_rect(seg: &BarSegment, w: f32, h: f32) -> D2D_RECT_F {
    // `outer`/`inner` are offsets along the inward axis (inward-positive), so on
    // the far edges they are measured back from that edge.
    match seg.edge {
        Edge::Top => D2D_RECT_F {
            left: seg.pos,
            top: seg.outer,
            right: seg.pos + seg.thickness,
            bottom: seg.inner,
        },
        Edge::Bottom => D2D_RECT_F {
            left: seg.pos,
            top: h - seg.inner,
            right: seg.pos + seg.thickness,
            bottom: h - seg.outer,
        },
        Edge::Left => D2D_RECT_F {
            left: seg.outer,
            top: seg.pos,
            right: seg.inner,
            bottom: seg.pos + seg.thickness,
        },
        Edge::Right => D2D_RECT_F {
            left: w - seg.inner,
            top: seg.pos,
            right: w - seg.outer,
            bottom: seg.pos + seg.thickness,
        },
    }
}

/// A flat brush of one colour, used by the Bars theme.
fn make_solid_brush(dc: &ID2D1DeviceContext, color: [f32; 3]) -> WinResult<ID2D1SolidColorBrush> {
    let [r, g, b] = color;
    // SAFETY: COM call on a valid device context.
    unsafe { dc.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a: 1.0 }, None) }
}

/// Gradient start (edge, opaque) and end (inward, transparent) points.
fn gradient_line(edge: Edge, w: f32, h: f32, thickness: f32) -> (Vector2, Vector2) {
    match edge {
        Edge::Top => (
            Vector2 { X: 0.0, Y: 0.0 },
            Vector2 {
                X: 0.0,
                Y: thickness,
            },
        ),
        Edge::Bottom => (
            Vector2 { X: 0.0, Y: h },
            Vector2 {
                X: 0.0,
                Y: h - thickness,
            },
        ),
        Edge::Left => (
            Vector2 { X: 0.0, Y: 0.0 },
            Vector2 {
                X: thickness,
                Y: 0.0,
            },
        ),
        Edge::Right => (
            Vector2 { X: w, Y: 0.0 },
            Vector2 {
                X: w - thickness,
                Y: 0.0,
            },
        ),
    }
}

/// Build `COLOR_STEPS` linear-gradient brushes, one per palette sample, all
/// oriented for the top edge (the per-frame code re-points them). Alpha stops
/// come from the glow falloff profile.
fn make_color_brushes(
    dc: &ID2D1DeviceContext,
    edge_len: f32,
    palette: &Palette,
) -> WinResult<Vec<ID2D1LinearGradientBrush>> {
    let mut brushes = Vec::with_capacity(COLOR_STEPS);
    for i in 0..COLOR_STEPS {
        let [r, g, b] = palette_sample(palette, i);
        // Straight (non-premultiplied) stops so the fade has no dark fringe.
        let stops = visual::FALLOFF.map(|(pos, alpha)| D2D1_GRADIENT_STOP {
            position: pos,
            color: D2D1_COLOR_F { r, g, b, a: alpha },
        });
        // SAFETY: COM calls on a valid device context.
        let coll = unsafe {
            dc.CreateGradientStopCollection(
                &stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
            )?
        };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 { X: 0.0, Y: 0.0 },
            endPoint: Vector2 {
                X: 0.0,
                Y: edge_len,
            },
        };
        // SAFETY: COM call on a valid device context.
        brushes.push(unsafe { dc.CreateLinearGradientBrush(&props, None, &coll)? });
    }
    Ok(brushes)
}

/// White radial brush whose alpha falls from the centre, used for sparkles.
fn make_sparkle_brush(dc: &ID2D1DeviceContext, radius: f32) -> WinResult<ID2D1RadialGradientBrush> {
    let stops = [
        D2D1_GRADIENT_STOP {
            position: 0.0,
            color: D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        },
        D2D1_GRADIENT_STOP {
            position: 1.0,
            color: D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.0,
            },
        },
    ];
    // SAFETY: COM calls on a valid device context.
    unsafe {
        let coll = dc.CreateGradientStopCollection(
            &stops,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_BUFFER_PRECISION_8BPC_UNORM,
            D2D1_EXTEND_MODE_CLAMP,
            D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
        )?;
        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
            center: Vector2 { X: 0.0, Y: 0.0 },
            gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 },
            radiusX: radius.max(1.0),
            radiusY: radius.max(1.0),
        };
        dc.CreateRadialGradientBrush(&props, None, &coll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_rect_expands_from_the_edge() {
        let r = segment_rect(Edge::Top, 10.0, 20.0, 5.0, 100.0, 50.0);
        assert_eq!((r.left, r.top, r.right, r.bottom), (10.0, 0.0, 20.0, 5.0));
        let r = segment_rect(Edge::Bottom, 10.0, 20.0, 5.0, 100.0, 50.0);
        assert_eq!((r.left, r.top, r.right, r.bottom), (10.0, 45.0, 20.0, 50.0));
        let r = segment_rect(Edge::Left, 10.0, 20.0, 5.0, 100.0, 50.0);
        assert_eq!((r.left, r.top, r.right, r.bottom), (0.0, 10.0, 5.0, 20.0));
        let r = segment_rect(Edge::Right, 10.0, 20.0, 5.0, 100.0, 50.0);
        assert_eq!(
            (r.left, r.top, r.right, r.bottom),
            (95.0, 10.0, 100.0, 20.0)
        );
    }

    #[test]
    fn gradient_line_runs_inward() {
        let (s, e) = gradient_line(Edge::Top, 100.0, 50.0, 5.0);
        assert_eq!((s.X, s.Y), (0.0, 0.0));
        assert_eq!((e.X, e.Y), (0.0, 5.0));
    }

    #[test]
    fn nearest_brushes_finds_the_right_sample() {
        let p = Palette::new(&[[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
        // A pure-red segment should pick a brush close to the red sample.
        let (lo, _, _) = nearest_brushes(&p, [1.0, 0.0, 0.0]);
        assert!(dist2(palette_sample(&p, lo), [1.0, 0.0, 0.0]) < 0.01);
        // A pure-blue segment should pick a brush close to the blue sample.
        let (lo, _, _) = nearest_brushes(&p, [0.0, 0.0, 1.0]);
        assert!(dist2(palette_sample(&p, lo), [0.0, 0.0, 1.0]) < 0.01);
    }

    #[test]
    fn frac_is_bounded() {
        assert_eq!(frac_between(0.0, 1.0), 0.0);
        assert!(frac_between(1.0, 1.0) <= 0.5);
        assert!(frac_between(1.0, 0.0) >= 0.0);
    }
}
