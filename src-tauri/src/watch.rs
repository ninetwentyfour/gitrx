//! Filesystem watching + debounced auto-refresh.
//!
//! We watch the repository working tree recursively via `notify` (`FSEvents` on
//! macOS). Raw filesystem events are noisy — a single `git status`/`git add`
//! touches the index, a `.lock` file, loose objects, reflogs, and more — so we
//! run every event path through [`fold_event_paths`] to keep only the changes that
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

use std::collections::HashSet;
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

/// Per-burst cache of directories proven git-ignored, so a storm confined to one
/// ignored subtree (a `vendor/` reinstall, a `target/` rebuild — up to 300k+ raw
/// events observed) costs a handful of libgit2 `is_path_ignored` queries instead
/// of one per file.
///
/// Only *positive directory* results are cached: "directory `D` is ignored" soundly
/// implies every path under `D` is ignored. The converse does not hold — a file can
/// be ignored by a pattern (`*.log`) while its directory is not — so a non-ignored
/// parent falls back to querying the file itself, and negatives are never cached.
///
/// The cache lives exactly as long as the per-burst [`Repository`] handle: built
/// fresh after each debounce flush, discarded when the burst ends.
#[derive(Debug, Default)]
struct IgnoreCache {
    /// Absolute directory paths proven ignored during the current burst.
    ignored_dirs: HashSet<PathBuf>,
}

impl IgnoreCache {
    /// Whether `path` (absolute, under `workdir`) is git-ignored, consulting and
    /// populating the ignored-directory cache. `query` is the expensive libgit2
    /// predicate; it is invoked at most once per distinct ancestor directory per
    /// burst rather than once per path.
    fn is_ignored(&mut self, workdir: &Path, query: &impl Fn(&Path) -> bool, path: &Path) -> bool {
        // Fast path: a known-ignored ancestor covers the whole subtree beneath it.
        if self.has_ignored_ancestor(workdir, path) {
            return true;
        }
        // Query the parent directory: a positive result is cacheable and covers
        // every sibling to come. Then walk from the parent toward the workdir root,
        // caching each further ancestor that itself queries as ignored, so the
        // topmost ignored directory lands in the set and later paths take the
        // ancestor fast-path above.
        let Some(parent) = path.parent() else {
            return query(path);
        };
        if parent.starts_with(workdir) && parent != workdir && query(parent) {
            self.insert_ignored_chain(workdir, parent, query);
            return true;
        }
        // Parent not ignored (or the file sits directly in the workdir root): a
        // file pattern like `*.log` can still ignore the file itself. Query it
        // directly and never cache the result — it is file-level, not a subtree.
        query(path)
    }

    /// Whether any ancestor directory of `path` (down to, but excluding, `workdir`)
    /// is already known ignored.
    fn has_ignored_ancestor(&self, workdir: &Path, path: &Path) -> bool {
        for dir in path.ancestors() {
            if !dir.starts_with(workdir) {
                break;
            }
            if self.ignored_dirs.contains(dir) {
                return true;
            }
            if dir == workdir {
                break;
            }
        }
        false
    }

    /// Cache `parent` (already known ignored by the caller) and every ancestor above
    /// it that also queries as ignored, stopping at the first non-ignored ancestor
    /// or the workdir root. This drives the topmost ignored directory into the set
    /// so the whole subtree then hits [`has_ignored_ancestor`].
    fn insert_ignored_chain(
        &mut self,
        workdir: &Path,
        parent: &Path,
        query: &impl Fn(&Path) -> bool,
    ) {
        for dir in parent.ancestors() {
            if !dir.starts_with(workdir) || dir == workdir {
                break;
            }
            // `parent` itself was already proven ignored; only ancestors above it
            // still need a query.
            if dir != parent && !query(dir) {
                break;
            }
            self.ignored_dirs.insert(dir.to_path_buf());
        }
    }
}

/// Classify one event path *after* the ignore filter — the single per-path
/// evaluation that both the `kept` stat and the burst [`Reason`] derive from, so
/// the two can never drift. `Some(reason)` means "kept, contributes `reason`";
/// `None` means "dropped" (an ignored workdir path, or `.git`-internal churn that
/// is not a real trigger).
///
/// `.git`-internal paths bypass the ignore machinery entirely (`is_path_ignored`
/// reasons about working-tree rules and would only confuse them); workdir paths are
/// dropped only when the cache-accelerated ignore check returns `true`.
fn classify_filtered_one(
    git_dir: &Path,
    workdir: &Path,
    is_ignored: &impl Fn(&Path) -> bool,
    cache: &mut IgnoreCache,
    path: &Path,
) -> Option<Reason> {
    if !path.starts_with(git_dir) && cache.is_ignored(workdir, is_ignored, path) {
        return None;
    }
    classify_one(git_dir, path)
}

