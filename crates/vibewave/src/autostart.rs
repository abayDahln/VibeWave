//! "Start with Windows" (PRD FR-19) via the per-user `Run` registry key.
//!
//! Windows launches every value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` at login. We store
//! `"<path to vibewave.exe>" --autostart`; the flag tells the app it was started
//! by login so it never opens the settings window (FR-18). Per-user (HKCU), so
//! no admin rights are needed. The registry value is the single source of
//! truth; nothing is mirrored in `settings.ini`.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "VibeWave";

/// Command-line flag written into the `Run` value.
pub const AUTOSTART_FLAG: &str = "--autostart";

/// True when the `Run` value exists.
pub fn is_enabled() -> bool {
    let key = HSTRING::from(RUN_KEY);
    let name = HSTRING::from(VALUE_NAME);
    // SAFETY: both strings are valid NUL-terminated UTF-16 that outlive the
    // call; passing no data buffer only asks whether the value exists.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
    };
    rc == ERROR_SUCCESS
}

/// Turn autostart on (writing the current exe path) or off.
pub fn set(enabled: bool) -> Result<(), String> {
    let key = HSTRING::from(RUN_KEY);
    let name = HSTRING::from(VALUE_NAME);
    if !enabled {
        // SAFETY: valid NUL-terminated strings that outlive the call.
        let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &name) };
        // Already absent counts as success.
        return check(rc, ERROR_FILE_NOT_FOUND);
    }

    let exe = std::env::current_exe().map_err(|e| format!("cannot find own exe: {e}"))?;
    let command = format!("\"{}\" {AUTOSTART_FLAG}", exe.display());
    // REG_SZ data: UTF-16 including the terminating NUL; size is in bytes.
    let data: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = u32::try_from(data.len() * 2).map_err(|_| "exe path too long".to_string())?;
    // SAFETY: `data` is a live buffer of exactly `bytes` bytes; the strings are
    // valid NUL-terminated UTF-16 that outlive the call.
    let rc = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &key,
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            bytes,
        )
    };
    check(rc, ERROR_SUCCESS)
}

fn check(rc: WIN32_ERROR, also_ok: WIN32_ERROR) -> Result<(), String> {
    if rc == ERROR_SUCCESS || rc == also_ok {
        Ok(())
    } else {
        Err(format!("registry error {}", rc.0))
    }
}
