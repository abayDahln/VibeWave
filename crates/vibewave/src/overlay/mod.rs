//! Overlay subsystem: window + renderer + the frame loop.
//!
//! The loop owns the window and renderer, pumps Win32 messages (so the window
//! stays responsive and Alt+Tab etc. behave), and draws one frame per present.
//! `Present(1)` waits for vblank, so an unthrottled loop naturally runs at the
//! display refresh rate.
//!
//! The loop also follows the chosen monitor (multi-monitor target, live
//! "over taskbar") and hides the glow while a fullscreen app covers that
//! monitor unless the user chose "Stay visible in fullscreen" (FR-13).

pub mod fullscreen;
pub mod renderer;
pub mod window;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vibewave_core::bars::{compose_bars, BarColorMode, BarsFrame, BarsParams, SPECTRUM_BINS};
use vibewave_core::compose::{compose, ComposeParams, FrameInput, GlowFrame};
use vibewave_core::envelope::BandEnvelopes;
use vibewave_core::geometry::{overlay_rect, pick_monitor, scale_px, Rect as CoreRect};
use vibewave_core::level_slot::LevelSlot;
use vibewave_core::palette::{Palette, MAX_PALETTE};
use vibewave_core::settings::Theme;
use vibewave_core::visual;

use crate::log;
use crate::monitors;
use crate::settings_state::SharedSettings;
use crate::stats::Stats;
use crate::stats::StatsSnapshot;
use renderer::{RenderError, Renderer};
use window::OverlayWindow;

/// How often the loop wakes while the glow is hidden (tray "Active" off, or a
/// fullscreen app covers the monitor).
const IDLE_POLL: Duration = Duration::from_millis(50);
/// How often the target monitor geometry is re-read, to follow resolution
/// changes, a moved taskbar, or a monitor being plugged in or out.
const LAYOUT_POLL: Duration = Duration::from_secs(1);
/// How often fullscreen is checked (spike S-4: detected within one 250 ms poll).
const FULLSCREEN_POLL: Duration = Duration::from_millis(250);

/// Where the overlay sits: the window rect plus the full rect of its monitor
/// (the latter is what a fullscreen app has to cover).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    /// Display number of the chosen monitor (for logs).
    number: u32,
    rect: CoreRect,
    monitor: CoreRect,
}

/// Resolve the monitor setting into a placement. `None` only when Windows
/// reports no monitor at all.
fn placement(wanted: u32, over_taskbar: bool) -> Option<Placement> {
    let list = monitors::list();
    let m = list[pick_monitor(&list, wanted)?];
    Some(Placement {
        number: m.number,
        rect: overlay_rect(m.full, m.work, over_taskbar),
        monitor: m.full,
    })
}

/// The overlay window together with its renderer.
///
/// Field order matters: Rust drops fields top to bottom, so the renderer (which
/// holds a DirectComposition target bound to the window) is released before
/// the window is destroyed.
struct Surface {
    renderer: Renderer,
    window: OverlayWindow,
    dpi: u32,
}

impl Surface {
    fn create(rect: CoreRect, palette: Palette) -> Result<Self, RenderError> {
        let win32_rect = windows::Win32::Foundation::RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        let window = OverlayWindow::create(win32_rect).map_err(|e| {
            log::error(&format!("cannot create overlay window: {e}"));
            RenderError::DeviceLost
        })?;
        let renderer = Renderer::new(window.hwnd(), win32_rect, palette)?;
        let dpi = window.dpi();
        Ok(Self {
            renderer,
            window,
            dpi,
        })
    }
}

