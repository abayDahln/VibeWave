//! S-2 spike: WASAPI loopback capture of the default output device.
//!
//! Loopback = "record what the speakers play". Shared mode, polled every
//! 10 ms. Loopback delivers NO packets while nothing plays, so silence is
//! detected from elapsed time since the last loud packet, not from packets.
//!
//! Flags: --seconds N (default 60)

use std::time::{Duration, Instant};
use windows::core::Result;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

const SILENCE_RMS: f32 = 1e-4; // about -80 dBFS
const SILENCE_AFTER: Duration = Duration::from_secs(3);

struct Capture {
    _client: IAudioClient,
    capture: IAudioCaptureClient,
    device_id: String,
    channels: usize,
}

fn default_device_id(en: &IMMDeviceEnumerator) -> Result<String> {
    // SAFETY: COM calls; the PWSTR from GetId is freed with CoTaskMemFree.
    unsafe {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let id = dev.GetId()?;
        let s = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as *const _));
        Ok(s)
    }
}

fn open(en: &IMMDeviceEnumerator) -> Result<Capture> {
    // SAFETY: COM calls; the mix format pointer is freed with CoTaskMemFree
    // after Initialize copied what it needs.
    unsafe {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let id = dev.GetId()?;
        let device_id = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as *const _));
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let fmt = client.GetMixFormat()?;
        let channels = (*fmt).nChannels as usize;
        let bits = (*fmt).wBitsPerSample;
        let rate = (*fmt).nSamplesPerSec;
        // 200 ms buffer (units of 100 ns).
        let r = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            2_000_000,
            0,
            fmt,
            None,
        );
        CoTaskMemFree(Some(fmt as *const _));
        r?;
        println!("open device={device_id} rate={rate} channels={channels} bits={bits}");
        if bits != 32 {
            println!("WARN mix format is not 32-bit float; RMS below is meaningless");
        }
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(Capture { _client: client, capture, device_id, channels })
    }
}

fn qpc_100ns(freq: i64) -> u64 {
    let mut now = 0i64;
    // SAFETY: out-param is a valid i64.
    unsafe {
        let _ = QueryPerformanceCounter(&mut now);
    }
    (now as i128 * 10_000_000 / freq as i128) as u64
}

fn main() -> Result<()> {
    let seconds: u64 = std::env::args()
        .skip_while(|a| a != "--seconds")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    // SAFETY: COM init once for this thread.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let mut freq = 0i64;
    // SAFETY: out-param.
    unsafe { QueryPerformanceFrequency(&mut freq)? };
    // SAFETY: COM object creation.
    let en: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    println!("pid={}", std::process::id());

    let start = Instant::now();
    let mut cap: Option<Capture> = None;
    let mut backoff = Duration::from_millis(500);
    let mut next_retry = Instant::now();
    let mut last_loud = Instant::now() - SILENCE_AFTER;
    let mut silent = true;
    let mut last_report = Instant::now();
    let mut last_dev_check = Instant::now();
    let (mut packets, mut peak, mut lat_sum, mut lat_n, mut lat_max) = (0u32, 0f32, 0u64, 0u64, 0u64);

    while start.elapsed() < Duration::from_secs(seconds) {
        let t = start.elapsed().as_secs_f32();
        if cap.is_none() && Instant::now() >= next_retry {
            match open(&en) {
                Ok(c) => {
                    println!("t={t:.1} capture opened");
                    cap = Some(c);
                    backoff = Duration::from_millis(500);
                }
                Err(e) => {
                    println!("t={t:.1} open failed: {e}; retry in {backoff:?}");
                    next_retry = Instant::now() + backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            }
        }

        let mut failed = false;
        if let Some(c) = &cap {
            // Drain all packets that are ready. No allocation, no locks.
            loop {
                // SAFETY: GetBuffer/ReleaseBuffer are paired; the slice is only
                // read while the buffer is held, and its length is frames*channels.
                let res: Result<bool> = unsafe {
                    let n = c.capture.GetNextPacketSize()?;
                    if n == 0 {
                        Ok(false)
                    } else {
                        let mut data = std::ptr::null_mut();
                        let (mut frames, mut flags, mut qpc) = (0u32, 0u32, 0u64);
                        c.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                        let samples = frames as usize * c.channels;
                        let rms = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || samples == 0 {
                            0.0
                        } else {
                            let s = std::slice::from_raw_parts(data as *const f32, samples);
                            (s.iter().map(|x| x * x).sum::<f32>() / samples as f32).sqrt()
                        };
                        c.capture.ReleaseBuffer(frames)?;
                        packets += 1;
                        peak = peak.max(rms);
                        // Latency: when the first frame hit the device vs now.
                        let now = qpc_100ns(freq);
                        if packets < 3 { println!("raw qpc={qpc} now={now} diff={}", now as i64 - qpc as i64); }
                        let lat = now.saturating_sub(qpc);
                        lat_sum += lat;
                        lat_n += 1;
                        lat_max = lat_max.max(lat);
                        if rms > SILENCE_RMS {
                            last_loud = Instant::now();
                            if silent {
                                silent = false;
                                println!("t={t:.2} AUDIO RESUMED (rms={rms:.4}, latency_ms={:.1})", lat as f64 / 10_000.0);
                            }
                        }
                        Ok(true)
                    }
                };
                match res {
                    Ok(true) => continue,
                    Ok(false) => break,
                    Err(e) => {
                        println!("t={t:.1} capture error: {e} -> recovering");
                        failed = true;
                        break;
                    }
                }
            }
        }
        if failed {
            cap = None;
            next_retry = Instant::now();
        }

        if !silent && last_loud.elapsed() >= SILENCE_AFTER {
            silent = true;
            println!("t={t:.2} SILENT (no audio above threshold for {SILENCE_AFTER:?})");
        }

        // Follow default device changes (FR-7) by polling its id once a second.
        if last_dev_check.elapsed() >= Duration::from_secs(1) {
            last_dev_check = Instant::now();
            if let (Some(c), Ok(id)) = (&cap, default_device_id(&en)) {
                if id != c.device_id {
                    println!("t={t:.1} default device changed -> reopen");
                    cap = None;
                    next_retry = Instant::now();
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            let avg = if lat_n > 0 { lat_sum as f64 / lat_n as f64 / 10_000.0 } else { 0.0 };
            println!(
                "t={t:.0} packets={packets} peak_rms={peak:.4} capture_latency_ms avg={avg:.1} max={:.1} silent={silent}",
                lat_max as f64 / 10_000.0
            );
            (packets, peak, lat_sum, lat_n, lat_max) = (0, 0.0, 0, 0, 0);
            last_report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("done");
    Ok(())
}
