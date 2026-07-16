//! Filesystem watching + debounced auto-refresh.
//!
//! We watch the repository working tree recursively via `notify` (`FSEvents` on
//! macOS). Raw filesystem events are noisy — a single `git status`/`git add`
//! touches the index, a `.lock` file, loose objects, reflogs, and more — so we
//! run every event path through [`classify_paths`] to keep only the changes that
//! actually alter what the staging screen renders, then debounce a burst down to
//! a single `repo-changed` event for the frontend.
//!
//! Lifecycle: the [`notify::RecommendedWatcher`] handle is parked in the owning
//! window's [`WindowRepo`](crate::state::WindowRepo) so it stays alive; dropping
//! that entry (the window closed, or a new repo opened in it) drops the watcher,
//! which in turn drops the event sender and lets the debouncer thread terminate
//! on channel disconnect.
//!
//! Events are window-targeted: each watcher emits `repo-changed` (and
//! `watch-error`) only to the label of the window whose repo it watches, so a
//! change in one repo refreshes only that repo's window.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use git2::Repository;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::git::open_repository;
use crate::logging::T_WATCH;

/// Trailing-edge quiet period: once no new event has arrived for this long, the
/// accumulated burst is flushed as one `repo-changed` event.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// A burst whose raw filesystem-event count meets this is logged as a storm —
/// the signature of a runaway (a rebuild, a huge checkout, an editor thrash).
const STORM_THRESHOLD: usize = 1000;
/// A burst that keeps more than this many relevant paths is logged at `info`
/// (a meaningful change); quieter bursts stay at `debug`.
const KEPT_INFO_THRESHOLD: usize = 100;
/// How many raw paths to capture as a storm sample (for the warn line).
const SAMPLE_LIMIT: usize = 5;

/// Why a `repo-changed` event fired, in strict priority order `Index > Head > Fs`.
///
/// When a debounced burst touches several categories, the highest-priority one
/// wins (an index write during a working-tree churn still reports `Index`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// `.git/index` changed — staging/unstaging altered the index.
    Index,
    /// `HEAD`, `MERGE_HEAD`, or a ref under `.git/refs/` changed.
    Head,
    /// A working-tree file (outside `.git`) changed.
    Fs,
}

impl Reason {
    /// The camelCase-free wire string sent to the frontend payload.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Head => "head",
            Self::Fs => "fs",
        }
    }

    /// Priority rank; higher wins when a burst mixes categories.
    const fn rank(self) -> u8 {
        match self {
            Self::Index => 2,
            Self::Head => 1,
            Self::Fs => 0,
        }
    }

    /// Return whichever of `self`/`other` has the higher priority.
    const fn max(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

/// Payload for the `repo-changed` Tauri event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoChanged {
    reason: &'static str,
}

/// Classify a single event path relative to the repo's `.git` directory.
///
/// Returns `None` for paths we deliberately ignore (loose objects, logs,
/// `FETCH_HEAD` churn, and any `*.lock` — `index.lock` above all).
fn classify_one(git_dir: &Path, path: &Path) -> Option<Reason> {
    // Drop lock-file churn wherever it lives (`index.lock`, `refs/**/*.lock`, ...).
    if path.extension().is_some_and(|ext| ext == "lock") {
        return None;
    }

    path.strip_prefix(git_dir).map_or(
        // Outside `.git`: a working-tree change is always relevant (gitignored
        // churn is harmless — the refresh it triggers is cheap and debounced).
        Some(Reason::Fs),
        // Inside `.git`: keep only a curated allow-list, drop everything else.
        |rel| {
            if rel == Path::new("index") {
                Some(Reason::Index)
            } else if rel == Path::new("HEAD")
                || rel == Path::new("MERGE_HEAD")
                || rel.starts_with("refs")
            {
                Some(Reason::Head)
            } else {
                None
            }
        },
    )
}

