//! Minimal stderr logger: `HH:MM:SS.mmm LEVEL message`.
//!
//! No logging crate: feature 1 has a handful of non-hot log lines (errors,
//! reconnects, silence transitions), so a small formatter is enough. Times use
//! local wall-clock via `GetLocalTime`.

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;
/// Severity prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Write one log line to stderr.
pub fn log(level: Level, msg: &str) {
    // SAFETY: `GetLocalTime` takes no arguments and returns a fully initialised
    // SYSTEMTIME by value.
    let st: SYSTEMTIME = unsafe { GetLocalTime() };
    eprintln!(
        "{:02}:{:02}:{:02}.{:03} {} {}",
        st.wHour,
        st.wMinute,
        st.wSecond,
        st.wMilliseconds,
        level.tag(),
        msg
    );
}

pub fn info(msg: &str) {
    log(Level::Info, msg);
}

pub fn warn(msg: &str) {
    log(Level::Warn, msg);
}

pub fn error(msg: &str) {
    log(Level::Error, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_tags() {
        assert_eq!(Level::Info.tag(), "INFO");
        assert_eq!(Level::Warn.tag(), "WARN");
        assert_eq!(Level::Error.tag(), "ERROR");
    }
}