/// Run the overlay until `seconds` elapse (or forever if `None`).
///
/// Reads `live` every frame, so a change made in the settings window is visible
/// on the next frame (FR-15). A change of target monitor or of "over taskbar"
/// moves the overlay by building a new window + renderer on the new rectangle
/// and dropping the old one. Returns `Err` only if the overlay cannot be
/// created at all at start (no GPU or no DirectComposition), matching the CLI
/// contract for a non-zero exit.
pub fn run(
    live: Arc<SharedSettings>,
    levels: Arc<LevelSlot>,
    seconds: Option<u64>,
    track_stats: bool,
) -> Result<(), RenderError> {
    let initial = live.get();
    let mut target = (initial.settings.monitor, initial.settings.over_taskbar);
    let Some(mut placed) = placement(target.0, target.1) else {
        log::error("cannot read monitor info: no monitor found");
        return Err(RenderError::DeviceLost);
    };
    let mut surface = Surface::create(placed.rect, initial.palette)?;
    log_placement(&placed, &surface);

    let mut frame = GlowFrame::new();
    let mut bars_frame = BarsFrame::new();
    let mut spectrum = [0.0f32; SPECTRUM_BINS];
    let mut env = BandEnvelopes::default();
    let mut stats = Stats::new();
    let start = Instant::now();
    let mut last = Instant::now();
    let mut applied_palette = initial.palette;
    let mut cleared = false;
    let mut last_layout = Instant::now();
    // `None` = never checked, so the first frame checks at once.
    let mut last_fullscreen: Option<Instant> = None;
    let mut fullscreen = false;

    loop {
        // Pump pending window messages so the popup behaves and we can exit.
        if !pump_messages() {
            break;
        }

        if let Some(limit) = seconds {
            if start.elapsed() >= Duration::from_secs(limit) {
                break;
            }
        }

        let now = Instant::now();
        let dt = (now - last).as_secs_f32();
        last = now;

        // Pick up any change (from the settings window or a new track) this frame.
        let current = live.get();

        // Follow the target monitor: at once when the setting changes, and
        // once a second for display changes Windows makes on its own.
        let wanted = (current.settings.monitor, current.settings.over_taskbar);
        if wanted != target || now - last_layout >= LAYOUT_POLL {
            target = wanted;
            last_layout = now;
            if let Some(p) = placement(wanted.0, wanted.1).filter(|p| *p != placed) {
                // Build the new surface first: if that fails, keep drawing on
                // the old one instead of losing the glow. Assigning drops the
                // old surface (renderer, then window).
                match Surface::create(p.rect, current.palette) {
                    Ok(s) => {
                        surface = s;
                        placed = p;
                        applied_palette = current.palette;
                        cleared = false;
                        log_placement(&placed, &surface);
                    }
                    Err(e) => log::warn(&format!("overlay: cannot move to new monitor: {e}")),
                }
            }
        }

        // FR-13: hide while a fullscreen app covers our monitor, unless the
        // user asked the glow to stay visible.
        if last_fullscreen.is_none_or(|t| now - t >= FULLSCREEN_POLL) {
            last_fullscreen = Some(now);
            let fs = fullscreen::covers_monitor(placed.monitor);
            if fs != fullscreen {
                fullscreen = fs;
                log::info(&format!(
                    "overlay: fullscreen app {}",
                    if fs { "detected" } else { "gone" }
                ));
            }
            if fs && current.settings.stay_visible_fullscreen {
                surface.window.raise_topmost();
            }
        }
        let hidden = !current.active || (fullscreen && !current.settings.stay_visible_fullscreen);

        // Hidden: clear once, then idle without drawing. Messages are still
        // pumped so the window stays healthy.
        if hidden {
            if !cleared {
                surface.renderer.clear()?;
                cleared = true;
            }
            std::thread::sleep(IDLE_POLL);
            last = Instant::now();
            continue;
        }
        cleared = false;

        if current.palette != applied_palette {
            surface.renderer.set_palette(current.palette)?;
            applied_palette = current.palette;
        }
        let (w, h) = (surface.window.width as f32, surface.window.height as f32);
        let (bass, mid, treble, packet_qpc) = levels.load_levels();

        match current.settings.theme {
            Theme::Glow => {
                let params = ComposeParams {
                    thickness_px: scale_px(current.settings.thickness_px, surface.dpi),
                    intensity: current.settings.intensity,
                    palette: current.palette,
                    direction: current.settings.direction,
                };
                // Latest raw levels → per-frame smoothing → composition. The
                // smoothing time scale is fixed (was the Character slider).
                env.set_time_scale(visual::SMOOTHING_SCALE);
                let smoothed = env.update(bass, mid, treble, dt);
                let t_s = start.elapsed().as_secs_f32();

                compose(
                    &mut frame,
                    w,
                    h,
                    &params,
                    &FrameInput {
                        levels: smoothed,
                        t_s,
                        dt,
                        // US1/US2: always drawn; fade arrives with US3.
                        fade: 1.0,
                    },
                );

                surface.renderer.draw(&frame)?;
            }
            Theme::Bars => {
                // Bars are driven by the per-bar spectrum (each bar its own bin).
                // A failed read keeps the previous spectrum. The bars module has
                // its own attack/release feel, so no glow envelope is applied.
                levels.load_spectrum(&mut spectrum);
                // Multi-colour modes need several colours. If the cover/custom
                // source only yields one (a monochrome cover, or a single custom
                // colour), expand it into hue-shifted variants so the modes still
                // show variety. A user's multi-colour custom list is used as-is.
                // Single-colour mode always uses the palette unchanged.
                let palette = if current.settings.bars_color_mode == BarColorMode::Off {
                    current.palette
                } else if current.palette.len() >= 2 {
                    // A multi-colour source (colourful cover, or the user's
                    // custom list) is honoured exactly.
                    current.palette
                } else {
                    // Single colour: expand so the modes still show variety.
                    current.palette.with_at_least(MAX_PALETTE)
                };
                let params = BarsParams {
                    thickness_px: scale_px(current.settings.bars_thickness_px, surface.dpi),
                    max_len_frac: current.settings.bars_max_len_frac,
                    center_profile: current.settings.bars_center_profile,
                    beat_response: current.settings.bars_beat_response,
                    opacity: current.settings.intensity,
                    palette,
                    color_mode: current.settings.bars_color_mode,
                    position: current.settings.bars_position,
                    rounded: current.settings.bars_rounded,
                };
                compose_bars(&mut bars_frame, w, h, &spectrum, &params);
                surface.renderer.draw_bars(&bars_frame)?;
            }
        }
        stats.on_frame(packet_qpc, dt);

        if track_stats && stats.due() {
            report(&stats.take_snapshot());
        }
    }

    let _ = surface.renderer.clear();
    Ok(())
}

fn log_placement(p: &Placement, s: &Surface) {
    log::info(&format!(
        "overlay: display={} at ({},{}) {}x{} dpi={}",
        p.number, p.rect.left, p.rect.top, s.window.width, s.window.height, s.dpi
    ));
}

/// Drain all pending messages. Returns `false` when the app should quit.
fn pump_messages() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    // SAFETY: standard message pump; MSG is a valid out-struct for PeekMessage.
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

/// Print one periodic stats line.
fn report(s: &StatsSnapshot) {
    log::info(&format!(
        "stats: fps={:.1} worst_frame_ms={:.1} latency p50={:.1} p95={:.1} max={:.1} ms frames={}",
        s.fps, s.worst_frame_ms, s.latency_p50_ms, s.latency_p95_ms, s.latency_max_ms, s.frames
    ));
}
