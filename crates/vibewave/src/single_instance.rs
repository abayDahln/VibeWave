//! One VibeWave per user session (PRD FR-18).
//!
//! A single named, auto-reset event does both jobs:
//! - The first instance creates it. A later `CreateEventW` with the same name
//!   opens the *same* event and Windows reports `ERROR_ALREADY_EXISTS`, which
//!   is how a second instance knows one is already running.
//! - The second instance then signals the event and exits; the first
//!   instance has a thread blocked on it and opens the settings window.
//!
//! `Local\` scopes the name to the current login session, so two users on one
//! PC each get their own VibeWave.

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject, INFINITE};

const EVENT_NAME: windows::core::PCWSTR = w!("Local\\VibeWave.OpenSettings");

/// Result of trying to become the running instance.
pub enum Claim {
    /// We are the only instance; keep the event to listen on.
    Primary(Listener),
    /// Another instance already runs; it has been asked to open settings
    /// when `notify` was true.
    Secondary,
}

/// The event the primary instance waits on. Lives for the whole process, so
/// the handle is intentionally never closed (Windows closes it at exit).
pub struct Listener {
    /// Raw handle value; `HANDLE` itself is not `Send`.
    raw: usize,
}

/// Become the primary instance, or signal the existing one.
///
/// `notify = false` (login autostart) exits quietly without opening settings.
/// If the event cannot be created at all we run anyway as primary-without-
/// listener rather than refusing to start.
pub fn claim(notify: bool) -> Option<Claim> {
    // SAFETY: plain Win32 call with a static NUL-terminated name and default
    // security; the returned handle is owned by us.
    let event = match unsafe { CreateEventW(None, false, false, EVENT_NAME) } {
        Ok(h) => h,
        Err(e) => {
            crate::log::warn(&format!("single instance: cannot create event: {e}"));
            return None;
        }
    };
    // SAFETY: reads the calling thread's last-error value, which CreateEventW
    // just set (ERROR_ALREADY_EXISTS on success when the event existed).
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if notify {
            // SAFETY: `event` is a valid handle we own.
            if let Err(e) = unsafe { SetEvent(event) } {
                crate::log::warn(&format!("single instance: cannot signal: {e}"));
            }
        }
        // SAFETY: closing our own valid handle once.
        unsafe {
            let _ = CloseHandle(event);
        }
        return Some(Claim::Secondary);
    }
    Some(Claim::Primary(Listener {
        raw: event.0 as usize,
    }))
}

impl Listener {
    /// Spawn a thread that calls `on_signal` each time another instance starts.
    pub fn spawn<F: Fn() + Send + 'static>(self, on_signal: F) {
        let raw = self.raw;
        let spawned = std::thread::Builder::new()
            .name("vibewave-instance".into())
            .spawn(move || {
                let event = HANDLE(raw as *mut core::ffi::c_void);
                loop {
                    // SAFETY: the handle stays valid for the process lifetime
                    // (never closed); blocking here costs no CPU.
                    let rc = unsafe { WaitForSingleObject(event, INFINITE) };
                    if rc.0 != 0 {
                        // WAIT_OBJECT_0 is 0; anything else means the wait
                        // itself failed, so stop instead of spinning.
                        crate::log::warn("single instance: wait failed; listener stopped");
                        return;
                    }
                    on_signal();
                }
            });
        if let Err(e) = spawned {
            crate::log::warn(&format!("single instance: cannot spawn listener: {e}"));
        }
    }
}
