//! S-3 spike: read the active media session the way Windows reports it.
//!
//! Polls once per second and prints only when something changes, plus logs
//! the change events Windows raises. Thumbnails are decoded downscaled to
//! 32x32 BGRA (cheap) and hashed, so "cover changed but title same" is caught.
//!
//! Flags: --seconds N (default 60)

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::Result;
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

#[derive(PartialEq, Clone, Debug, Default)]
struct Snapshot {
    sessions: Vec<String>,
    current: Option<String>,
    title: String,
    artist: String,
    thumb: String, // "none" | "error: ..." | "hash=.. avg=#rrggbb"
}

struct Thumb {
    hash: u64,
    avg: [u8; 3],
    read_ms: f64,
    decode_ms: f64,
}

fn status(s: &Session) -> String {
    s.GetPlaybackInfo()
        .and_then(|p| p.PlaybackStatus())
        .map(|st| format!("{st:?}"))
        .unwrap_or_else(|e| format!("err({e})"))
}

fn read_thumb(s: &Session) -> Result<Option<Thumb>> {
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let Ok(thumb_ref) = props.Thumbnail() else { return Ok(None) };
    let t0 = Instant::now();
    let stream = thumb_ref.OpenReadAsync()?.join()?;
    let read_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = Instant::now();
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let tf = BitmapTransform::new()?;
    tf.SetScaledWidth(32)?;
    tf.SetScaledHeight(32)?;
    tf.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    let px = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            &tf,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .join()?
        .DetachPixelData()?;
    let decode_ms = t1.elapsed().as_secs_f64() * 1000.0;
    // FNV-1a over the downscaled pixels; average color as a first color guess.
    let mut hash: u64 = 0xcbf29ce484222325;
    let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
    for p in px.chunks_exact(4) {
        for &byte in &p[..3] {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
        b += p[0] as u64;
        g += p[1] as u64;
        r += p[2] as u64;
    }
    let n = (px.len() / 4).max(1) as u64;
    Ok(Some(Thumb {
        hash,
        avg: [(r / n) as u8, (g / n) as u8, (b / n) as u8],
        read_ms,
        decode_ms,
    }))
}

fn snapshot(mgr: &Manager) -> Result<(Snapshot, Option<Thumb>)> {
    let mut snap = Snapshot::default();
    for s in mgr.GetSessions()? {
        let id = s.SourceAppUserModelId().unwrap_or_default().to_string();
        snap.sessions.push(format!("{id} [{}]", status(&s)));
    }
    let Ok(cur) = mgr.GetCurrentSession() else {
        snap.thumb = "none".into();
        return Ok((snap, None));
    };
    snap.current = Some(format!(
        "{} [{}]",
        cur.SourceAppUserModelId().unwrap_or_default(),
        status(&cur)
    ));
    if let Ok(p) = cur.TryGetMediaPropertiesAsync().and_then(|op| op.join()) {
        snap.title = p.Title().unwrap_or_default().to_string();
        snap.artist = p.Artist().unwrap_or_default().to_string();
    }
    let thumb = match read_thumb(&cur) {
        Ok(Some(t)) => {
            snap.thumb = format!(
                "hash={:016x} avg=#{:02x}{:02x}{:02x}",
                t.hash, t.avg[0], t.avg[1], t.avg[2]
            );
            Some(t)
        }
        Ok(None) => {
            snap.thumb = "none".into();
            None
        }
        Err(e) => {
            snap.thumb = format!("error: {e}");
            None
        }
    };
    Ok((snap, thumb))
}

fn main() -> Result<()> {
    let seconds: u64 = std::env::args()
        .skip_while(|a| a != "--seconds")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    // SAFETY: COM/WinRT apartment init once for this thread.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    println!("pid={}", std::process::id());

    let t0 = Instant::now();
    let mgr = Manager::RequestAsync()?.join()?;
    println!("manager ready in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);

    // Event counters: proves Windows notifies us, so production can avoid polling.
    let cur_changed = Arc::new(AtomicU32::new(0));
    let sess_changed = Arc::new(AtomicU32::new(0));
    let (c1, c2) = (cur_changed.clone(), sess_changed.clone());
    mgr.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
        c1.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }))?;
    mgr.SessionsChanged(&TypedEventHandler::new(move |_, _| {
        c2.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }))?;

    let start = Instant::now();
    let mut last = Snapshot::default();
    let mut last_events = (0, 0);
    while start.elapsed() < Duration::from_secs(seconds) {
        let t = start.elapsed().as_secs_f32();
        let ts = Instant::now();
        match snapshot(&mgr) {
            Ok((snap, thumb)) => {
                if snap != last {
                    println!("t={t:.1} CHANGE (snapshot took {:.1} ms)", ts.elapsed().as_secs_f64() * 1000.0);
                    println!("  sessions: {:?}", snap.sessions);
                    println!("  current:  {:?}", snap.current);
                    println!("  title:    {:?} / {:?}", snap.title, snap.artist);
                    println!("  thumb:    {}", snap.thumb);
                    if let Some(th) = thumb {
                        println!("  thumb_ms: read={:.1} decode32={:.1}", th.read_ms, th.decode_ms);
                    }
                    if snap.title == last.title && snap.thumb != last.thumb && !snap.title.is_empty() {
                        println!("  NOTE: cover changed with same title");
                    }
                    last = snap;
                }
            }
            Err(e) => println!("t={t:.1} snapshot error: {e}"),
        }
        let ev = (cur_changed.load(Ordering::Relaxed), sess_changed.load(Ordering::Relaxed));
        if ev != last_events {
            println!("t={t:.1} events: current_changed={} sessions_changed={}", ev.0, ev.1);
            last_events = ev;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("done");
    Ok(())
}