/// Pure classification of a batch of event paths into an optional [`Reason`].
///
/// `None` means "nothing worth refreshing" (e.g. only objects/locks changed).
/// Otherwise the highest-priority reason across all kept paths is returned.
pub fn classify_paths(git_dir: &Path, paths: &[PathBuf]) -> Option<Reason> {
    let mut best: Option<Reason> = None;
    for path in paths {
        if let Some(reason) = classify_one(git_dir, path) {
            best = Some(best.map_or(reason, |cur| cur.max(reason)));
            // `Index` is the ceiling; no later path can outrank it.
            if best == Some(Reason::Index) {
                break;
            }
        }
    }
    best
}

/// Classify a burst of event paths, first dropping any working-tree path that
/// git ignores (e.g. `src-tauri/target/**` churn during a build, `*.log`).
///
/// `.git`-internal paths bypass the ignore check entirely: `is_path_ignored`
/// reasons about working-tree ignore rules and would only confuse them. They are
/// instead kept iff they are a real refresh trigger (see [`is_kept`]); workdir
/// paths are dropped only when `is_ignored` returns `true`.
///
/// This is the testable seam — the watcher tests drive it with a closure built
/// from a real repo's `.gitignore`, no `FSEvents` required. [`classify_paths`]
/// itself stays pure.
fn classify_paths_filtered(
    git_dir: &Path,
    is_ignored: &impl Fn(&Path) -> bool,
    paths: &[PathBuf],
) -> Option<Reason> {
    let kept: Vec<PathBuf> = paths
        .iter()
        .filter(|p| is_kept(git_dir, is_ignored, p))
        .cloned()
        .collect();
    classify_paths(git_dir, &kept)
}

/// Whether a raw event path actually drives a refresh — the single predicate
/// shared by [`classify_paths_filtered`] (which paths to classify) and the
/// burst-stats `kept` counter (how many to report), so the logged count can
/// never drift from what refreshes.
///
/// A workdir path git ignores is dropped outright. Every other path is kept iff
/// [`classify_one`] yields `Some`: this means a `.git`-internal path counts only
/// when it is a real trigger (`index`/`HEAD`/`MERGE_HEAD`/`refs`, non-`.lock`) —
/// pack-object, loose-object, log, and `FETCH_HEAD` churn no longer inflate the
/// `kept` stat during a storm even though they are `.git`-internal.
fn is_kept(git_dir: &Path, is_ignored: &impl Fn(&Path) -> bool, path: &Path) -> bool {
    if !path.starts_with(git_dir) && is_ignored(path) {
        return false;
    }
    classify_one(git_dir, path).is_some()
}

/// Per-burst diagnostics accumulated across a debounced window, logged when the
/// burst flushes. `raw_events` is every filesystem path seen (the storm metric);
/// `kept` is how many survived the ignore filter (the refresh-relevance metric).
#[derive(Debug, Default)]
struct BurstStats {
    raw_events: usize,
    kept: usize,
    samples: Vec<PathBuf>,
}

impl BurstStats {
    /// Fold one raw event path into the tally, capturing up to [`SAMPLE_LIMIT`]
    /// example paths for the storm warn line.
    fn record_raw(&mut self, path: &Path) {
        self.raw_events += 1;
        if self.samples.len() < SAMPLE_LIMIT {
            self.samples.push(path.to_path_buf());
        }
    }
}

/// True when `path` (an absolute event path) is ignored by git's ignore rules.
///
/// libgit2 evaluates ignore rules relative to the working tree, so the path is
/// made repo-relative first. A path outside the workdir, a missing workdir, or a
/// libgit2 error all resolve to "not ignored" — we would rather over-refresh
/// than silently swallow a real change (availability over silence). The path not
/// existing on disk is fine: `is_path_ignored` answers purely from the rules.
fn path_is_ignored(repo: &Repository, path: &Path) -> bool {
    let Some(workdir) = repo.workdir() else {
        return false;
    };
    let rel = path.strip_prefix(workdir).unwrap_or(path);
    repo.is_path_ignored(rel).unwrap_or(false)
}

