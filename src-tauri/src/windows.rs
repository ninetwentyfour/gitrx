//! Multi-window repository management (GitX-style: one app, many repo windows).
//!
//! Each repo lives in its own window, keyed by a **stable label** derived from
//! the repository's canonical working-tree path (`repo-<hex>`). A stable label
//! lets `tauri-plugin-window-state` restore each repo window's geometry across
//! launches, and lets us find an already-open window for a repo to focus rather
//! than duplicate it. The initial window keeps the config-default label `main`
//! and is assigned whatever repo the CLI arg / restore flow hands it.
//!
//! Responsibilities:
//! - the pure open-or-focus decision (`decide_open`) and label hashing,
//! - binding a repo to a window (`set_window_repo`) — building and parking that
//!   window's filesystem watcher,
//! - opening-or-focusing a window for a path (`open_or_focus`), used by the
//!   single-instance forwarding callback,
//! - persisting the set of open repos to `settings.json` so the app can restore
//!   every window on next launch (`persist_open_repos` / `load_open_repos`).

use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_store::StoreExt;

use crate::error::{AppError, AppResult};
use crate::git::open_repository;
use crate::state::{AppState, WindowRepo};

/// Label of the initial window (matches the single entry in `tauri.conf.json`).
pub const MAIN_LABEL: &str = "main";

/// Settings file (shared with the frontend's theme persistence) and the keys the
/// window-lifecycle persistence owns.
const SETTINGS_STORE: &str = "settings.json";
const OPEN_REPOS_KEY: &str = "openRepos";
const LAST_REPO_KEY: &str = "lastRepoPath";

/// Default window geometry, kept in sync with `tauri.conf.json`'s `app.windows`
/// entry so spawned repo windows match the initial one.
const WINDOW_WIDTH: f64 = 1200.0;
const WINDOW_HEIGHT: f64 = 800.0;
const WINDOW_MIN_WIDTH: f64 = 800.0;
const WINDOW_MIN_HEIGHT: f64 = 600.0;
const WINDOW_TITLE: &str = "gitrx";

/// The outcome of resolving a repo path against the currently-open windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenDecision {
    /// A window already shows this repo; focus the given label.
    Focus(String),
    /// No window shows it; create a new window with the given (stable) label.
    CreateNew(String),
}

/// Normalize a path for comparison/hashing: lossy string with any trailing
/// separators trimmed (git2 reports workdirs with a trailing `/`, and a subdir
/// resolves to the same workdir). The root `/` is preserved.
fn normalize(path: &Path) -> String {
    let s = path.to_string_lossy();
    let trimmed = s.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// FNV-1a 64-bit hash — a tiny, dependency-free stable hash for label derivation.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The stable window label for a repository at `canonical_path`.
///
/// Same path → same label (geometry restore, dedupe); distinct paths → distinct
/// labels (barring an astronomically unlikely 64-bit collision). Trailing-slash
/// differences normalize to the same label.
pub fn label_for_repo(canonical_path: &Path) -> String {
    format!("repo-{:016x}", fnv1a(normalize(canonical_path).as_bytes()))
}

/// Decide whether to focus an existing window or create a new one for the
/// (already canonicalized) repo working-tree path `canonical`.
///
/// `existing` is the current `(label, repo_path)` set. Comparison is
/// normalization-aware so a trailing-slash / already-open repo focuses rather
/// than duplicates.
pub fn decide_open<'a>(
    existing: impl IntoIterator<Item = (&'a str, &'a Path)>,
    canonical: &Path,
) -> OpenDecision {
    let target = normalize(canonical);
    for (label, path) in existing {
        if normalize(path) == target {
            return OpenDecision::Focus(label.to_string());
        }
    }
    OpenDecision::CreateNew(label_for_repo(canonical))
}

/// Resolve any path (repo root, subdirectory, or a path with a trailing slash) to
/// the enclosing repository's canonical working-tree path — the stable key used
/// for labels, comparison, and persistence.
pub fn resolve_workdir(path: &Path) -> AppResult<PathBuf> {
    let repo = open_repository(path)?;
    let workdir = repo.workdir().unwrap_or_else(|| repo.path()).to_path_buf();
    // Collapse symlinks / relative artifacts so the same repo always yields the
    // same key regardless of how it was reached.
    Ok(std::fs::canonicalize(&workdir).unwrap_or(workdir))
}

