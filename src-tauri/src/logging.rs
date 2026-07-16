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

use std::collections::HashSet;
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
///
/// We call [`Builder::targets`] (which *replaces* the target list) rather than
/// [`Builder::target`] (which *appends*). `Builder::new()` is seeded via `Default`
/// with `DEFAULT_LOG_TARGETS == [Stdout, LogDir { file_name: None }]`, and a
/// `LogDir { file_name: None }` resolves to `package_info().name` — for this app
/// that is the product name `gitrx`, i.e. the SAME `gitrx.log` we name explicitly.
/// Appending therefore produced two `Stdout` and two `gitrx.log` writers, so every
/// line was logged twice. Replacing the list yields exactly one writer each.
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let level = level_from_env(std::env::var(LOG_ENV).ok().as_deref());
    Builder::new()
        .level(level)
        .max_file_size(MAX_LOG_FILE_BYTES)
        .rotation_strategy(RotationStrategy::KeepSome(KEEP_LOG_FILES))
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .targets([
            Target::new(TargetKind::Stdout),
            Target::new(TargetKind::LogDir {
                file_name: Some(LOG_FILE_NAME.to_string()),
            }),
        ])
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

/// How many top `WebKit` consumers to name on the watchdog line.
const WEBKIT_TOP_LIMIT: usize = 3;

/// `comm` substrings identifying the out-of-process `WebKit` helpers that host the
/// webview's JS/DOM heaps — the actual runaway last time (~10 GB) while our own
/// RSS sat at ~40 MB. Matched system-wide because on macOS these helpers are XPC
/// services reparented to `launchd` (ppid 1), so a ppid-tree walk from our own
/// PID misses them; a `WebKit` entry that grows while gitrx runs and disappears
/// when it quits is almost certainly ours.
const WEBKIT_MARKERS: [&str; 3] = ["WebKit.WebContent", "WebKit.GPU", "WebKit.Networking"];

/// Parse the stdout of `ps -o rss= -p <pid>` (resident set size in KiB on macOS)
/// into whole mebibytes. Returns `None` on empty/garbage output. Shared by the
/// per-line parser below so the KiB→MiB conversion has a single tested spelling.
fn parse_ps_rss_kib_to_mb(output: &str) -> Option<u64> {
    let kib: u64 = output.trim().parse().ok()?;
    Some(kib / 1024)
}

/// One row of `ps -axo pid=,ppid=,rss=,comm=`, with RSS already reduced to MiB.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcSample {
    pid: u32,
    ppid: u32,
    rss_mb: u64,
    comm: String,
}

/// Parse one `ps -axo pid=,ppid=,rss=,comm=` line. The first three whitespace
/// tokens are `pid`, `ppid`, `rss` (KiB); everything after is the command, which
/// may itself contain spaces, so it is re-joined. Returns `None` for blank or
/// malformed lines.
fn parse_ps_proc_line(line: &str) -> Option<ProcSample> {
    let mut parts = line.split_whitespace();
    let pid: u32 = parts.next()?.parse().ok()?;
    let ppid: u32 = parts.next()?.parse().ok()?;
    let rss_mb = parse_ps_rss_kib_to_mb(parts.next()?)?;
    let comm = parts.collect::<Vec<_>>().join(" ");
    if comm.is_empty() {
        return None;
    }
    Some(ProcSample {
        pid,
        ppid,
        rss_mb,
        comm,
    })
}

/// Parse the full `ps -axo pid=,ppid=,rss=,comm=` table, skipping unparseable
/// lines. Pure so the whole watchdog aggregation is unit-testable without `ps`.
fn parse_ps_processes(output: &str) -> Vec<ProcSample> {
    output.lines().filter_map(parse_ps_proc_line).collect()
}

