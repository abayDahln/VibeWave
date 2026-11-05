//! S-1 spike: native edge-glow overlay window.
//!
//! The window is a plain Win32 popup with these extended styles:
//! - WS_EX_LAYERED | WS_EX_TRANSPARENT: hit-testing skips the window, so every
//!   click/scroll lands on whatever is underneath (click-through).
//! - WS_EX_NOACTIVATE: clicking or showing never activates it (no focus steal).
//! - WS_EX_TOOLWINDOW: not listed in the taskbar or Alt+Tab.
//! - WS_EX_TOPMOST: above normal windows.
//! - WS_EX_NOREDIRECTIONBITMAP: no GDI surface; content comes only from
//!   DirectComposition, which supports per-pixel alpha.
//!
//! Pixels: D3D11 device -> DXGI swap chain "for composition" (premultiplied
//! alpha) -> drawn with Direct2D -> shown via a DirectComposition visual.

use windows::core::{w, Interface, Result};
use windows_numerics::Vector2;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
    D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1LinearGradientBrush, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_BUFFER_PRECISION_8BPC_UNORM,
    D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
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
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_MOUSEACTIVATE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Rectangles of the primary monitor in physical pixels.
pub struct MonitorRects {
    pub full: RECT,
    pub work: RECT,
}

pub fn primary_monitor() -> MonitorRects {
    // SAFETY: plain Win32 queries with a correctly sized out-struct.
    unsafe {
        let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(hmon, &mut mi);
        MonitorRects { full: mi.rcMonitor, work: mi.rcWork }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        // Belt and braces: never activate even if a click somehow reaches us.
        return LRESULT(MA_NOACTIVATE as isize);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

pub struct Overlay {
    pub hwnd: HWND,
    pub width: u32,
    pub height: u32,
    swap_chain: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    _target_bitmap: ID2D1Bitmap1,
    dcomp: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    // One gradient brush per edge: top, bottom, left, right.
    brushes: [ID2D1LinearGradientBrush; 4],
    color: [f32; 3],
    thickness: f32,
}

impl Overlay {
    pub fn create(rect: RECT, thickness: f32) -> Result<Self> {
        let width = (rect.right - rect.left) as u32;
        let height = (rect.bottom - rect.top) as u32;
        // SAFETY: all calls are Win32/COM FFI with valid, owned arguments; COM
        // objects are reference counted by the `windows` crate wrappers.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("VibeWaveSpikeOverlay");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOREDIRECTIONBITMAP,
                class,
                w!("VibeWave spike overlay"),
                WS_POPUP,
                rect.left,
                rect.top,
                width as i32,
                height as i32,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            // Layered windows stay invisible until attributes are set. Alpha 255
            // here means "use the composition content's own per-pixel alpha".
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;

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
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
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

            let color = [0.3, 0.6, 1.0];
            let brushes = make_brushes(&dc, width as f32, height as f32, thickness, color)?;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            Ok(Self {
                hwnd,
                width,
                height,
                swap_chain,
                dc,
                _target_bitmap: target_bitmap,
                dcomp,
                _dcomp_target: dcomp_target,
                _visual: visual,
                brushes,
                color,
                thickness,
            })
        }
    }

    pub fn set_color(&mut self, color: [f32; 3]) -> Result<()> {
        self.color = color;
        self.brushes = make_brushes(
            &self.dc,
            self.width as f32,
            self.height as f32,
            self.thickness,
            color,
        )?;
        Ok(())
    }

    /// Draw one frame. `levels` = [top/bottom, left/right] opacity 0..1.
    /// Present(1) waits for vblank, so this paces the caller to the refresh rate.
    pub fn frame(&self, levels: [f32; 2]) -> Result<()> {
        let (w, h, t) = (self.width as f32, self.height as f32, self.thickness);
        let rects = [
            D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: t },
            D2D_RECT_F { left: 0.0, top: h - t, right: w, bottom: h },
            D2D_RECT_F { left: 0.0, top: 0.0, right: t, bottom: h },
            D2D_RECT_F { left: w - t, top: 0.0, right: w, bottom: h },
        ];
        // SAFETY: COM calls on objects owned by self; BeginDraw/EndDraw paired.
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            for (i, (rect, brush)) in rects.iter().zip(&self.brushes).enumerate() {
                brush.SetOpacity(levels[i / 2].clamp(0.0, 1.0));
                self.dc.FillRectangle(rect, brush);
            }
            self.dc.EndDraw(None, None)?;
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Clear to fully transparent and show it (used before stopping rendering).
    pub fn clear(&self) -> Result<()> {
        self.frame([0.0, 0.0])
    }

    /// Re-assert topmost z-order without activating, moving, or resizing.
    pub fn reassert_topmost(&self) {
        // SAFETY: valid hwnd owned by self.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    }

    pub fn commit(&self) -> Result<()> {
        // SAFETY: owned COM object.
        unsafe { self.dcomp.Commit() }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // SAFETY: hwnd created by us on this thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn make_brushes(
    dc: &ID2D1DeviceContext,
    w: f32,
    h: f32,
    t: f32,
    [r, g, b]: [f32; 3],
) -> Result<[ID2D1LinearGradientBrush; 4]> {
    // Straight (non-premultiplied) alpha stops; D2D interpolates them in
    // premultiplied space so the fade to transparent has no dark fringe.
    let stops = [
        D2D1_GRADIENT_STOP { position: 0.0, color: D2D1_COLOR_F { r, g, b, a: 1.0 } },
        D2D1_GRADIENT_STOP { position: 0.35, color: D2D1_COLOR_F { r, g, b, a: 0.4 } },
        D2D1_GRADIENT_STOP { position: 1.0, color: D2D1_COLOR_F { r, g, b, a: 0.0 } },
    ];
    // Gradient runs from the screen edge (stop 0) inward (stop 1).
    let lines = [
        (Vector2 { X: 0.0, Y: 0.0 }, Vector2 { X: 0.0, Y: t }),
        (Vector2 { X: 0.0, Y: h }, Vector2 { X: 0.0, Y: h - t }),
        (Vector2 { X: 0.0, Y: 0.0 }, Vector2 { X: t, Y: 0.0 }),
        (Vector2 { X: w, Y: 0.0 }, Vector2 { X: w - t, Y: 0.0 }),
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
        let mk = |(s, e): (Vector2, Vector2)| {
            dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: s, endPoint: e },
                None,
                &coll,
            )
        };
        Ok([mk(lines[0])?, mk(lines[1])?, mk(lines[2])?, mk(lines[3])?])
    }
}