/// Fold a batch of raw event paths into the burst's `pending` reason and `stats`,
/// evaluating each path exactly once through the cache-accelerated ignore filter.
///
/// This is the pure, closure-injected seam the watcher tests drive (no `FSEvents`,
/// no Tauri handle): they supply an `is_ignored` closure built from a real repo's
/// `.gitignore` — or a counting stub — and inspect `pending`/`stats.kept`.
///
/// `kept` is counted for *every* surviving path, even after the `Index` priority
/// ceiling is reached: the ancestor cache keeps that exact count cheap in a
/// hundred-thousand-path storm, so the stat stays truthful. Only the (now
/// pointless) reason max-fold is short-circuited once `Index` is pending.
fn fold_event_paths(
    git_dir: &Path,
    workdir: &Path,
    is_ignored: &impl Fn(&Path) -> bool,
    cache: &mut IgnoreCache,
    paths: &[PathBuf],
    pending: &mut Option<Reason>,
    stats: &mut BurstStats,
) {
    for path in paths {
        stats.record_raw(path);
        let Some(reason) = classify_filtered_one(git_dir, workdir, is_ignored, cache, path) else {
            continue;
        };
        stats.kept += 1;
        // `Index` is the priority ceiling, so once pending no later path can change
        // the outcome — skip the max-fold entirely (the kept count above stays
        // exact regardless).
        if *pending != Some(Reason::Index) {
            *pending = Some(pending.map_or(reason, |cur| cur.max(reason)));
        }
    }
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
        let ctx = BurstCtx {
            app,
            label,
            git_dir,
            workdir: repo_workdir,
            is_ignored: &is_ignored,
        };
        // Per-burst ignored-directory cache: same lifetime as `repo`, rebuilt each
        // burst. Collapses a storm under one ignored tree to a few libgit2 queries.
        let mut cache = IgnoreCache::default();

        let mut pending: Option<Reason> = None;
        let mut stats = BurstStats::default();
        accumulate(&ctx, &mut cache, first, &mut pending, &mut stats);

        // Coalesce follow-on events until the quiet window elapses.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => accumulate(&ctx, &mut cache, ev, &mut pending, &mut stats),
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

/// Immutable per-burst context threaded through [`accumulate`]. Bundles the handful
/// of references the fold needs so the hot function stays under the argument limit
/// and the debounce loop builds them exactly once per burst.
struct BurstCtx<'a, F: Fn(&Path) -> bool> {
    app: &'a AppHandle,
    label: &'a str,
    git_dir: &'a Path,
    workdir: &'a Path,
    is_ignored: &'a F,
}