/// Build a watcher for `repo_workdir` whose debounced `repo-changed` events are
/// targeted at the window `label`, returning the live handle for the caller to
/// park in that window's [`WindowRepo`](crate::state::WindowRepo).
///
/// Best-effort: on failure the caller emits a `watch-error` to `label` and leaves
/// the window fully usable (just without live refresh) rather than aborting the
/// open. Spawns the debouncer thread that lives until the returned watcher is
/// dropped.
pub fn build_watcher(
    app: &AppHandle,
    label: &str,
    repo_workdir: &Path,
) -> notify::Result<RecommendedWatcher> {
    // Derive the `.git` directory for classification. `repo.path()` is the git
    // dir (handles worktrees/`.git` files); fall back to the obvious location if
    // discovery fails so we still watch, just with a best-guess filter.
    let git_dir = open_repository(repo_workdir).map_or_else(
        |_| repo_workdir.join(".git"),
        |repo| repo.path().to_path_buf(),
    );

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    // The notify callback runs on notify's own thread. Never panic there — just
    // forward every result (Ok and Err) to the debouncer for handling.
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(repo_workdir, RecursiveMode::Recursive)?;
    log::info!(target: T_WATCH, "watcher built: label={label} workdir={}", repo_workdir.display());

    let app = app.clone();
    let label = label.to_string();
    let repo_workdir = repo_workdir.to_path_buf();
    std::thread::spawn(move || debounce_loop(&app, &label, &git_dir, &repo_workdir, &rx));

    Ok(watcher)
}

/// Trailing-edge debouncer: block for the first event of a burst, coalesce until
/// `DEBOUNCE` of quiet, then emit at most one `repo-changed`. Exits when the
/// channel disconnects (watcher dropped).
fn debounce_loop(
    app: &AppHandle,
    label: &str,
    git_dir: &Path,
    repo_workdir: &Path,
    rx: &mpsc::Receiver<notify::Result<Event>>,
) {
    loop {
        // Wait (indefinitely) for the first event of the next burst.
        let Ok(first) = rx.recv() else {
            log::info!(target: T_WATCH, "watcher stopped (dropped): label={label}");
            return; // watcher dropped — we're done
        };

        // Open the repo once per burst to answer ignore queries (cheap; matches
        // the codebase's reopen-per-call convention). If the open fails we fall
        // back to "nothing ignored" so a real change is never dropped.
        let repo = open_repository(repo_workdir).ok();
        let is_ignored = |path: &Path| {
            repo.as_ref()
                .is_some_and(|repo| path_is_ignored(repo, path))
        };

        let mut pending: Option<Reason> = None;
        let mut stats = BurstStats::default();
        accumulate(
            app,
            label,
            git_dir,
            &is_ignored,
            first,
            &mut pending,
            &mut stats,
        );

        // Coalesce follow-on events until the quiet window elapses.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => accumulate(
                    app,
                    label,
                    git_dir,
                    &is_ignored,
                    ev,
                    &mut pending,
                    &mut stats,
                ),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    flush_burst(app, label, pending, &stats);
                    log::info!(target: T_WATCH, "watcher stopped (dropped): label={label}");
                    return;
                }
            }
        }

        flush_burst(app, label, pending, &stats);
    }
}

/// Fold one event's classification into the pending burst reason. Watcher errors
/// are surfaced as `watch-error` and otherwise ignored (keep going).
fn accumulate(
    app: &AppHandle,
    label: &str,
    git_dir: &Path,
    is_ignored: &impl Fn(&Path) -> bool,
    ev: notify::Result<Event>,
    pending: &mut Option<Reason>,
    stats: &mut BurstStats,
) {
    match ev {
        Ok(event) => {
            for path in &event.paths {
                stats.record_raw(path);
                if is_kept(git_dir, is_ignored, path) {
                    stats.kept += 1;
                }
            }
            if let Some(reason) = classify_paths_filtered(git_dir, is_ignored, &event.paths) {
                *pending = Some(pending.map_or(reason, |cur| cur.max(reason)));
            }
        }
        Err(e) => {
            log::warn!(target: T_WATCH, "watch error: label={label}: {e}");
            let _ = app.emit_to(label, "watch-error", format!("File watch error: {e}"));
        }
    }
}

