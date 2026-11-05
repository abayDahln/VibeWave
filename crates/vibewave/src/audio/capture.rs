//! WASAPI loopback capture of the default output device.
//!
//! "Loopback" records what the speakers are playing — all system audio,
//! including notification sounds. Capture runs continuously, even during
//! silence, because that is the only way to know when audio returns (FR-006,
//! FR-020). While nothing plays, loopback simply delivers no packets.
//!
//! All setup happens in `open`; `drain` only reads ready packets and allocates
//! nothing, so it is safe to call from the audio thread.

use std::fmt;

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemAlloc, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use vibewave_core::bands::rms_of;

/// `WAVE_FORMAT_IEEE_FLOAT` from `mmreg.h` (the `windows` crate does not export
/// it under `Win32::Media::Audio`).
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;

/// A 200 ms device buffer, in 100 ns units.
const BUFFER_DURATION_100NS: i64 = 2_000_000;

/// Failure opening or using the capture stream.
#[derive(Debug)]
pub enum CaptureError {
    /// A COM/audio call returned an error.
    Com(windows::core::Error),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Com(e) => write!(f, "audio error: {e}"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<windows::core::Error> for CaptureError {
    fn from(e: windows::core::Error) -> Self {
        CaptureError::Com(e)
    }
}

/// Identity of the device a stream was opened against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub sample_rate: u32,
    pub channels: usize,
}

/// Summary and samples of one drained packet, passed to the caller by reference
/// so nothing is copied.
pub struct Packet<'a> {
    /// Interleaved f32 samples (`frames * channels` long).
    pub samples: &'a [f32],
    /// Number of frames in the packet.
    pub frames: u32,
    /// RMS over all samples.
    pub rms: f32,
    /// Device timestamp of the first frame, in 100 ns units.
    pub qpc_100ns: u64,
}

/// One open loopback stream.
pub struct Capture {
    _client: IAudioClient,
    capture: IAudioCaptureClient,
    pub info: DeviceInfo,
}

/// Initialise COM for the current thread in multithreaded mode.
///
/// `RPC_E_CHANGED_MODE` (already initialised differently) is tolerated; any
/// other failure is not fatal here because the audio calls report their own
/// errors.
pub fn init_com() {
    // SAFETY: standard COM init for a thread we own.
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// Create a default-device enumerator.
pub fn enumerator() -> Result<IMMDeviceEnumerator, CaptureError> {
    // SAFETY: COM object creation; the returned interface is ref-counted.
    let en: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    Ok(en)
}

/// Read the current default render device's endpoint id.
pub fn default_device_id(en: &IMMDeviceEnumerator) -> Result<String, CaptureError> {
    // SAFETY: COM calls; the PWSTR from GetId is freed with CoTaskMemFree.
    unsafe {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let id = dev.GetId()?;
        let s = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as *const _));
        Ok(s)
    }
}

impl Capture {
    /// Open the current default output device for loopback capture.
    pub fn open(en: &IMMDeviceEnumerator) -> Result<Self, CaptureError> {
        // SAFETY: COM calls; both format buffers are freed after Initialize has
        // copied what it needs, and GetBuffer/ReleaseBuffer are paired.
        unsafe {
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let id = dev.GetId()?;
            let device_id = id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id.0 as *const _));

            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
            let mix = client.GetMixFormat()?;
            let channels = (*mix).nChannels as usize;
            let rate = (*mix).nSamplesPerSec;
            let bits = (*mix).wBitsPerSample;

            // Loopback usually delivers float32 already. If not, ask WASAPI to
            // convert to float32 for us so the analyzer always sees f32.
            let (flags, fmt): (_, *const WAVEFORMATEX) =
                if bits == 32 && (*mix).wFormatTag == WAVE_FORMAT_IEEE_FLOAT {
                    (AUDCLNT_STREAMFLAGS_LOOPBACK, mix as *const WAVEFORMATEX)
                } else {
                    (
                        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                        float32_format(rate, channels),
                    )
                };

            let res = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                BUFFER_DURATION_100NS,
                0,
                fmt,
                None,
            );

            // Free whichever format buffer we used, but never the original mix
            // format (WASAPI owns that one until we free it below).
            if fmt as *const u8 != mix as *const u8 {
                CoTaskMemFree(Some(fmt as *const _));
            }
            CoTaskMemFree(Some(mix as *const _));
            res?;

            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Self {
                _client: client,
                capture,
                info: DeviceInfo {
                    id: device_id,
                    sample_rate: rate,
                    channels,
                },
            })
        }
    }

    /// Drain all ready packets, invoking `on_packet` for each. Allocates
    /// nothing, so it is safe to call in the real-time loop.
    pub fn drain<F: FnMut(Packet<'_>)>(&self, on_packet: &mut F) -> Result<(), CaptureError> {
        loop {
            // SAFETY: GetBuffer/ReleaseBuffer are paired; the sample slice is
            // read only while the buffer is held, with length frames*channels.
            let step: Result<bool, CaptureError> = unsafe {
                let next = self.capture.GetNextPacketSize()?;
                if next == 0 {
                    Ok(false)
                } else {
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    let mut qpc = 0u64;
                    self.capture.GetBuffer(
                        &mut data,
                        &mut frames,
                        &mut flags,
                        None,
                        Some(&mut qpc),
                    )?;

                    let channels = self.info.channels;
                    let count = frames as usize * channels;
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    let samples: &[f32] = if silent || count == 0 || data.is_null() {
                        &[]
                    } else {
                        std::slice::from_raw_parts(data as *const f32, count)
                    };
                    on_packet(Packet {
                        samples,
                        frames,
                        rms: rms_of(samples),
                        qpc_100ns: qpc,
                    });

                    self.capture.ReleaseBuffer(frames)?;
                    Ok(true)
                }
            };
            match step {
                Ok(true) => continue,
                Ok(false) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}

/// Build a 32-bit float `WAVEFORMATEX` in the COM allocator for
/// `AUTOCONVERTPCM`. The caller frees it with `CoTaskMemFree`.
///
/// Returns a null pointer if allocation fails; `Initialize` then reports an
/// error, which the caller treats like any other capture failure.
fn float32_format(sample_rate: u32, channels: usize) -> *const WAVEFORMATEX {
    let size = std::mem::size_of::<WAVEFORMATEX>();
    // SAFETY: COM allocator; the block is exactly `size` bytes.
    let raw = unsafe { CoTaskMemAlloc(size) } as *mut WAVEFORMATEX;
    if raw.is_null() {
        return std::ptr::null();
    }
    let wf = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: channels as u16,
        nSamplesPerSec: sample_rate,
        wBitsPerSample: 32,
        nBlockAlign: (channels * 4) as u16,
        nAvgBytesPerSec: sample_rate * (channels as u32 * 4),
        cbSize: 0,
    };
    // SAFETY: `raw` points to at least `size` bytes of writable memory.
    unsafe {
        std::ptr::write(raw, wf);
    }
    raw as *const WAVEFORMATEX
}