/// Sum the RSS (MiB) of every descendant of `root_pid` by walking the ppid tree.
///
/// On macOS the `WebKit` helpers usually reparent to `launchd`, so this commonly
/// returns 0 — that is expected, and the reason [`webkit_top_consumers`] scans
/// system-wide as well.
fn sum_descendant_rss_mb(procs: &[ProcSample], root_pid: u32) -> u64 {
    let mut frontier = vec![root_pid];
    // Seed with the root so its own RSS (reported separately as `own_mb`) is never
    // folded back in, even through a pathological ppid cycle.
    let mut seen: HashSet<u32> = HashSet::from([root_pid]);
    let mut total = 0;
    while let Some(parent) = frontier.pop() {
        for proc in procs.iter().filter(|p| p.ppid == parent) {
            if seen.insert(proc.pid) {
                total += proc.rss_mb;
                frontier.push(proc.pid);
            }
        }
    }
    total
}

/// The `limit` largest `WebKit` helper processes (by RSS), as `(pid, rss_mb)`,
/// sorted descending by RSS with pid as a stable tie-break. Intentionally
/// unattributed: other apps' `WebKit` helpers appear too (see [`WEBKIT_MARKERS`]).
fn webkit_top_consumers(procs: &[ProcSample], limit: usize) -> Vec<(u32, u64)> {
    let mut hits: Vec<(u32, u64)> = procs
        .iter()
        .filter(|p| WEBKIT_MARKERS.iter().any(|m| p.comm.contains(m)))
        .map(|p| (p.pid, p.rss_mb))
        .collect();
    hits.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hits.truncate(limit);
    hits
}

/// The aggregated watchdog reading for one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchdogSample {
    /// Our own process RSS (MiB) — the historic `rss_mb=` anchor.
    own_mb: u64,
    /// Summed RSS (MiB) of processes we actually parent (often 0 on macOS).
    child_mb: u64,
    /// Top `WebKit` helpers system-wide, `(pid, rss_mb)`, descending.
    webkit_top: Vec<(u32, u64)>,
}

/// Reduce a parsed `ps` table to the tick's [`WatchdogSample`] for `own_pid`.
fn build_watchdog_sample(procs: &[ProcSample], own_pid: u32) -> WatchdogSample {
    let own_mb = procs
        .iter()
        .find(|p| p.pid == own_pid)
        .map_or(0, |p| p.rss_mb);
    WatchdogSample {
        own_mb,
        child_mb: sum_descendant_rss_mb(procs, own_pid),
        webkit_top: webkit_top_consumers(procs, WEBKIT_TOP_LIMIT),
    }
}

/// The largest single reading across own RSS, summed descendants, and the biggest
/// `WebKit` helper — this drives severity so a webview runaway (invisible in own
/// RSS) still escalates the line to warn/error.
fn watchdog_peak_mb(sample: &WatchdogSample) -> u64 {
    let webkit_max = sample.webkit_top.first().map_or(0, |&(_, mb)| mb);
    sample.own_mb.max(sample.child_mb).max(webkit_max)
}

