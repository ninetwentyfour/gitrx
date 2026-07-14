use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use notify::RecommendedWatcher;

/// The selection backing a currently-open (or most-recently-opened) native file
/// context menu. Stored in [`AppState`] so the dynamic menu-item ids stay
/// static (`ctx:stage`, …) while the *paths* they act on are looked up here at
/// click time. Replaced wholesale on every `show_file_context_menu` call, so a
/// second right-click while a first menu is open cleanly supersedes it (macOS
/// context menus are modal, so at most one is ever interactable).
#[derive(Debug, Clone)]
pub struct PendingCtxMenu {
    /// Repo-relative paths the menu acts on (already filtered to ones that still
    /// exist in `git status` at build time).
    pub paths: Vec<String>,
    /// Which panel the selection came from (staged vs. unstaged), controlling
    /// which actions the menu offers.
    pub staged: bool,
    /// Label of the window the menu was raised from. Post-action `repo-changed`
    /// events are targeted at this window so only its view refreshes, and the
    /// acting repository is resolved from `windows[window]`.
    pub window: String,
}

/// The repository bound to a single app window, plus the filesystem watcher that
/// keeps that window's view live.
///
/// One [`AppState::windows`] entry exists per open repo window, keyed by the
/// window's label. The watcher is parked here so it stays alive for the lifetime
/// of the binding; dropping the entry (window closed, or a new repo opened in the
/// same window) drops the watcher, which stops the watch and lets its debouncer
/// thread exit on channel disconnect.
///
/// `watcher` is optional so a repo can still be bound (and fully usable) when the
/// filesystem watcher fails to start — a best-effort concern, matching the
/// pre-refactor behavior of surfacing `watch-error` and carrying on.
pub struct WindowRepo {
    pub repo_path: PathBuf,
    /// Held purely for its RAII effect: keeping the watcher alive keeps the watch
    /// running; dropping this entry stops it. Never read directly.
    #[allow(dead_code)]
    pub watcher: Option<RecommendedWatcher>,
}

/// Shared application state managed by Tauri via `.manage()`.
///
/// We deliberately store only the *path* to each window's active repository, not
/// a `git2::Repository` handle. `Repository` is not `Sync`, and caching it across
/// commands invites stale in-memory index/status data. Each command re-opens the
/// repository from the stored path, which is cheap and always reflects on-disk
/// truth.
#[derive(Default)]
pub struct AppState {
    /// Per-window repository bindings, keyed by window label. Each window shows a
    /// different repository; commands resolve theirs via the injected window's
    /// label.
    pub windows: Mutex<HashMap<String, WindowRepo>>,
    /// Serializes all index/working-tree-mutating commands (stage, unstage,
    /// discard, hunk operations, commit) across *every* window so concurrent
    /// invocations cannot interleave writes to an on-disk index. Different
    /// repositories could in principle proceed in parallel, but a single global
    /// lock is simple and not the bottleneck.
    pub write_lock: tokio::sync::Mutex<()>,
    /// Backing selection for the native file context menu (see
    /// [`PendingCtxMenu`]).
    pub pending_ctx_menu: Mutex<Option<PendingCtxMenu>>,
    /// Working-tree path of the most-recently-*opened* repository (updated on
    /// every window↔repo bind). Backs the legacy `lastRepoPath` persistence key,
    /// which must reflect open order — not the alphabetically-last path a sorted
    /// snapshot would otherwise yield.
    pub last_opened: Mutex<Option<PathBuf>>,
    /// Set once the app is tearing down (`RunEvent::ExitRequested`) so the
    /// per-window `Destroyed` handler does not rewrite the persisted open-repos
    /// set while every window closes on quit — that would clobber the very set we
    /// want to restore on next launch.
    pub exiting: AtomicBool,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }
}