/// Bind `repo_path` to the window `label`, (re)starting that window's filesystem
/// watcher. Any prior binding for the label is replaced (dropping its watcher).
///
/// Best-effort watching: a watcher-build failure surfaces a `watch-error` to the
/// window and still records the binding, so the window remains fully usable.
pub fn set_window_repo(app: &AppHandle, label: &str, repo_path: PathBuf) {
    let watcher = match crate::watch::build_watcher(app, label, &repo_path) {
        Ok(watcher) => Some(watcher),
        Err(e) => {
            let _ = app.emit_to(
                label,
                "watch-error",
                format!("Failed to start file watcher: {e}"),
            );
            None
        }
    };

    let state = app.state::<AppState>();
    let mut windows = match state.windows.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    windows.insert(label.to_string(), WindowRepo { repo_path, watcher });
}

/// Remove a window's binding (dropping its watcher). Returns whether an entry was
/// present.
pub fn remove_window(app: &AppHandle, label: &str) -> bool {
    let state = app.state::<AppState>();
    let removed = match state.windows.lock() {
        Ok(mut windows) => windows.remove(label).is_some(),
        Err(_) => false,
    };
    removed
}

/// Open a window for the repository at `workdir` (already a canonical working
/// tree), or focus the existing one. Used by the single-instance forwarding
/// callback. On create, persists the updated open-repos set.
pub fn open_or_focus(app: &AppHandle, workdir: PathBuf) -> Result<(), String> {
    let decision = {
        let state = app.state::<AppState>();
        let windows = state
            .windows
            .lock()
            .map_err(|_| "Internal state lock poisoned".to_string())?;
        let snapshot: Vec<(String, PathBuf)> = windows
            .iter()
            .map(|(label, repo)| (label.clone(), repo.repo_path.clone()))
            .collect();
        decide_open(
            snapshot.iter().map(|(l, p)| (l.as_str(), p.as_path())),
            &workdir,
        )
    };

    match decision {
        OpenDecision::Focus(label) => {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            Ok(())
        }
        OpenDecision::CreateNew(label) => create_repo_window(app, &label, workdir),
    }
}

/// Create a new repo window with the stable `label` for `workdir`.
///
/// Binds the repo BEFORE building the window so the fresh frontend's `get_status`
/// resolves its repo by label with zero extra round-trips. Rolls the binding back
/// if the window fails to build.
pub fn create_repo_window(app: &AppHandle, label: &str, workdir: PathBuf) -> Result<(), String> {
    set_window_repo(app, label, workdir);

    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .title(WINDOW_TITLE)
        .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
        .min_inner_size(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)
        .build();

    match built {
        Ok(_) => {
            persist_open_repos(app);
            Ok(())
        }
        Err(e) => {
            remove_window(app, label);
            Err(format!("Failed to open window: {e}"))
        }
    }
}