/// Render the `WebKit` top list as `[pid:mb, pid:mb]` (empty list → `[]`).
fn format_webkit_top(top: &[(u32, u64)]) -> String {
    let inner = top
        .iter()
        .map(|(pid, mb)| format!("{pid}:{mb}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// Format the single periodic watchdog line. `rss_mb=` stays the leading key so
/// the grep playbook anchored on it keeps working.
fn format_watchdog_line(sample: &WatchdogSample) -> String {
    format!(
        "rss_mb={} child_rss_mb={} webkit_top={}",
        sample.own_mb,
        sample.child_mb,
        format_webkit_top(&sample.webkit_top),
    )
}

/// Emit one watchdog reading at the severity its peak warrants. Split from the
/// sampling so the threshold logic is unit-testable without spawning `ps`.
fn log_watchdog_sample(sample: &WatchdogSample) {
    let line = format_watchdog_line(sample);
    let peak = watchdog_peak_mb(sample);
    if peak >= RSS_ERROR_MB {
        log::error!(target: T_WATCHDOG, "{line} — runaway (>= {RSS_ERROR_MB} MB)");
    } else if peak >= RSS_WARN_MB {
        log::warn!(target: T_WATCHDOG, "{line} — elevated (>= {RSS_WARN_MB} MB)");
    } else {
        log::info!(target: T_WATCHDOG, "{line}");
    }
}

/// Sample every process once via `ps -axo pid=,ppid=,rss=,comm=`.
///
/// One `ps` per tick (at a 60 s cadence the fork/exec cost is irrelevant) feeds
/// own RSS, the descendant sum, and the `WebKit` scan — no separate per-PID call.
/// `ps -o rss=` is *current* resident size, better for spotting growth than
/// `getrusage`'s peak-only `ru_maxrss`.
#[cfg(unix)]
fn sample_processes() -> Option<Vec<ProcSample>> {
    let output = std::process::Command::new("ps")
        .arg("-axo")
        .arg("pid=,ppid=,rss=,comm=")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_ps_processes(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(not(unix))]
fn sample_processes() -> Option<Vec<ProcSample>> {
    None
}

/// Spawn the background memory watchdog on the Tauri async runtime.
///
/// Every [`WATCHDOG_INTERVAL`] it samples all processes and logs one line
/// (info, escalating to warn/error once the peak of own/descendant/`WebKit` RSS
/// crosses the thresholds). This periodic line is the anchor everything else
/// correlates against: find the sample where `rss_mb`/`webkit_top` jumps, then
/// read the surrounding watch/command/window lines to see what drove it.
pub fn spawn_memory_watchdog() {
    tauri::async_runtime::spawn(async {
        let mut ticker = tokio::time::interval(WATCHDOG_INTERVAL);
        loop {
            ticker.tick().await;
            match sample_processes() {
                Some(procs) => {
                    log_watchdog_sample(&build_watchdog_sample(&procs, std::process::id()));
                }
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

    fn proc(pid: u32, ppid: u32, rss_mb: u64, comm: &str) -> ProcSample {
        ProcSample {
            pid,
            ppid,
            rss_mb,
            comm: comm.to_string(),
        }
    }

    #[test]
    fn parse_ps_proc_line_splits_fields_and_rejoins_command() {
        // Right-aligned rss, a command path containing spaces.
        let line = "  501    1   65536 /Applications/My App.app/Contents/MacOS/App";
        assert_eq!(
            parse_ps_proc_line(line),
            Some(proc(
                501,
                1,
                64,
                "/Applications/My App.app/Contents/MacOS/App"
            ))
        );
    }

    #[test]
    fn parse_ps_proc_line_rejects_malformed_rows() {
        assert_eq!(parse_ps_proc_line(""), None);
        assert_eq!(parse_ps_proc_line("   "), None);
        // Missing the command column.
        assert_eq!(parse_ps_proc_line("501 1 65536"), None);
        // Non-numeric pid.
        assert_eq!(parse_ps_proc_line("x 1 65536 comm"), None);
    }

    #[test]
    fn parse_ps_processes_skips_unparseable_lines() {
        let output = "\
501 1 2048 launchd
not a real row
777 501 4096 /path/to/child
";
        let procs = parse_ps_processes(output);
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0], proc(501, 1, 2, "launchd"));
        assert_eq!(procs[1], proc(777, 501, 4, "/path/to/child"));
    }

    #[test]
    fn descendants_sum_walks_the_ppid_tree() {
        // 100 -> {200, 300}; 200 -> 400; 999 is unrelated.
        let procs = vec![
            proc(100, 1, 40, "gitrx"),
            proc(200, 100, 10, "helper"),
            proc(300, 100, 20, "helper"),
            proc(400, 200, 30, "grandchild"),
            proc(999, 1, 500, "unrelated"),
        ];
        // Own RSS is excluded; 10 + 20 + 30 = 60.
        assert_eq!(sum_descendant_rss_mb(&procs, 100), 60);
        // A leaf has no descendants.
        assert_eq!(sum_descendant_rss_mb(&procs, 400), 0);
    }

    #[test]
    fn descendants_sum_survives_a_ppid_cycle() {
        // Pathological self/mutual parenting must not loop forever.
        let procs = vec![proc(10, 20, 5, "a"), proc(20, 10, 7, "b")];
        // From 10: 20 is a child (+7), and 20's child 10 is already seen.
        assert_eq!(sum_descendant_rss_mb(&procs, 10), 7);
    }

    #[test]
    fn webkit_top_picks_largest_helpers_capped() {
        let procs = vec![
            proc(1, 1, 10, "com.apple.WebKit.WebContent"),
            proc(2, 1, 900, "com.apple.WebKit.GPU"),
            proc(3, 1, 500, "com.apple.WebKit.Networking"),
            proc(4, 1, 700, "com.apple.WebKit.WebContent (Prewarmed)"),
            proc(5, 1, 9999, "some.other.process"),
        ];
        // Descending by rss, capped at the limit; the non-WebKit giant is excluded.
        assert_eq!(
            webkit_top_consumers(&procs, WEBKIT_TOP_LIMIT),
            vec![(2, 900), (4, 700), (3, 500)]
        );
    }

    #[test]
    fn webkit_top_ties_break_on_pid() {
        let procs = vec![
            proc(30, 1, 100, "WebKit.WebContent"),
            proc(10, 1, 100, "WebKit.WebContent"),
            proc(20, 1, 100, "WebKit.WebContent"),
        ];
        assert_eq!(
            webkit_top_consumers(&procs, 3),
            vec![(10, 100), (20, 100), (30, 100)]
        );
    }

    #[test]
    fn build_sample_reads_own_rss_and_aggregates() {
        let procs = vec![
            proc(100, 1, 40, "gitrx"),
            proc(200, 100, 15, "helper"),
            proc(300, 1, 812, "com.apple.WebKit.WebContent"),
        ];
        let sample = build_watchdog_sample(&procs, 100);
        assert_eq!(sample.own_mb, 40);
        assert_eq!(sample.child_mb, 15);
        assert_eq!(sample.webkit_top, vec![(300, 812)]);
    }

    #[test]
    fn build_sample_defaults_own_rss_to_zero_when_absent() {
        let procs = vec![proc(200, 1, 15, "helper")];
        let sample = build_watchdog_sample(&procs, 100);
        assert_eq!(sample.own_mb, 0);
        assert_eq!(sample.child_mb, 0);
        assert!(sample.webkit_top.is_empty());
    }

    #[test]
    fn peak_is_the_max_of_all_three_signals() {
        // WebKit runaway dominates while own RSS stays tiny — the real scenario.
        let sample = WatchdogSample {
            own_mb: 40,
            child_mb: 0,
            webkit_top: vec![(300, 9000), (301, 20)],
        };
        assert_eq!(watchdog_peak_mb(&sample), 9000);

        // Descendant sum can dominate too.
        let sample = WatchdogSample {
            own_mb: 40,
            child_mb: 5000,
            webkit_top: vec![],
        };
        assert_eq!(watchdog_peak_mb(&sample), 5000);
    }

    #[test]
    fn peak_crosses_thresholds_for_a_webkit_runaway() {
        let sample = WatchdogSample {
            own_mb: 40,
            child_mb: 0,
            webkit_top: vec![(300, RSS_ERROR_MB + 1)],
        };
        assert!(watchdog_peak_mb(&sample) >= RSS_ERROR_MB);
    }

    #[test]
    fn format_webkit_top_renders_pid_colon_mb() {
        assert_eq!(format_webkit_top(&[]), "[]");
        assert_eq!(
            format_webkit_top(&[(1234, 812), (5678, 210)]),
            "[1234:812, 5678:210]"
        );
    }

    #[test]
    fn format_line_keeps_rss_mb_as_leading_key() {
        let sample = WatchdogSample {
            own_mb: 41,
            child_mb: 0,
            webkit_top: vec![(1234, 812), (5678, 210)],
        };
        assert_eq!(
            format_watchdog_line(&sample),
            "rss_mb=41 child_rss_mb=0 webkit_top=[1234:812, 5678:210]"
        );
    }

    #[test]
    fn timer_reports_nonpanicking_elapsed() {
        let t = Timer::start();
        // Just exercising the API: elapsed is always representable and monotone.
        let _ms: u128 = t.ms();
    }
}