/// Fold one event's paths into the pending burst reason and stats. Watcher errors
/// are surfaced as `watch-error` and otherwise ignored (keep going).
fn accumulate<F: Fn(&Path) -> bool>(
    ctx: &BurstCtx<'_, F>,
    cache: &mut IgnoreCache,
    ev: notify::Result<Event>,
    pending: &mut Option<Reason>,
    stats: &mut BurstStats,
) {
    match ev {
        Ok(event) => fold_event_paths(
            ctx.git_dir,
            ctx.workdir,
            ctx.is_ignored,
            cache,
            &event.paths,
            pending,
            stats,
        ),
        Err(e) => {
            log::warn!(target: T_WATCH, "watch error: label={}: {e}", ctx.label);
            let _ = ctx
                .app
                .emit_to(ctx.label, "watch-error", format!("File watch error: {e}"));
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

    /// The working tree root paired with [`git_dir`].
    fn workdir() -> PathBuf {
        PathBuf::from("/repo")
    }

    /// Drive the production per-batch fold with a fresh per-burst cache for the
    /// given git/workdir roots, returning `(reason, kept)` — exactly what a real
    /// burst computes, so tests exercise the shipping code path (cache included).
    fn fold_with(
        git_dir: &Path,
        workdir: &Path,
        is_ignored: &impl Fn(&Path) -> bool,
        paths: &[PathBuf],
    ) -> (Option<Reason>, usize) {
        let mut cache = IgnoreCache::default();
        let mut pending = None;
        let mut stats = BurstStats::default();
        fold_event_paths(
            git_dir,
            workdir,
            is_ignored,
            &mut cache,
            paths,
            &mut pending,
            &mut stats,
        );
        (pending, stats.kept)
    }

    /// [`fold_with`] against the canonical `/repo` roots.
    fn fold(is_ignored: &impl Fn(&Path) -> bool, paths: &[PathBuf]) -> (Option<Reason>, usize) {
        fold_with(&git_dir(), &workdir(), is_ignored, paths)
    }

    /// Pure classification (nothing ignored) — the reason a batch yields.
    fn classify(paths: &[PathBuf]) -> Option<Reason> {
        fold(&|_: &Path| false, paths).0
    }

    /// Classification after the ignore filter (the old `classify_paths_filtered`).
    fn classify_filtered(is_ignored: &impl Fn(&Path) -> bool, paths: &[PathBuf]) -> Option<Reason> {
        fold(is_ignored, paths).0
    }

    /// The `kept` stat a batch produces under the given ignore predicate.
    fn kept_count(is_ignored: &impl Fn(&Path) -> bool, paths: &[PathBuf]) -> usize {
        fold(is_ignored, paths).1
    }

    #[test]
    fn index_write_is_index() {
        let paths = vec![PathBuf::from("/repo/.git/index")];
        assert_eq!(classify(&paths), Some(Reason::Index));
    }

    #[test]
    fn head_change_is_head() {
        let paths = vec![PathBuf::from("/repo/.git/HEAD")];
        assert_eq!(classify(&paths), Some(Reason::Head));
    }

    #[test]
    fn merge_head_is_head() {
        let paths = vec![PathBuf::from("/repo/.git/MERGE_HEAD")];
        assert_eq!(classify(&paths), Some(Reason::Head));
    }

    #[test]
    fn ref_change_is_head() {
        let paths = vec![PathBuf::from("/repo/.git/refs/heads/main")];
        assert_eq!(classify(&paths), Some(Reason::Head));
    }

    #[test]
    fn workdir_file_is_fs() {
        let paths = vec![PathBuf::from("/repo/src/main.rs")];
        assert_eq!(classify(&paths), Some(Reason::Fs));
    }

    #[test]
    fn objects_are_ignored() {
        let paths = vec![PathBuf::from("/repo/.git/objects/ab/cdef0123")];
        assert_eq!(classify(&paths), None);
    }

    #[test]
    fn logs_and_fetch_head_are_ignored() {
        let paths = vec![
            PathBuf::from("/repo/.git/logs/HEAD"),
            PathBuf::from("/repo/.git/FETCH_HEAD"),
        ];
        assert_eq!(classify(&paths), None);
    }

    #[test]
    fn index_lock_is_ignored() {
        let paths = vec![PathBuf::from("/repo/.git/index.lock")];
        assert_eq!(classify(&paths), None);
    }

    #[test]
    fn ref_lock_is_ignored() {
        // A `refs/**/*.lock` must be dropped even though `refs/` is otherwise kept.
        let paths = vec![PathBuf::from("/repo/.git/refs/heads/main.lock")];
        assert_eq!(classify(&paths), None);
    }

    #[test]
    fn mixed_index_and_workdir_prioritises_index() {
        let paths = vec![
            PathBuf::from("/repo/src/main.rs"),
            PathBuf::from("/repo/.git/index"),
        ];
        assert_eq!(classify(&paths), Some(Reason::Index));
    }

    #[test]
    fn mixed_head_and_workdir_prioritises_head() {
        let paths = vec![
            PathBuf::from("/repo/README.md"),
            PathBuf::from("/repo/.git/refs/heads/main"),
        ];
        assert_eq!(classify(&paths), Some(Reason::Head));
    }

    #[test]
    fn empty_batch_is_none() {
        assert_eq!(classify(&[]), None);
    }

    // ---- Ignore-filter seam (pure, no repo) ----

    #[test]
    fn git_internal_paths_bypass_the_ignore_filter() {
        // Even an "ignore everything" predicate must not drop a `.git/index`
        // write — internal paths keep their curated handling.
        let always = |_: &Path| true;
        let paths = vec![PathBuf::from("/repo/.git/index")];
        assert_eq!(classify_filtered(&always, &paths), Some(Reason::Index));
    }

    #[test]
    fn ignored_workdir_paths_are_dropped_by_the_seam() {
        // A workdir path the predicate calls ignored yields nothing to refresh.
        let always = |_: &Path| true;
        let paths = vec![PathBuf::from("/repo/src/main.rs")];
        assert_eq!(classify_filtered(&always, &paths), None);
    }

    #[test]
    fn seam_keeps_the_real_path_when_only_some_are_ignored() {
        // Ignore only `*.o`; the real source edit must still win as `Fs`.
        let is_ignored = |p: &Path| p.extension().is_some_and(|e| e == "o");
        let paths = vec![
            PathBuf::from("/repo/target/debug/foo.o"),
            PathBuf::from("/repo/src/main.rs"),
        ];
        assert_eq!(classify_filtered(&is_ignored, &paths), Some(Reason::Fs));
    }

    // ---- Burst `kept` stat (the storm-count fix) ----

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
        // Filter and stat move in lockstep: both derive from one evaluation.
        assert_eq!(fold(&never, &paths), (None, 0));
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
        assert_eq!(fold(&is_ignored, &paths), (Some(Reason::Index), 2));
    }

    // ---- Per-burst ignored-ancestor cache (the CPU fix) ----

    #[test]
    fn cache_collapses_a_storm_under_one_ignored_dir_to_a_few_queries() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        // A `vendor/`-style rule: anything under /repo/vendor is ignored. Count how
        // many times the (expensive) predicate is actually consulted.
        let is_ignored = |p: &Path| {
            calls.set(calls.get() + 1);
            p.starts_with("/repo/vendor")
        };
        // 10k churned files three directory levels deep under the one ignored tree.
        let paths: Vec<PathBuf> = (0..10_000)
            .map(|i| PathBuf::from(format!("/repo/vendor/pkg{}/src/file{i}.php", i % 20)))
            .collect();
        assert_eq!(
            fold_with(&git_dir(), &workdir(), &is_ignored, &paths),
            (None, 0)
        );
        // Depth from workdir to the deepest ignored dir is 3 (vendor/pkgN/src); the
        // ancestor cache must need only a handful of queries — never one per file.
        assert!(
            calls.get() <= 3 + 2,
            "ignore predicate called {} times for 10k paths",
            calls.get()
        );
    }

    #[test]
    fn mixed_storm_still_classifies_and_counts_with_cache_active() {
        // A vendor churn storm plus one real edit and one index write: the cache
        // must not swallow the real signals.
        let is_ignored = |p: &Path| p.starts_with("/repo/vendor");
        let mut paths: Vec<PathBuf> = (0..1_000)
            .map(|i| PathBuf::from(format!("/repo/vendor/a/b/f{i}.php")))
            .collect();
        paths.push(PathBuf::from("/repo/src/main.rs")); // real source -> kept, Fs
        paths.push(PathBuf::from("/repo/.git/index")); // index write -> kept, Index
        assert_eq!(
            fold_with(&git_dir(), &workdir(), &is_ignored, &paths),
            (Some(Reason::Index), 2)
        );
    }

    #[test]
    fn kept_stays_exact_after_the_index_ceiling() {
        // The chosen behaviour: `kept` keeps counting real refresh-drivers even
        // after the Index ceiling fixes the reason (the cache keeps it cheap).
        let never = |_: &Path| false;
        let paths = vec![
            PathBuf::from("/repo/.git/index"),  // sets the Index ceiling first
            PathBuf::from("/repo/src/main.rs"), // arrives after the ceiling, still kept
            PathBuf::from("/repo/README.md"),   // ditto
        ];
        assert_eq!(fold(&never, &paths), (Some(Reason::Index), 3));
    }

    #[test]
    fn file_pattern_ignore_survives_the_parent_dir_cache() {
        // `*.log` semantics: the directory is NOT ignored, only the file is — the
        // parent-dir cache must not over-generalise and drop the sibling source.
        let is_ignored = |p: &Path| p.extension().is_some_and(|e| e == "log");
        let paths = vec![
            PathBuf::from("/repo/src/app.log"), // file-ignored; /repo/src is not
            PathBuf::from("/repo/src/main.rs"), // real source -> kept
        ];
        assert_eq!(
            fold_with(&git_dir(), &workdir(), &is_ignored, &paths),
            (Some(Reason::Fs), 1)
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
        assert_eq!(fold_with(&git_dir, &workdir, &is_ignored, &paths).0, None);
    }

    #[test]
    fn real_non_ignored_source_edit_is_fs() {
        let (_dir, repo) = ignore_repo();
        let workdir = repo.workdir().unwrap().to_path_buf();
        let git_dir = repo.path().to_path_buf();
        let is_ignored = |p: &Path| path_is_ignored(&repo, p);

        let paths = vec![workdir.join("src/main.rs")];
        assert_eq!(
            fold_with(&git_dir, &workdir, &is_ignored, &paths).0,
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
            fold_with(&git_dir, &workdir, &is_ignored, &paths).0,
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
            fold_with(&git_dir, &workdir, &is_ignored, &paths).0,
            Some(Reason::Fs)
        );
    }

    #[test]
    fn real_file_pattern_ignore_in_subdir_keeps_the_sibling() {
        // `*.log` ignores the file wherever it lives, but its directory is not
        // ignored — the parent-dir cache must not drop the real sibling edit.
        let (_dir, repo) = ignore_repo();
        let workdir = repo.workdir().unwrap().to_path_buf();
        let git_dir = repo.path().to_path_buf();
        let is_ignored = |p: &Path| path_is_ignored(&repo, p);

        let paths = vec![
            workdir.join("src/debug.log"), // file-ignored via `*.log`
            workdir.join("src/main.rs"),   // real source -> kept, Fs
        ];
        assert_eq!(
            fold_with(&git_dir, &workdir, &is_ignored, &paths),
            (Some(Reason::Fs), 1)
        );
    }
}
