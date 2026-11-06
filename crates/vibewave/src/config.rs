//! Load and save `%APPDATA%\VibeWave\settings.ini`.
//!
//! Reading and parsing are separated: parsing and serialising are pure and live
//! in `vibewave-core::settings`; this module only handles path lookup and I/O.

use std::io;
use std::path::PathBuf;

use vibewave_core::settings::Settings;

/// Load settings, logging any problems. A missing or unreadable file yields
/// the defaults with no error — that is the expected first-run state.
pub fn load() -> Settings {
    let Some(path) = settings_path() else {
        crate::log::warn("APPDATA not set; using default settings");
        return Settings::default();
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            crate::log::info("no settings file; using defaults");
            return Settings::default();
        }
        Err(e) => {
            crate::log::warn(&format!(
                "cannot read {}: {e}; using defaults",
                path.display()
            ));
            return Settings::default();
        }
    };

    let (settings, warnings) = Settings::parse(&text);
    for w in warnings {
        crate::log::warn(&format!("settings: {w}"));
    }
    crate::log::info(&format!(
        "settings: theme={} color_mode={} over_taskbar={} thickness_px={} sensitivity={} \
         stay_visible_fullscreen={} monitor={} show_tray_icon={}",
        settings.theme.id(),
        settings.color_mode.id(),
        settings.over_taskbar,
        settings.thickness_px,
        settings.sensitivity,
        settings.stay_visible_fullscreen,
        settings.monitor,
        settings.show_tray_icon
    ));
    settings
}

/// True when no settings file exists yet: this is the first run (FR-18).
pub fn is_first_run() -> bool {
    settings_path().is_some_and(|p| !p.exists())
}

/// Write the settings file (FR-16).
///
/// Writes to `settings.ini.tmp` first, then renames over the real file, so a
/// crash or power loss mid-write never leaves a half-written settings file.
pub fn save(settings: &Settings) -> io::Result<()> {
    let path = settings_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA not set"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("ini.tmp");
    std::fs::write(&tmp, settings.to_ini())?;
    std::fs::rename(&tmp, &path)
}

/// Save and log the outcome; for callers that cannot do anything else on error.
pub fn save_logged(settings: &Settings) {
    if let Err(e) = save(settings) {
        crate::log::warn(&format!("cannot save settings: {e}"));
    }
}

/// `%APPDATA%\VibeWave\settings.ini`, or `None` if `APPDATA` is unset.
pub fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    let mut p = PathBuf::from(base);
    p.push("VibeWave");
    p.push("settings.ini");
    Some(p)
}
