//! Production diagnostic logging: the `tauri-plugin-log` configuration, a
//! process-memory watchdog, and a tiny command-timing helper.
//!
//! Motivation: the app has twice run away on memory (82 GB once, ~10 GB later)
//! and had to be killed. The existing guards (watcher ignore-filter, 8 MiB diff
//! cap, 20 MB image cap, frontend refresh coalescing) did not stop it, so this
//! layer exists to reconstruct *what the app was doing* as memory grew — event
//! storms, refresh loops, oversized payloads, window/watcher churn — anchored by
//! a periodic RSS sample.
//!
//! Log file location (macOS): `~/Library/Logs/com.travisberry.rust-gitx/gitrx.log`
//! (the app's `LogDir`, keyed by the bundle identifier). Frontend logs go through
//! the JS plugin API into the SAME file. Raise verbosity by launching with
//! `RUST_LOG=debug` (accepts `off|error|warn|info|debug|trace`).

use std::time::{Duration, Instant};

use log::LevelFilter;
use tauri::Runtime;
use tauri_plugin_log::{Builder, RotationStrategy, Target, TargetKind, TimezoneStrategy};

/// Base name of the on-disk log file (the plugin appends `.log`).
const LOG_FILE_NAME: &str = "gitrx";
/// Rotate once a file passes ~5 MB.
const MAX_LOG_FILE_BYTES: u128 = 5 * 1024 * 1024;
/// Keep a handful of rotated files — enough to look back across a runaway, but
/// bounded so logging can never itself become a disk-blowup.
const KEEP_LOG_FILES: usize = 5;

/// Environment variable honored to override the default log level at launch.
const LOG_ENV: &str = "RUST_LOG";

/// Log target namespaces, so `RUST_LOG` / `level_for` filtering and log-grepping
/// have stable prefixes.
pub const T_CMD: &str = "gitrx::cmd";
pub const T_WATCH: &str = "gitrx::watch";
pub const T_WINDOW: &str = "gitrx::window";
pub const T_WATCHDOG: &str = "gitrx::watchdog";

/// Parse a human log-level string into a [`LevelFilter`]. Case-insensitive;
/// unrecognized input yields `None` so the caller can fall back to the default.
fn parse_level(value: &str) -> Option<LevelFilter> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// Resolve the effective level from an optional env value, defaulting to `Info`.
///
/// Pure (takes the value rather than reading the environment) so it is testable
/// without mutating process-global state.
fn level_from_env(value: Option<&str>) -> LevelFilter {
    value.and_then(parse_level).unwrap_or(LevelFilter::Info)
}

/// Build the configured `tauri-plugin-log` plugin.
///
/// Targets: the app `LogDir` (persisted, rotated) plus `Stdout` (dev console).
/// Level: `Info` by default, overridable via `RUST_LOG`. The plugin's default
/// format already prefixes each line with a timestamp, the target, and the
/// level; [`TimezoneStrategy::UseLocal`] makes those timestamps local time.
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let level = level_from_env(std::env::var(LOG_ENV).ok().as_deref());
    Builder::new()
        .level(level)
        .max_file_size(MAX_LOG_FILE_BYTES)
        .rotation_strategy(RotationStrategy::KeepSome(KEEP_LOG_FILES))
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .target(Target::new(TargetKind::Stdout))
        .target(Target::new(TargetKind::LogDir {
            file_name: Some(LOG_FILE_NAME.to_string()),
        }))
        .build()
}

// ---------------------------------------------------------------------------
// Memory watchdog
// ---------------------------------------------------------------------------

/// How often the watchdog samples resident memory.
const WATCHDOG_INTERVAL: Duration = Duration::from_mins(1);
/// Above this RSS the sample is logged at `warn` (something is growing).
const RSS_WARN_MB: u64 = 1536; // 1.5 GB
/// Above this RSS the sample is logged at `error` (runaway territory).
const RSS_ERROR_MB: u64 = 4096; // 4 GB

/// Parse the stdout of `ps -o rss= -p <pid>` (resident set size in KiB on macOS)
/// into whole mebibytes. Returns `None` on empty/garbage output.
fn parse_ps_rss_kib_to_mb(output: &str) -> Option<u64> {
    let kib: u64 = output.trim().parse().ok()?;
    Some(kib / 1024)
}