/// Emit a single `repo-changed` to the owning window if the burst kept anything
/// worth refreshing, and log the burst's diagnostics.
///
/// A burst of [`STORM_THRESHOLD`]+ raw events is warned about with sample paths
/// even when nothing was kept — that pattern (thousands of ignored-path events)
/// is itself the runaway signature we want to spot in the log.
fn flush_burst(app: &AppHandle, label: &str, pending: Option<Reason>, stats: &BurstStats) {
    if stats.raw_events >= STORM_THRESHOLD {
        log::warn!(
            target: T_WATCH,
            "event storm: label={label} raw_events={} kept={} samples={:?}",
            stats.raw_events, stats.kept, stats.samples
        );
    }
    if let Some(reason) = pending {
        let _ = app.emit_to(
            label,
            "repo-changed",
            RepoChanged {
                reason: reason.as_str(),
            },
        );
        if stats.kept > KEPT_INFO_THRESHOLD {
            log::info!(
                target: T_WATCH,
                "repo-changed: label={label} reason={} raw_events={} kept={}",
                reason.as_str(), stats.raw_events, stats.kept
            );
        } else {
            log::debug!(
                target: T_WATCH,
                "repo-changed: label={label} reason={} raw_events={} kept={}",
                reason.as_str(), stats.raw_events, stats.kept
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative `.git` dir for the pure-filter tests.
    fn git_dir() -> PathBuf {
        PathBuf::from("/repo/.git")
    }

    #[test]
    fn index_write_is_index() {
        let paths = vec![PathBuf::from("/repo/.git/index")];
        assert_eq!(classify_paths(&git_dir(), &paths), Some(Reason::Index));
    }

    #[test]
    fn head_change_is_head() {
        let paths = vec![PathBuf::from("/repo/.git/HEAD")];
        assert_eq!(classify_paths(&git_dir(), &paths), Some(Reason::Head));
    }

    #[test]
    fn merge_head_is_head() {
        let paths = vec![PathBuf::from("/repo/.git/MERGE_HEAD")];
        assert_eq!(classify_paths(&git_dir(), &paths), Some(Reason::Head));
    }

    #[test]
    fn ref_change_is_head() {
        let paths = vec![PathBuf::from("/repo/.git/refs/heads/main")];
        assert_eq!(classify_paths(&git_dir(), &paths), Some(Reason::Head));
    }

    #[test]
    fn workdir_file_is_fs() {
        let paths = vec![PathBuf::from("/repo/src/main.rs")];
        assert_eq!(classify_paths(&git_dir(), &paths), Some(Reason::Fs));
    }

    #[test]
    fn objects_are_ignored() {
        let paths = vec![PathBuf::from("/repo/.git/objects/ab/cdef0123")];
        assert_eq!(classify_paths(&git_dir(), &paths), None);
    }

    #[test]
    fn logs_and_fetch_head_are_ignored() {
        let paths = vec![
            PathBuf::from("/repo/.git/logs/HEAD"),
            PathBuf::from("/repo/.git/FETCH_HEAD"),
        ];
        assert_eq!(classify_paths(&git_dir(), &paths), None);
    }

    #[test]
    fn index_lock_is_ignored() {
        let paths = vec![PathBuf::from("/repo/.git/index.lock")];
        assert_eq!(classify_paths(&git_dir(), &paths), None);
    }

    #[test]
    fn ref_lock_is_ignored() {
        // A `refs/**/*.lock` must be dropped even though `refs/` is otherwise kept.
        let paths = vec![PathBuf::from("/repo/.git/refs/heads/main.lock")];
        assert_eq!(classify_paths(&git_dir(), &paths), None);
    }

    #[test]
    fn mixed_index_and_workdir_prioritises_index() {
        let paths = vec![
            PathBuf::from("/repo/src/main.rs"),
            PathBuf::from("/repo/.git/index"),
        ];
        assert_eq!(classify_paths(&git_dir(), &paths), Some(Reason::Index));
    }

    #[test]
    fn mixed_head_and_workdir_prioritises_head() {
        let paths = vec![
            PathBuf::from("/repo/README.md"),
            PathBuf::from("/repo/.git/refs/heads/main"),
        ];
        assert_eq!(classify_paths(&git_dir(), &paths), Some(Reason::Head));
    }

    #[test]
    fn empty_batch_is_none() {
        assert_eq!(classify_paths(&git_dir(), &[]), None);
    }

    // ---- Ignore-filter seam (pure, no repo) ----

    #[test]
    fn git_internal_paths_bypass_the_ignore_filter() {
        // Even an "ignore everything" predicate must not drop a `.git/index`
        // write — internal paths keep their curated handling.
        let always = |_: &Path| true;
        let paths = vec![PathBuf::from("/repo/.git/index")];
        assert_eq!(
            classify_paths_filtered(&git_dir(), &always, &paths),
            Some(Reason::Index)
        );
    }

    #[test]
    fn ignored_workdir_paths_are_dropped_by_the_seam() {
        // A workdir path the predicate calls ignored yields nothing to refresh.
        let always = |_: &Path| true;
        let paths = vec![PathBuf::from("/repo/src/main.rs")];
        assert_eq!(classify_paths_filtered(&git_dir(), &always, &paths), None);
    }

    #[test]
    fn seam_keeps_the_real_path_when_only_some_are_ignored() {
        // Ignore only `*.o`; the real source edit must still win as `Fs`.
        let is_ignored = |p: &Path| p.extension().is_some_and(|e| e == "o");
        let paths = vec![
            PathBuf::from("/repo/target/debug/foo.o"),
            PathBuf::from("/repo/src/main.rs"),
        ];
        assert_eq!(
            classify_paths_filtered(&git_dir(), &is_ignored, &paths),
            Some(Reason::Fs)
        );
    }

    // ---- `is_kept` / burst `kept` stat (the storm-count fix) ----

    /// Count paths the shared predicate keeps — mirrors the per-event tally in
    /// [`accumulate`], so it exercises exactly what the `kept=` stat reports.
    fn kept_count(is_ignored: &impl Fn(&Path) -> bool, paths: &[PathBuf]) -> usize {
        paths
            .iter()
            .filter(|p| is_kept(&git_dir(), is_ignored, p))
            .count()
    }

    #[test]
    fn pack_object_churn_counts_zero_kept() {
        // A pure pack/loose-object burst is `.git`-internal but drives no refresh,
        // so it must report kept=0 (previously it wrongly counted every path).
        let never = |_: &Path| false;
        let paths = vec![
            PathBuf::from("/repo/.git/objects/pack/pack-abc.pack"),
            PathBuf::from("/repo/.git/objects/pack/pack-abc.idx"),
            PathBuf::from("/repo/.git/objects/ab/cdef0123"),
            PathBuf::from("/repo/.git/logs/HEAD"),
            PathBuf::from("/repo/.git/FETCH_HEAD"),
        ];
        assert_eq!(kept_count(&never, &paths), 0);
        // And the classification stays None — filter and stat in lockstep.
        assert_eq!(classify_paths_filtered(&git_dir(), &never, &paths), None);
    }

    #[test]
    fn index_write_counts_one_kept() {
        let never = |_: &Path| false;
        let paths = vec![PathBuf::from("/repo/.git/index")];
        assert_eq!(kept_count(&never, &paths), 1);
    }

    #[test]
    fn lock_file_amid_pack_churn_counts_zero_kept() {
        // `index.lock` is `.git`-internal but classify_one drops it → not kept.
        let never = |_: &Path| false;
        let paths = vec![
            PathBuf::from("/repo/.git/index.lock"),
            PathBuf::from("/repo/.git/objects/ab/cdef0123"),
        ];
        assert_eq!(kept_count(&never, &paths), 0);
    }

    #[test]
    fn gitignored_workdir_paths_are_excluded_from_kept() {
        let always = |_: &Path| true;
        let paths = vec![
            PathBuf::from("/repo/target/debug/foo.o"),
            PathBuf::from("/repo/build.log"),
        ];
        assert_eq!(kept_count(&always, &paths), 0);
    }

    #[test]
    fn non_ignored_workdir_paths_stay_kept() {
        let never = |_: &Path| false;
        let paths = vec![
            PathBuf::from("/repo/src/main.rs"),
            PathBuf::from("/repo/README.md"),
        ];
        assert_eq!(kept_count(&never, &paths), 2);
    }

    #[test]
    fn kept_count_matches_classification_on_a_mixed_burst() {
        // Ignore only `*.o`. Kept = index + real source; classification = Index.
        let is_ignored = |p: &Path| p.extension().is_some_and(|e| e == "o");
        let paths = vec![
            PathBuf::from("/repo/target/debug/foo.o"), // ignored workdir  -> drop
            PathBuf::from("/repo/.git/objects/ab/cd"), // object churn      -> drop
            PathBuf::from("/repo/.git/index"),         // index write       -> keep
            PathBuf::from("/repo/src/main.rs"),        // real source edit  -> keep
        ];
        assert_eq!(kept_count(&is_ignored, &paths), 2);
        assert_eq!(
            classify_paths_filtered(&git_dir(), &is_ignored, &paths),
            Some(Reason::Index)
        );
    }

    // ---- Real `.gitignore` integration via `is_path_ignored` ----

    /// A temp repo whose `.gitignore` excludes `target/` and `*.log`.
    fn ignore_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        std::fs::write(
            repo.workdir().unwrap().join(".gitignore"),
            "target/\n*.log\n",
        )
        .unwrap();
        (dir, repo)
    }

    #[test]
    fn real_ignored_build_and_log_churn_produce_no_reason() {
        let (_dir, repo) = ignore_repo();
        let workdir = repo.workdir().unwrap().to_path_buf();
        let git_dir = repo.path().to_path_buf();
        let is_ignored = |p: &Path| path_is_ignored(&repo, p);

        // Neither of these files needs to exist — `is_path_ignored` is rule-based.
        let paths = vec![
            workdir.join("target/debug/foo.o"),
            workdir.join("build.log"),
        ];
        assert_eq!(classify_paths_filtered(&git_dir, &is_ignored, &paths), None);
    }

    #[test]
    fn real_non_ignored_source_edit_is_fs() {
        let (_dir, repo) = ignore_repo();
        let workdir = repo.workdir().unwrap().to_path_buf();
        let git_dir = repo.path().to_path_buf();
        let is_ignored = |p: &Path| path_is_ignored(&repo, p);

        let paths = vec![workdir.join("src/main.rs")];
        assert_eq!(
            classify_paths_filtered(&git_dir, &is_ignored, &paths),
            Some(Reason::Fs)
        );
    }

    #[test]
    fn real_mixed_ignored_and_real_lets_the_real_win() {
        let (_dir, repo) = ignore_repo();
        let workdir = repo.workdir().unwrap().to_path_buf();
        let git_dir = repo.path().to_path_buf();
        let is_ignored = |p: &Path| path_is_ignored(&repo, p);

        let paths = vec![
            workdir.join("target/debug/foo.o"),
            workdir.join("build.log"),
            workdir.join("src/main.rs"),
        ];
        assert_eq!(
            classify_paths_filtered(&git_dir, &is_ignored, &paths),
            Some(Reason::Fs)
        );
    }

    #[test]
    fn real_gitignore_change_itself_is_fs() {
        // `.gitignore` is not itself ignored, so editing it still refreshes.
        let (_dir, repo) = ignore_repo();
        let workdir = repo.workdir().unwrap().to_path_buf();
        let git_dir = repo.path().to_path_buf();
        let is_ignored = |p: &Path| path_is_ignored(&repo, p);

        let paths = vec![workdir.join(".gitignore")];
        assert_eq!(
            classify_paths_filtered(&git_dir, &is_ignored, &paths),
            Some(Reason::Fs)
        );
    }
}
