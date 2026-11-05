//! Command-line arguments (see `specs/001-audio-reactive-glow/contracts/cli.md`).
//!
//! `vibewave.exe [--seconds N] [--stats] [--settings] [--autostart]`
//!
//! Unknown arguments are reported as warnings and ignored.

/// Parsed CLI options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cli {
    /// Exit cleanly after this many seconds (for tests/measurement).
    pub seconds: Option<u64>,
    /// Print periodic FPS/latency stats.
    pub stats: bool,
    /// Open the settings window at startup.
    pub settings: bool,
    /// Started by Windows at login (written into the `Run` key). The settings
    /// window never opens automatically in this mode (FR-18).
    pub autostart: bool,
}

/// Parse arguments (excluding the program name). Returns the options plus a
/// list of warnings for anything unrecognised.
pub fn parse(args: &[String]) -> (Cli, Vec<String>) {
    let mut cli = Cli::default();
    let mut warnings = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stats" => cli.stats = true,
            "--settings" => cli.settings = true,
            crate::autostart::AUTOSTART_FLAG => cli.autostart = true,
            "--seconds" => {
                // Always consume the next argument, even if it is invalid, so
                // it is not also reported as an unknown argument.
                match args.get(i + 1) {
                    Some(v) => match v.parse::<u64>() {
                        Ok(n) => cli.seconds = Some(n),
                        Err(_) => {
                            warnings.push(format!("--seconds: '{v}' is not a number, ignored"))
                        }
                    },
                    None => warnings.push("--seconds needs a value, ignored".to_string()),
                }
                i += 1;
            }
            other => warnings.push(format!("unknown argument '{other}', ignored")),
        }
        i += 1;
    }
    (cli, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_is_default() {
        let (cli, w) = parse(&[]);
        assert_eq!(cli, Cli::default());
        assert!(w.is_empty());
        assert!(!cli.stats);
        assert_eq!(cli.seconds, None);
    }

    #[test]
    fn parses_seconds_and_stats() {
        let (cli, w) = parse(&args(&["--seconds", "20", "--stats"]));
        assert_eq!(cli.seconds, Some(20));
        assert!(cli.stats);
        assert!(w.is_empty());
    }

    #[test]
    fn parses_settings_flag() {
        let (cli, w) = parse(&args(&["--settings"]));
        assert!(cli.settings);
        assert!(w.is_empty());
    }

    #[test]
    fn parses_autostart_flag() {
        let (cli, w) = parse(&args(&["--autostart"]));
        assert!(cli.autostart);
        assert!(!cli.settings);
        assert!(w.is_empty());
    }

    #[test]
    fn bad_seconds_warns() {
        let (cli, w) = parse(&args(&["--seconds", "abc"]));
        assert_eq!(cli.seconds, None);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn unknown_argument_warns_and_is_ignored() {
        let (cli, w) = parse(&args(&["--verbose", "--stats"]));
        assert!(cli.stats);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("--verbose"));
    }
}