/// Sample this process's resident set size in MB, or `None` if unavailable.
///
/// Uses a one-shot `ps` rather than a new crate dependency: at a 60 s cadence the
/// fork/exec cost is irrelevant, and `ps -o rss=` (RSS in KiB on macOS/Linux) is
/// the *current* resident size — more useful for spotting growth than
/// `getrusage`'s peak-only `ru_maxrss`.
#[cfg(unix)]
fn sample_rss_mb() -> Option<u64> {
    let pid = std::process::id();
    let output = std::process::Command::new("ps")
        .arg("-o")
        .arg("rss=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ps_rss_kib_to_mb(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(unix))]
fn sample_rss_mb() -> Option<u64> {
    None
}

/// Emit one RSS sample at the severity its size warrants. Split out so the
/// threshold logic is unit-testable without spawning `ps` or a runtime.
fn log_rss_sample(mb: u64) {
    if mb >= RSS_ERROR_MB {
        log::error!(target: T_WATCHDOG, "rss_mb={mb} — runaway (>= {RSS_ERROR_MB} MB)");
    } else if mb >= RSS_WARN_MB {
        log::warn!(target: T_WATCHDOG, "rss_mb={mb} — elevated (>= {RSS_WARN_MB} MB)");
    } else {
        log::info!(target: T_WATCHDOG, "rss_mb={mb}");
    }
}

/// Spawn the background memory watchdog on the Tauri async runtime.
///
/// Every [`WATCHDOG_INTERVAL`] it samples RSS and logs it (info, escalating to
/// warn/error past the thresholds). This periodic line is the anchor everything
/// else correlates against: find the sample where `rss_mb` jumps, then read the
/// surrounding watch/command/window lines to see what drove it.
pub fn spawn_memory_watchdog() {
    tauri::async_runtime::spawn(async {
        let mut ticker = tokio::time::interval(WATCHDOG_INTERVAL);
        loop {
            ticker.tick().await;
            match sample_rss_mb() {
                Some(mb) => log_rss_sample(mb),
                None => log::debug!(target: T_WATCHDOG, "rss sample unavailable"),
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Command timing
// ---------------------------------------------------------------------------

/// A minimal stopwatch for command instrumentation: `Timer::start()` at entry,
/// `.ms()` when logging the outcome. Exists so the command layer has one shared
/// spelling of the `Instant::now()` boilerplate instead of a dozen copies.
#[derive(Debug, Clone, Copy)]
pub struct Timer {
    start: Instant,
}

impl Timer {
    /// Start timing now.
    #[must_use]
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Milliseconds elapsed since [`Timer::start`].
    #[must_use]
    pub fn ms(self) -> u128 {
        self.start.elapsed().as_millis()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_accepts_known_levels_case_insensitively() {
        assert_eq!(parse_level("info"), Some(LevelFilter::Info));
        assert_eq!(parse_level("DEBUG"), Some(LevelFilter::Debug));
        assert_eq!(parse_level("  Warn "), Some(LevelFilter::Warn));
        assert_eq!(parse_level("warning"), Some(LevelFilter::Warn));
        assert_eq!(parse_level("off"), Some(LevelFilter::Off));
        assert_eq!(parse_level("trace"), Some(LevelFilter::Trace));
        assert_eq!(parse_level("error"), Some(LevelFilter::Error));
    }

    #[test]
    fn parse_level_rejects_garbage() {
        assert_eq!(parse_level(""), None);
        assert_eq!(parse_level("verbose"), None);
        assert_eq!(parse_level("5"), None);
    }

    #[test]
    fn level_from_env_defaults_to_info() {
        assert_eq!(level_from_env(None), LevelFilter::Info);
        assert_eq!(level_from_env(Some("nonsense")), LevelFilter::Info);
    }

    #[test]
    fn level_from_env_honors_a_valid_override() {
        assert_eq!(level_from_env(Some("debug")), LevelFilter::Debug);
        assert_eq!(level_from_env(Some("ERROR")), LevelFilter::Error);
    }

    #[test]
    fn parse_ps_rss_converts_kib_to_mb() {
        // ps reports KiB; 2048 KiB == 2 MiB.
        assert_eq!(parse_ps_rss_kib_to_mb("2048"), Some(2));
        // Trailing newline (ps output) is trimmed.
        assert_eq!(parse_ps_rss_kib_to_mb("1048576\n"), Some(1024));
        // Sub-MiB rounds down to 0.
        assert_eq!(parse_ps_rss_kib_to_mb("512"), Some(0));
    }

    #[test]
    fn parse_ps_rss_rejects_non_numeric() {
        assert_eq!(parse_ps_rss_kib_to_mb(""), None);
        assert_eq!(parse_ps_rss_kib_to_mb("   "), None);
        assert_eq!(parse_ps_rss_kib_to_mb("abc"), None);
    }

    #[test]
    fn timer_reports_nonpanicking_elapsed() {
        let t = Timer::start();
        // Just exercising the API: elapsed is always representable and monotone.
        let _ms: u128 = t.ms();
    }
}