/// Focus any existing window (used when a second invocation carries no path):
/// prefer the frontmost/focused one, else any.
pub fn focus_any_window(app: &AppHandle) {
    let windows = app.webview_windows();
    let window = windows
        .values()
        .find(|w| w.is_focused().unwrap_or(false))
        .cloned()
        .or_else(|| windows.values().next().cloned());
    if let Some(window) = window {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// ---------------------------------------------------------------------------
// Persistence of the open-repos set (restore-on-launch)
// ---------------------------------------------------------------------------

/// Collect the distinct repo paths currently bound to windows, sorted for a
/// deterministic on-disk representation.
fn collect_open_repos(app: &AppHandle) -> Vec<String> {
    let state = app.state::<AppState>();
    let Ok(windows) = state.windows.lock() else {
        return Vec::new();
    };
    let mut repos: Vec<String> = windows
        .values()
        .map(|repo| repo.repo_path.to_string_lossy().into_owned())
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

/// Persist the current open-repos set to `settings.json`.
///
/// Written from Rust (not the webview) so window lifecycle changes persist even
/// without a live frontend. The save is **explicit**: the store's default
/// auto-save is a 100 ms debounce that a quit can outrun, so lifecycle writes
/// flush to disk immediately.
///
/// Empty-set guard: an empty set is never written, so the last non-empty snapshot
/// survives app teardown (every window emits `Destroyed` on quit) and is
/// available to restore next launch.
pub fn persist_open_repos(app: &AppHandle) {
    let repos = collect_open_repos(app);
    if repos.is_empty() {
        return;
    }

    if let Ok(store) = app.store(SETTINGS_STORE) {
        store.set(OPEN_REPOS_KEY, open_repos_to_json(&repos));
        // Keep the legacy single-repo key populated for backward compatibility.
        if let Some(last) = repos.last() {
            store.set(LAST_REPO_KEY, JsonValue::String(last.clone()));
        }
        let _ = store.save();
    }
}

/// Load the persisted open-repos set, falling back to the legacy single
/// `lastRepoPath` key when `openRepos` is absent (migration).
pub fn load_open_repos(app: &AppHandle) -> Vec<String> {
    let Ok(store) = app.store(SETTINGS_STORE) else {
        return Vec::new();
    };
    if let Some(value) = store.get(OPEN_REPOS_KEY) {
        return parse_open_repos(&value);
    }
    // Backward-compat migration: a prior single-repo session.
    store
        .get(LAST_REPO_KEY)
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
        .map(|s| vec![s])
        .unwrap_or_default()
}

/// Serialize the open-repos set to a JSON string array. Pure (testable without a
/// live store).
pub fn open_repos_to_json(paths: &[String]) -> JsonValue {
    JsonValue::Array(paths.iter().cloned().map(JsonValue::String).collect())
}

/// Parse an `openRepos` JSON value back into a path list, dropping any non-string
/// or empty entries. Pure inverse of [`open_repos_to_json`].
pub fn parse_open_repos(value: &JsonValue) -> Vec<String> {
    match value {
        JsonValue::Array(items) => items
            .iter()
            .filter_map(JsonValue::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// CLI-arg resolution (shared by startup and the single-instance callback)
// ---------------------------------------------------------------------------

/// The first non-flag positional argument (a path), if any. `argv[0]` is the
/// binary and is skipped. Pure and testable.
pub fn first_path_arg(argv: &[String]) -> Option<String> {
    argv.iter()
        .skip(1)
        .find(|a| !a.is_empty() && !a.starts_with('-'))
        .cloned()
}

/// Resolve a possibly-relative `arg` against the launching process's `cwd`.
///
/// A second `gitrx` invocation runs in a *different* directory than the running
/// app, so its relative paths must resolve against the invoker's cwd (delivered
/// by the single-instance plugin), not ours. Absolute args pass through.
pub fn resolve_arg_path(arg: &str, cwd: &str) -> PathBuf {
    let path = Path::new(arg);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(cwd).join(path)
    }
}

/// Full resolution of a CLI path arg (relative-to-`cwd` join, then repo
/// discovery) to a canonical working tree.
pub fn resolve_cli_workdir(arg: &str, cwd: &str) -> AppResult<PathBuf> {
    let candidate = resolve_arg_path(arg, cwd);
    if !candidate.exists() {
        return Err(AppError::msg(format!(
            "Cannot resolve path '{}'",
            candidate.display()
        )));
    }
    resolve_workdir(&candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, RepositoryInitOptions};
    use tempfile::tempdir;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // ---- Label hashing ----

    #[test]
    fn label_is_stable_for_the_same_path() {
        assert_eq!(
            label_for_repo(&p("/home/me/repo")),
            label_for_repo(&p("/home/me/repo"))
        );
    }

    #[test]
    fn label_distinguishes_different_paths() {
        assert_ne!(
            label_for_repo(&p("/home/me/a")),
            label_for_repo(&p("/home/me/b"))
        );
    }

    #[test]
    fn label_ignores_trailing_slash() {
        assert_eq!(
            label_for_repo(&p("/home/me/repo")),
            label_for_repo(&p("/home/me/repo/"))
        );
    }

    #[test]
    fn label_has_expected_shape() {
        let label = label_for_repo(&p("/x"));
        assert!(label.starts_with("repo-"));
        assert_eq!(label.len(), "repo-".len() + 16);
    }

    // ---- decide_open ----

    #[test]
    fn decide_open_creates_when_empty() {
        let existing: Vec<(String, PathBuf)> = Vec::new();
        let decision = decide_open(
            existing.iter().map(|(l, p)| (l.as_str(), p.as_path())),
            &p("/repos/new"),
        );
        assert_eq!(
            decision,
            OpenDecision::CreateNew(label_for_repo(&p("/repos/new")))
        );
    }

    #[test]
    fn decide_open_focuses_existing_match() {
        let existing = vec![
            ("main".to_string(), p("/repos/a")),
            ("repo-x".to_string(), p("/repos/b")),
        ];
        let decision = decide_open(
            existing.iter().map(|(l, p)| (l.as_str(), p.as_path())),
            &p("/repos/b"),
        );
        assert_eq!(decision, OpenDecision::Focus("repo-x".to_string()));
    }

    #[test]
    fn decide_open_focus_is_trailing_slash_insensitive() {
        // Stored with git2's trailing slash; requested without (and vice versa).
        let existing = vec![("main".to_string(), p("/repos/a/"))];
        let decision = decide_open(
            existing.iter().map(|(l, p)| (l.as_str(), p.as_path())),
            &p("/repos/a"),
        );
        assert_eq!(decision, OpenDecision::Focus("main".to_string()));
    }

    // ---- resolve_workdir (real repo: subdir + trailing-slash normalization) ----

    fn init_repo(dir: &Path) -> Repository {
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(dir, &opts).unwrap()
    }

    #[test]
    fn resolve_workdir_maps_root_and_subdir_to_same_key() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        init_repo(&root);
        let sub = root.join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();

        let from_root = resolve_workdir(&root).unwrap();
        let from_sub = resolve_workdir(&sub).unwrap();
        assert_eq!(normalize(&from_root), normalize(&from_sub));
        assert_eq!(normalize(&from_root), normalize(&root));
    }

    #[test]
    fn resolve_workdir_rejects_non_repo() {
        let dir = tempdir().unwrap();
        assert!(resolve_workdir(dir.path()).is_err());
    }

    // ---- openRepos persistence round-trip ----

    #[test]
    fn open_repos_round_trip() {
        let paths = vec!["/repos/a".to_string(), "/repos/b".to_string()];
        let json = open_repos_to_json(&paths);
        assert_eq!(parse_open_repos(&json), paths);
    }

    #[test]
    fn parse_open_repos_drops_non_strings_and_empties() {
        let value = serde_json::json!(["/repos/a", "", 42, null, "/repos/b"]);
        assert_eq!(
            parse_open_repos(&value),
            vec!["/repos/a".to_string(), "/repos/b".to_string()]
        );
    }

    #[test]
    fn parse_open_repos_non_array_is_empty() {
        assert!(parse_open_repos(&serde_json::json!("nope")).is_empty());
        assert!(parse_open_repos(&serde_json::json!(null)).is_empty());
    }

    // ---- argv parsing ----

    #[test]
    fn first_path_arg_takes_first_positional() {
        let argv = vec!["gitrx".to_string(), "/repos/a".to_string()];
        assert_eq!(first_path_arg(&argv), Some("/repos/a".to_string()));
    }

    #[test]
    fn first_path_arg_skips_flags_and_empties() {
        let argv = vec![
            "gitrx".to_string(),
            "--flag".to_string(),
            "".to_string(),
            "/repos/b".to_string(),
        ];
        assert_eq!(first_path_arg(&argv), Some("/repos/b".to_string()));
    }

    #[test]
    fn first_path_arg_none_when_only_binary() {
        assert_eq!(first_path_arg(&["gitrx".to_string()]), None);
    }

    // ---- relative-arg resolution against a provided cwd ----

    #[test]
    fn resolve_arg_path_joins_relative_against_cwd() {
        assert_eq!(resolve_arg_path("sub/dir", "/work"), p("/work/sub/dir"));
        assert_eq!(resolve_arg_path(".", "/work"), p("/work/."));
    }

    #[test]
    fn resolve_arg_path_passes_absolute_through() {
        assert_eq!(resolve_arg_path("/abs/path", "/work"), p("/abs/path"));
    }
}
