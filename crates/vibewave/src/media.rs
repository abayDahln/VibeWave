//! Watch the Windows media session and publish the current cover-art palette.
//!
//! Colour follows the *active* media session as Windows reports it (PRD FR-9),
//! not whatever is making sound. Windows is the single source of truth, so a
//! paused Spotify session can still supply the colour while YouTube plays.
//!
//! Runs on its own thread: it polls once a second, reads the session thumbnail
//! via Windows Imaging (no image crate needed), extracts a small palette, and
//! hands it to [`SharedSettings`]. Reading a thumbnail takes a few tens of ms,
//! which is why it must never touch the render or audio path (FR-12).

use std::sync::Arc;
use std::time::Duration;

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform,
    ColorManagementMode, ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use vibewave_core::cover_color::palette_from_bgra;

use crate::log;
use crate::settings_state::SharedSettings;

/// How often to re-check the session and its thumbnail.
const POLL: Duration = Duration::from_secs(1);
/// Edge length of the thumbnail we decode for colour extraction.
const THUMB_PX: u32 = 32;
/// How long to keep the previous cover palette while no thumbnail is available
/// (track change / transient failure) before falling back. Long enough to cover
/// the gap between `CurrentSessionChanged` and the new thumbnail being ready.
const NO_THUMB_GRACE: Duration = Duration::from_secs(2);

/// Spawn the media watcher thread.
pub fn spawn(shared: Arc<SharedSettings>) {
    std::thread::Builder::new()
        .name("vibewave-media".into())
        .spawn(move || run(shared))
        .ok();
}

fn run(shared: Arc<SharedSettings>) {
    // SAFETY: COM/WinRT apartment init once for this thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let manager = match Manager::RequestAsync().and_then(|op| op.join()) {
        Ok(m) => m,
        Err(e) => {
            log::warn(&format!("media: session manager unavailable: {e}"));
            return;
        }
    };

    // Wake immediately on session changes so a new track updates the colour
    // without waiting for the poll.
    let wake = Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let wake = Arc::clone(&wake);
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            wake.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }));
    }

    let mut last_hash: Option<u64> = None;
    // When the thumbnail briefly disappears (track change, transient read), hold
    // the previous palette and only clear it after this long with no thumbnail.
    // This is what stops the colour flashing to the fallback between tracks.
    let mut cleared_since: Option<std::time::Instant> = None;
    loop {
        // Sleep until the poll interval or a session-change wake-up.
        let start = std::time::Instant::now();
        while start.elapsed() < POLL {
            if wake.swap(false, std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        match read_thumbnail(&manager) {
            Ok(Some((hash, bgra))) => {
                cleared_since = None;
                if Some(hash) != last_hash {
                    last_hash = Some(hash);
                    let palette = palette_from_bgra(&bgra);
                    log::info(&format!(
                        "media: cover colours updated ({} colour(s))",
                        palette.len()
                    ));
                    // Straight switch to the new cover colour: the previous one
                    // was held through the gap, so there is no fallback flash.
                    shared.set_cover_palette(Some(palette));
                }
            }
            // A transient read failure: keep the last palette (FR-10/FR-12) and
            // start the same hold as "no thumbnail", so a brief glitch does not
            // clear the colour either.
            Err(e) => {
                log::warn(&format!("media: thumbnail read failed: {e}"));
                hold_or_clear(&shared, &mut last_hash, &mut cleared_since);
            }
            // No thumbnail right now (a track change in progress, or no session).
            Ok(None) => {
                hold_or_clear(&shared, &mut last_hash, &mut cleared_since);
            }
        }
    }
}

/// Hold the previous cover palette through a short gap; clear it to the fallback
/// exactly once when the gap exceeds [`NO_THUMB_GRACE`].
fn hold_or_clear(
    shared: &SharedSettings,
    last_hash: &mut Option<u64>,
    cleared_since: &mut Option<std::time::Instant>,
) {
    let since = *cleared_since.get_or_insert_with(std::time::Instant::now);
    if since.elapsed() >= NO_THUMB_GRACE && last_hash.is_some() {
        *last_hash = None;
        log::info("media: no active session thumbnail; using fallback colour");
        shared.set_cover_palette(None);
    }
}

/// Read and decode the current session's thumbnail to BGRA, plus a hash of the
/// pixels so "cover changed but title same" is detected (PRD FR-9).
///
/// Returns `Ok(None)` when there is no current session or no thumbnail.
fn read_thumbnail(manager: &Manager) -> windows::core::Result<Option<(u64, Vec<u8>)>> {
    let Ok(session) = manager.GetCurrentSession() else {
        return Ok(None);
    };
    let bgra = decode_thumbnail(&session)?;
    Ok(bgra.map(|px| (fnv1a(&px), px)))
}

fn decode_thumbnail(session: &Session) -> windows::core::Result<Option<Vec<u8>>> {
    let props = session.TryGetMediaPropertiesAsync()?.join()?;
    let Ok(thumb_ref) = props.Thumbnail() else {
        return Ok(None);
    };
    let stream = thumb_ref.OpenReadAsync()?.join()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;

    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(THUMB_PX)?;
    transform.SetScaledHeight(THUMB_PX)?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;

    let pixels = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            &transform,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .join()?
        .DetachPixelData()?
        .to_vec();
    Ok(Some(pixels))
}

/// FNV-1a hash over the pixel bytes.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash = (hash ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
