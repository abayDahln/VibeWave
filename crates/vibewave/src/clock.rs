//! High-resolution timing helpers.
//!
//! `QueryPerformanceCounter` (QPC) is the clock Windows uses for audio packet
//! timestamps, so latency measurement must use the same clock. We expose it in
//! 100 ns units to match the WASAPI packet timestamps directly.

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// QPC frequency in ticks per second. Panics never happens in practice; if the
/// call fails we fall back to a sane default so callers need not handle it.
pub fn qpc_freq() -> i64 {
    let mut f = 0i64;
    // SAFETY: out-param is a valid, aligned i64.
    if unsafe { QueryPerformanceFrequency(&mut f) }.is_err() || f <= 0 {
        return 10_000_000;
    }
    f
}

/// Current QPC time converted to 100 ns units (matching WASAPI timestamps).
pub fn qpc_now_100ns(freq: i64) -> u64 {
    let mut now = 0i64;
    // SAFETY: out-param is a valid, aligned i64.
    let _ = unsafe { QueryPerformanceCounter(&mut now) };
    let freq = if freq > 0 { freq } else { 10_000_000 };
    (now as i128 * 10_000_000 / freq as i128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_is_monotonic() {
        let f = qpc_freq();
        let a = qpc_now_100ns(f);
        let b = qpc_now_100ns(f);
        assert!(b >= a, "QPC went backwards: {a} -> {b}");
    }

    #[test]
    fn freq_is_positive() {
        assert!(qpc_freq() > 0);
    }
}
