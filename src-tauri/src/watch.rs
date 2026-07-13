//! Filesystem watching + debounced auto-refresh.
//!
//! We watch the repository working tree recursively via `notify` (FSEvents on
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

/// Trailing-edge quiet period: once no new event has arrived for this long, the
/// accumulated burst is flushed as one `repo-changed` event.
const DEBOUNCE: Duration = Duration::from_millis(200);

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
    fn as_str(self) -> &'static str {
        match self {
            Reason::Index => "index",
            Reason::Head => "head",
            Reason::Fs => "fs",
        }
    }

    /// Priority rank; higher wins when a burst mixes categories.
    fn rank(self) -> u8 {
        match self {
            Reason::Index => 2,
            Reason::Head => 1,
            Reason::Fs => 0,
        }
    }

    /// Return whichever of `self`/`other` has the higher priority.
    fn max(self, other: Reason) -> Reason {
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

    match path.strip_prefix(git_dir) {
        // Inside `.git`: keep only a curated allow-list, drop everything else.
        Ok(rel) => {
            if rel == Path::new("index") {
                Some(Reason::Index)
            } else if rel == Path::new("HEAD") || rel == Path::new("MERGE_HEAD") {
                Some(Reason::Head)
            } else if rel.starts_with("refs") {
                Some(Reason::Head)
            } else {
                None
            }
        }
        // Outside `.git`: a working-tree change is always relevant (gitignored
        // churn is harmless — the refresh it triggers is cheap and debounced).
        Err(_) => Some(Reason::Fs),
    }
}

/// Pure classification of a batch of event paths into an optional [`Reason`].
///
/// `None` means "nothing worth refreshing" (e.g. only objects/locks changed).
/// Otherwise the highest-priority reason across all kept paths is returned.
pub fn classify_paths(git_dir: &Path, paths: &[PathBuf]) -> Option<Reason> {
    let mut best: Option<Reason> = None;
    for path in paths {
        if let Some(reason) = classify_one(git_dir, path) {
            best = Some(match best {
                Some(cur) => cur.max(reason),
                None => reason,
            });
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
/// `.git`-internal paths bypass the ignore check entirely: their curated
/// handling lives in [`classify_paths`], and `is_path_ignored` would only
/// confuse them (it reasons about working-tree ignore rules). So the filter
/// keeps every path under `git_dir` and drops only workdir paths for which
/// `is_ignored` returns `true`.
///
/// This is the testable seam — the watcher tests drive it with a closure built
/// from a real repo's `.gitignore`, no FSEvents required. [`classify_paths`]
/// itself stays pure.
fn classify_paths_filtered(
    git_dir: &Path,
    is_ignored: &impl Fn(&Path) -> bool,
    paths: &[PathBuf],
) -> Option<Reason> {
    let kept: Vec<PathBuf> = paths
        .iter()
        .filter(|p| p.starts_with(git_dir) || !is_ignored(p))
        .cloned()
        .collect();
    classify_paths(git_dir, &kept)
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
    let git_dir = open_repository(repo_workdir)
        .map(|repo| repo.path().to_path_buf())
        .unwrap_or_else(|_| repo_workdir.join(".git"));

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    // The notify callback runs on notify's own thread. Never panic there — just
    // forward every result (Ok and Err) to the debouncer for handling.
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(repo_workdir, RecursiveMode::Recursive)?;

    let app = app.clone();
    let label = label.to_string();
    let repo_workdir = repo_workdir.to_path_buf();
    std::thread::spawn(move || debounce_loop(&app, &label, &git_dir, &repo_workdir, rx));

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
    rx: mpsc::Receiver<notify::Result<Event>>,
) {
    loop {
        // Wait (indefinitely) for the first event of the next burst.
        let first = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => return, // watcher dropped — we're done
        };

        // Open the repo once per burst to answer ignore queries (cheap; matches
        // the codebase's reopen-per-call convention). If the open fails we fall
        // back to "nothing ignored" so a real change is never dropped.
        let repo = open_repository(repo_workdir).ok();
        let is_ignored = |path: &Path| match &repo {
            Some(repo) => path_is_ignored(repo, path),
            None => false,
        };

        let mut pending: Option<Reason> = None;
        accumulate(app, label, git_dir, &is_ignored, first, &mut pending);

        // Coalesce follow-on events until the quiet window elapses.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => accumulate(app, label, git_dir, &is_ignored, ev, &mut pending),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    emit_if_any(app, label, pending);
                    return;
                }
            }
        }

        emit_if_any(app, label, pending);
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
) {
    match ev {
        Ok(event) => {
            if let Some(reason) = classify_paths_filtered(git_dir, is_ignored, &event.paths) {
                *pending = Some(match *pending {
                    Some(cur) => cur.max(reason),
                    None => reason,
                });
            }
        }
        Err(e) => {
            let _ = app.emit_to(label, "watch-error", format!("File watch error: {e}"));
        }
    }
}

/// Emit a single `repo-changed` to the owning window if the burst kept anything
/// worth refreshing.
fn emit_if_any(app: &AppHandle, label: &str, pending: Option<Reason>) {
    if let Some(reason) = pending {
        let _ = app.emit_to(
            label,
            "repo-changed",
            RepoChanged {
                reason: reason.as_str(),
            },
        );
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
