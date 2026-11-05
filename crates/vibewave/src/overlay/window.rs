//! The Win32 overlay window.
//!
//! A plain popup with layered + composition styles that make it invisible to
//! input and to the shell:
//! - `WS_EX_LAYERED | WS_EX_TRANSPARENT` → hit-testing skips it (click-through).
//! - `WS_EX_NOACTIVATE` → showing or clicking never steals focus.
//! - `WS_EX_TOOLWINDOW` → not in the taskbar or Alt+Tab.
//! - `WS_EX_TOPMOST` → above normal windows.
//! - `WS_EX_NOREDIRECTIONBITMAP` → no GDI surface; content is pure composition.
//!
//! The pixel path (created in `renderer.rs`) is D3D11 → DXGI composition swap
//! chain → Direct2D → DirectComposition.

use std::fmt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_MOUSEACTIVATE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Failure creating the overlay window.
#[derive(Debug)]
pub enum WindowError {
    /// A Win32 call reported an error.
    Win32(windows::core::Error),
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WindowError::Win32(e) => write!(f, "win32 error: {e}"),
        }
    }
}

impl std::error::Error for WindowError {}

impl From<windows::core::Error> for WindowError {
    fn from(e: windows::core::Error) -> Self {
        WindowError::Win32(e)
    }
}

/// The overlay window. Owns the `HWND`; dropping it destroys the window.
pub struct OverlayWindow {
    hwnd: HWND,
    pub width: u32,
    pub height: u32,
}

impl OverlayWindow {
    /// Create the overlay covering `rect` (position + size in physical pixels).
    pub fn create(rect: RECT) -> Result<Self, WindowError> {
        let width = (rect.right - rect.left).max(1) as u32;
        let height = (rect.bottom - rect.top).max(1) as u32;

        // SAFETY: all calls are Win32 FFI with valid, owned arguments.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("VibeWaveOverlay");
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
                PCWSTR::from_raw(w!("VibeWave overlay").as_ptr()),
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
            // Layered windows start invisible; alpha 255 means "use the
            // composition content's own per-pixel alpha".
            SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0),
                255,
                LWA_ALPHA,
            )?;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(Self {
                hwnd,
                width,
                height,
            })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Put the window back at the top of the topmost band.
    ///
    /// A borderless-fullscreen app can itself be topmost and was activated
    /// after us, so it sits above the overlay. Re-asserting `HWND_TOPMOST`
    /// (without activating) brings the glow back over it — used when the user
    /// chose "Stay visible in fullscreen" (FR-13).
    pub fn raise_topmost(&self) {
        // SAFETY: valid hwnd owned by self; no move/size/activation.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// Effective DPI of the window (fallback 96 if the query fails).
    pub fn dpi(&self) -> u32 {
        // SAFETY: valid hwnd owned by self.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            96
        } else {
            dpi
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // SAFETY: hwnd was created by us on this thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        // Never activate even if a click somehow reaches the window.
        return LRESULT(MA_NOACTIVATE as isize);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
