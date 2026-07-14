//! Native, GitX-style right-click menu for files in the staging panels.
//!
//! The frontend sends only the selected repo-relative paths plus which panel
//! they came from; this module re-derives each file's real status from git2
//! (never trusting the client beyond the path strings), builds the appropriate
//! native menu, and pops it at the cursor on the main window.
//!
//! ## Menu-item id scheme
//!
//! All dynamic ids are namespaced under `ctx:` (`ctx:stage`, `ctx:unstage`,
//! `ctx:ignore`, `ctx:discard`, `ctx:trash`, `ctx:open`, `ctx:reveal`) so they
//! never collide with the static application-menu ids handled in `menu.rs`. The
//! ids carry no per-file data: the acting selection lives in
//! [`AppState::pending_ctx_menu`], replaced on each menu open, so a rapid second
//! right-click cleanly supersedes the first.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use git2::{Repository, Status, StatusOptions};
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::error::{AppError, AppResult};
use crate::git::{
    discard_file, open_repository, stage_file, unstage_file, validate_repo_relative_path,
};
use crate::state::{AppState, PendingCtxMenu};

/// Id prefix shared by every dynamic context-menu item.
const CTX_PREFIX: &str = "ctx:";

/// The concrete action an item triggers, resolved from its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CtxAction {
    Stage,
    Unstage,
    Ignore,
    Discard,
    Trash,
    Open,
    Reveal,
}

impl CtxAction {
    fn from_id(id: &str) -> Option<Self> {
        match id {
            "ctx:stage" => Some(Self::Stage),
            "ctx:unstage" => Some(Self::Unstage),
            "ctx:ignore" => Some(Self::Ignore),
            "ctx:discard" => Some(Self::Discard),
            "ctx:trash" => Some(Self::Trash),
            "ctx:open" => Some(Self::Open),
            "ctx:reveal" => Some(Self::Reveal),
            _ => None,
        }
    }

    /// Whether this action is valid for a selection taken from the given panel.
    /// `Unstage` is staged-only; `Stage`/`Ignore`/`Discard`/`Trash` are
    /// unstaged-only; `Open`/`Reveal` are valid from either.
    const fn matches_panel(self, staged: bool) -> bool {
        match self {
            Self::Unstage => staged,
            Self::Stage | Self::Ignore | Self::Discard | Self::Trash => !staged,
            Self::Open | Self::Reveal => true,
        }
    }
}

/// Payload for the `repo-changed` event (kept in sync with `watch.rs`). Emitted
/// after a mutating action so the frontend refreshes immediately rather than
/// waiting for the debounced filesystem watcher.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoChanged {
    reason: &'static str,
}

/// Build and pop a native context menu for `paths` at the cursor.
///
/// `staged` selects the panel (and thus the action set). Paths are validated,
/// then filtered down to those that still appear in `git status`; if none
/// survive, no menu is shown.
#[tauri::command]
pub async fn show_file_context_menu(
    paths: Vec<String>,
    staged: bool,
    app: AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
) -> Result<(), AppError> {
    let repo_path =
        repo_path_for_window(&app, window.label()).ok_or_else(AppError::no_repo_open)?;

    // Lexically validate every path up front; silently drop malformed ones
    // rather than failing the whole menu.
    let candidates: Vec<String> = paths
        .into_iter()
        .filter(|p| validate_repo_relative_path(&repo_path, p).is_ok())
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }

    // Re-derive status off the async worker (git2 handles are not `Send`).
    let derived = {
        let repo_path = repo_path.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<Derived, AppError> {
            let repo = open_repository(&repo_path)?;
            derive_selection(&repo, &candidates, staged)
        })
        .await
        .map_err(|e| AppError::git(format!("Failed to derive file status: {e}")))??
    };

    if derived.paths.is_empty() {
        return Ok(());
    }

    // Record the acting selection before showing the menu so click handlers can
    // resolve it by static id.
    {
        let mut guard = state
            .pending_ctx_menu
            .lock()
            .map_err(|_| AppError::git("Internal state lock poisoned"))?;
        *guard = Some(PendingCtxMenu {
            paths: derived.paths.clone(),
            staged,
            window: window.label().to_string(),
        });
    }

    let menu = build_menu(&app, &derived)
        .map_err(|e| AppError::git(format!("Failed to build menu: {e}")))?;

    // Pop the menu on the window that requested it (not a hardcoded "main").
    window
        .popup_menu(&menu)
        .map_err(|e| AppError::git(format!("Failed to show context menu: {e}")))?;

    Ok(())
}

/// The repository path bound to `label`, if any.
fn repo_path_for_window<R: Runtime>(app: &AppHandle<R>, label: &str) -> Option<std::path::PathBuf> {
    let state = app.state::<AppState>();
    let windows = state.windows.lock().ok()?;
    windows.get(label).map(|repo| repo.repo_path.clone())
}

/// The status-derived facts the menu is built from.
struct Derived {
    /// Paths that still exist in `git status`, in the original selection order.
    paths: Vec<String>,
    staged: bool,
    /// At least one surviving path is tracked (non-untracked).
    any_tracked: bool,
    /// Every surviving path is untracked.
    all_untracked: bool,
}

/// Filter `candidates` to those present in `git status` and compute the
/// tracked/untracked facts the menu needs.
fn derive_selection(repo: &Repository, candidates: &[String], staged: bool) -> AppResult<Derived> {
    let statuses = status_map(repo)?;

    let mut paths = Vec::new();
    let mut any_tracked = false;
    let mut all_untracked = true;

    for path in candidates {
        let Some(status) = statuses.get(path) else {
            continue; // vanished from status since the frontend snapshot
        };
        if !is_untracked(*status) {
            any_tracked = true;
            all_untracked = false;
        }
        paths.push(path.clone());
    }

    if paths.is_empty() {
        all_untracked = false;
    }

    Ok(Derived {
        paths,
        staged,
        any_tracked,
        all_untracked,
    })
}

/// Build a `path -> Status` snapshot for the whole working tree.
fn status_map(repo: &Repository) -> AppResult<HashMap<String, Status>> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut map = HashMap::new();
    for entry in statuses.iter() {
        if let Some(path) = entry.path() {
            map.insert(path.to_string(), entry.status());
        }
    }
    Ok(map)
}

/// A file is untracked when it is present in the working tree but absent from
/// the index (`WT_NEW`).
const fn is_untracked(status: Status) -> bool {
    status.contains(Status::WT_NEW)
}

/// Construct the native menu for `derived`.
fn build_menu<R: Runtime>(app: &AppHandle<R>, derived: &Derived) -> tauri::Result<Menu<R>> {
    let label = selection_label(&derived.paths);
    let single = derived.paths.len() == 1;

    let mut items: Vec<Box<dyn tauri::menu::IsMenuItem<R>>> = Vec::new();

    if derived.staged {
        items.push(item(app, "ctx:unstage", &format!("Unstage {label}"))?);
        if single {
            items.push(sep(app)?);
            items.push(item(app, "ctx:open", &format!("Open {label}"))?);
            items.push(item(
                app,
                "ctx:reveal",
                &format!("Reveal {label} in Finder"),
            )?);
        }
    } else {
        items.push(item(app, "ctx:stage", &format!("Stage {label}"))?);
        items.push(item(app, "ctx:ignore", &format!("Ignore {label}"))?);
        if derived.any_tracked {
            items.push(item(
                app,
                "ctx:discard",
                &format!("Discard changes to {label}…"),
            )?);
        }
        // The separator only earns its place when at least one item follows it
        // (single-selection Open/Reveal, or Trash for an all-untracked set).
        if single || derived.all_untracked {
            items.push(sep(app)?);
        }
        if single {
            items.push(item(app, "ctx:open", &format!("Open {label}"))?);
            items.push(item(
                app,
                "ctx:reveal",
                &format!("Reveal {label} in Finder"),
            )?);
        }
        if derived.all_untracked {
            items.push(item(app, "ctx:trash", &format!("Move {label} to Trash"))?);
        }
    }

    let refs: Vec<&dyn tauri::menu::IsMenuItem<R>> = items.iter().map(AsRef::as_ref).collect();
    Menu::with_items(app, &refs)
}

fn item<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    text: &str,
) -> tauri::Result<Box<dyn tauri::menu::IsMenuItem<R>>> {
    Ok(Box::new(MenuItem::with_id(
        app,
        id,
        text,
        true,
        None::<&str>,
    )?))
}

fn sep<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Box<dyn tauri::menu::IsMenuItem<R>>> {
    Ok(Box::new(PredefinedMenuItem::separator(app)?))
}

/// The label fragment used after each verb: a quoted basename for a single file,
/// or `N files` for a multi-selection.
fn selection_label(paths: &[String]) -> String {
    if paths.len() == 1 {
        format!("\"{}\"", basename(&paths[0]))
    } else {
        format!("{} files", paths.len())
    }
}

/// Last path component (falls back to the whole string).
fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// Route a `ctx:*` menu event. Called from `menu::handle_menu_event`; ignores
/// any non-context id.
pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if !id.starts_with(CTX_PREFIX) {
        return;
    }
    let Some(action) = CtxAction::from_id(id) else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move { run_action(app, action).await });
}

/// Execute a resolved context-menu action against the pending selection.
// This is a flat per-action dispatch over a git-mutating command surface;
// splitting it would scatter the action match and obscure the sequential flow
// without any behavior benefit, so the length lint is silenced here.
#[allow(clippy::too_many_lines)]
async fn run_action<R: Runtime>(app: AppHandle<R>, action: CtxAction) {
    let pending = {
        let state = app.state::<AppState>();
        let Ok(guard) = state.pending_ctx_menu.lock() else {
            return;
        };
        guard.clone()
    };
    let Some(pending) = pending else { return };
    let window_label = pending.window;
    let paths = pending.paths;
    if paths.is_empty() {
        return;
    }
    // Defensive: ignore an event whose action does not belong to the panel the
    // pending selection came from (guards against a stale menu firing against a
    // newer selection from the other panel).
    if !action.matches_panel(pending.staged) {
        return;
    }

    // Non-mutating actions: no lock, no refresh event. Single-selection only.
    match action {
        CtxAction::Open | CtxAction::Reveal => {
            open_or_reveal(&app, &window_label, action, &paths);
            return;
        }
        _ => {}
    }

    // Discard asks for confirmation before touching anything. Use the async
    // callback dialog (not `blocking_show`): this runs inside a spawned async
    // task, and `blocking_show` would park a tokio worker for the entire time the
    // modal sits open on user think-time. The callback returns immediately and we
    // await the result over a oneshot, yielding the worker meanwhile.
    if action == CtxAction::Discard {
        let label = selection_label(&paths);
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.dialog()
            .message(format!(
                "Discard changes to {label}? This cannot be undone."
            ))
            .title("Discard Changes")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Discard".to_string(),
                "Cancel".to_string(),
            ))
            .show(move |confirmed| {
                let _ = tx.send(confirmed);
            });
        // A dropped sender (dialog closed without firing the callback) is treated
        // as "not confirmed" — never discard on ambiguity.
        if !rx.await.unwrap_or(false) {
            return;
        }
    }

    // Mutating actions serialize on the same write lock as the stage/unstage
    // commands so they cannot interleave index/working-tree writes.
    let state = app.state::<AppState>();
    let _guard = state.write_lock.lock().await;
    let Some(repo_path) = repo_path_for_window(&app, &window_label) else {
        return;
    };

    let outcome = tauri::async_runtime::spawn_blocking(move || -> AppResult<&'static str> {
        let repo = open_repository(&repo_path)?;
        match action {
            CtxAction::Stage => {
                for path in &paths {
                    stage_file(&repo, path)?;
                }
                Ok("index")
            }
            CtxAction::Unstage => {
                for path in &paths {
                    unstage_file(&repo, path)?;
                }
                Ok("index")
            }
            CtxAction::Ignore => {
                let workdir = repo
                    .workdir()
                    .ok_or_else(|| AppError::validation("Repository has no working tree"))?;
                append_gitignore_entries(workdir, &paths)?;
                Ok("fs")
            }
            CtxAction::Discard => {
                let map = status_map(&repo)?;
                for path in &paths {
                    // Only tracked files have working-tree changes to discard;
                    // untracked ones are handled via Move to Trash.
                    if map.get(path).is_some_and(|s| !is_untracked(*s)) {
                        discard_file(&repo, path)?;
                    }
                }
                Ok("fs")
            }
            CtxAction::Trash => {
                let workdir = repo
                    .workdir()
                    .ok_or_else(|| AppError::validation("Repository has no working tree"))?;
                for path in &paths {
                    let full = workdir.join(path);
                    trash::delete(&full)
                        .map_err(|e| AppError::io(format!("Failed to trash '{path}': {e}")))?;
                }
                Ok("fs")
            }
            CtxAction::Open | CtxAction::Reveal => unreachable!("handled before the lock"),
        }
    })
    .await;

    match outcome {
        Ok(Ok(reason)) => {
            // Target only the window whose menu acted, so unrelated repo windows
            // do not needlessly refresh.
            let _ = app.emit_to(&window_label, "repo-changed", RepoChanged { reason });
        }
        Ok(Err(e)) => surface_error(&app, &e.to_string()),
        Err(e) => surface_error(&app, &format!("Context-menu action failed: {e}")),
    }
}

/// Open or reveal each path via the opener plugin (single-selection actions, but
/// we defensively loop in case a multi-selection ever reaches here).
fn open_or_reveal<R: Runtime>(
    app: &AppHandle<R>,
    window_label: &str,
    action: CtxAction,
    paths: &[String],
) {
    let Some(repo_path) = repo_path_for_window(app, window_label) else {
        return;
    };

    for path in paths {
        let full = repo_path.join(path);
        let result = match action {
            CtxAction::Open => app
                .opener()
                .open_path(full.to_string_lossy().to_string(), None::<&str>),
            CtxAction::Reveal => app.opener().reveal_item_in_dir(&full),
            _ => return,
        };
        if let Err(e) = result {
            surface_error(app, &format!("Failed to open '{path}': {e}"));
        }
    }
}

/// Best-effort error surface: a native message dialog off the main thread.
fn surface_error<R: Runtime>(app: &AppHandle<R>, message: &str) {
    let app = app.clone();
    let message = message.to_string();
    std::thread::spawn(move || {
        app.dialog()
            .message(message)
            .title("Action Failed")
            .kind(MessageDialogKind::Error)
            .blocking_show();
    });
}

/// Escape a repo-relative path into a *literal* `.gitignore` pattern body (the
/// caller prepends the anchoring `/`).
///
/// gitignore is glob-based: `* ? [ ]` are wildcards, a leading `!`/`#` marks
/// negation/comment, `\` is the escape char, and unescaped trailing whitespace is
/// stripped. A raw filename containing any of these would match the wrong files
/// (or, for `[`, form a broken pattern). Backslash-escaping them makes the
/// pattern match the file's actual name and nothing else. `!`/`#` are escaped
/// defensively even though our leading `/` keeps them off the line start.
fn escape_gitignore_pattern(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for ch in path.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '!' | '#' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    // Escape any trailing spaces/tabs, which gitignore would otherwise trim.
    let trimmed_len = out.trim_end_matches([' ', '\t']).len();
    if trimmed_len < out.len() {
        let mut tail = String::with_capacity((out.len() - trimmed_len) * 2);
        for c in out[trimmed_len..].chars() {
            tail.push('\\');
            tail.push(c);
        }
        out.truncate(trimmed_len);
        out.push_str(&tail);
    }
    out
}

/// Append repo-relative `rel_paths` to `<workdir>/.gitignore`, each as a
/// leading-slash anchored, glob-escaped pattern on its own line. Creates the file
/// if missing and skips any line that already exists verbatim (dedupe).
///
/// Pure filesystem logic, factored out for direct unit testing.
pub fn append_gitignore_entries(workdir: &Path, rel_paths: &[String]) -> AppResult<()> {
    let gitignore = workdir.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let existing_lines: HashSet<String> = existing.lines().map(str::to_string).collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut to_add: Vec<String> = Vec::new();
    for path in rel_paths {
        let line = format!("/{}", escape_gitignore_pattern(path));
        if existing_lines.contains(&line) || !seen.insert(line.clone()) {
            continue;
        }
        to_add.push(line);
    }
    if to_add.is_empty() {
        return Ok(());
    }

    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    for line in &to_add {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(&gitignore, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_file, setup};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn selection_label_single_and_multi() {
        assert_eq!(selection_label(&["src/main.rs".to_string()]), "\"main.rs\"");
        assert_eq!(
            selection_label(&[
                "a.txt".to_string(),
                "b.txt".to_string(),
                "c.txt".to_string()
            ]),
            "3 files"
        );
    }

    #[test]
    fn gitignore_created_with_leading_slash() {
        let dir = tempdir().unwrap();
        append_gitignore_entries(dir.path(), &["build/out.o".to_string()]).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(content, "/build/out.o\n");
    }

    #[test]
    fn gitignore_appends_and_dedupes() {
        let dir = tempdir().unwrap();
        let gitignore = dir.path().join(".gitignore");
        // Pre-existing content without a trailing newline, plus an entry that
        // must not be duplicated.
        fs::write(&gitignore, "node_modules\n/keep.txt").unwrap();

        append_gitignore_entries(
            dir.path(),
            &[
                "keep.txt".to_string(), // already present as `/keep.txt` -> skip
                "dist/app.js".to_string(),
                "dist/app.js".to_string(), // duplicate within the batch -> once
            ],
        )
        .unwrap();

        let content = fs::read_to_string(&gitignore).unwrap();
        assert_eq!(content, "node_modules\n/keep.txt\n/dist/app.js\n");
    }

    #[test]
    fn gitignore_escapes_glob_metacharacters() {
        let dir = tempdir().unwrap();
        append_gitignore_entries(dir.path(), &["src/[weird]*?.txt".to_string()]).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(content, "/src/\\[weird\\]\\*\\?.txt\n");
    }

    #[test]
    fn gitignore_escapes_leading_bang_and_hash_and_backslash() {
        let dir = tempdir().unwrap();
        append_gitignore_entries(dir.path(), &["!weird#\\name".to_string()]).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(content, "/\\!weird\\#\\\\name\n");
    }

    #[test]
    fn gitignore_escapes_trailing_whitespace() {
        let dir = tempdir().unwrap();
        // Internal space stays literal; only the trailing space is escaped so
        // gitignore does not strip it.
        append_gitignore_entries(dir.path(), &["weird name ".to_string()]).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(content, "/weird name\\ \n");
    }

    #[test]
    fn gitignore_noop_when_all_present() {
        let dir = tempdir().unwrap();
        let gitignore = dir.path().join(".gitignore");
        fs::write(&gitignore, "/a.txt\n/b.txt\n").unwrap();

        append_gitignore_entries(dir.path(), &["a.txt".to_string(), "b.txt".to_string()]).unwrap();

        let content = fs::read_to_string(&gitignore).unwrap();
        assert_eq!(content, "/a.txt\n/b.txt\n", "no duplicate lines appended");
    }

    // ---- basename (pure menu-label helper) ----

    #[test]
    fn basename_takes_last_component_and_falls_back() {
        assert_eq!(basename("src/git/mod.rs"), "mod.rs");
        assert_eq!(basename("top.txt"), "top.txt");
    }

    // ---- CtxAction::from_id ----

    #[test]
    fn from_id_maps_every_known_id_and_rejects_the_rest() {
        assert_eq!(CtxAction::from_id("ctx:stage"), Some(CtxAction::Stage));
        assert_eq!(CtxAction::from_id("ctx:unstage"), Some(CtxAction::Unstage));
        assert_eq!(CtxAction::from_id("ctx:ignore"), Some(CtxAction::Ignore));
        assert_eq!(CtxAction::from_id("ctx:discard"), Some(CtxAction::Discard));
        assert_eq!(CtxAction::from_id("ctx:trash"), Some(CtxAction::Trash));
        assert_eq!(CtxAction::from_id("ctx:open"), Some(CtxAction::Open));
        assert_eq!(CtxAction::from_id("ctx:reveal"), Some(CtxAction::Reveal));
        // Unknown suffix and an un-prefixed id both resolve to None.
        assert_eq!(CtxAction::from_id("ctx:bogus"), None);
        assert_eq!(CtxAction::from_id("stage"), None);
    }

    // ---- CtxAction::matches_panel (staged/unstaged gating) ----

    #[test]
    fn matches_panel_staged_allows_only_unstage_open_reveal() {
        // The staged panel rejects the unstaged-only actions.
        assert!(CtxAction::Unstage.matches_panel(true));
        assert!(!CtxAction::Stage.matches_panel(true));
        assert!(!CtxAction::Discard.matches_panel(true));
        assert!(!CtxAction::Trash.matches_panel(true));
        assert!(!CtxAction::Ignore.matches_panel(true));
        // Open/Reveal are valid from either panel.
        assert!(CtxAction::Open.matches_panel(true));
        assert!(CtxAction::Reveal.matches_panel(true));
    }

    #[test]
    fn matches_panel_unstaged_allows_everything_but_unstage() {
        assert!(!CtxAction::Unstage.matches_panel(false));
        assert!(CtxAction::Stage.matches_panel(false));
        assert!(CtxAction::Discard.matches_panel(false));
        assert!(CtxAction::Trash.matches_panel(false));
        assert!(CtxAction::Ignore.matches_panel(false));
        assert!(CtxAction::Open.matches_panel(false));
        assert!(CtxAction::Reveal.matches_panel(false));
    }

    // ---- derive_selection against a real temp repo ----

    #[test]
    fn derive_selection_marks_tracked_modified_as_tracked() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "one\n");
        // Working-tree modification of a tracked file → WT_MODIFIED, not WT_NEW.
        fs::write(dir.path().join("a.txt"), "two\n").unwrap();

        let derived = derive_selection(&repo, &["a.txt".to_string()], false).unwrap();

        assert_eq!(derived.paths, vec!["a.txt".to_string()]);
        assert!(derived.any_tracked);
        assert!(!derived.all_untracked);
    }

    #[test]
    fn derive_selection_marks_untracked_only_set_as_all_untracked() {
        let (dir, repo) = setup();
        fs::write(dir.path().join("u.txt"), "new\n").unwrap();

        let derived = derive_selection(&repo, &["u.txt".to_string()], false).unwrap();

        assert_eq!(derived.paths, vec!["u.txt".to_string()]);
        assert!(!derived.any_tracked);
        assert!(derived.all_untracked);
    }

    #[test]
    fn derive_selection_mixed_set_is_tracked_and_not_all_untracked() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "one\n");
        fs::write(dir.path().join("a.txt"), "two\n").unwrap(); // tracked-modified
        fs::write(dir.path().join("u.txt"), "new\n").unwrap(); // untracked

        let derived =
            derive_selection(&repo, &["a.txt".to_string(), "u.txt".to_string()], false).unwrap();

        assert_eq!(derived.paths.len(), 2);
        assert!(derived.any_tracked);
        assert!(!derived.all_untracked);
    }

    #[test]
    fn derive_selection_drops_paths_that_vanished_from_status() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "one\n");
        fs::write(dir.path().join("a.txt"), "two\n").unwrap();

        // "ghost.txt" is clean/absent, so it has no status entry and is dropped;
        // only the surviving tracked-modified a.txt remains.
        let derived = derive_selection(
            &repo,
            &["a.txt".to_string(), "ghost.txt".to_string()],
            false,
        )
        .unwrap();

        assert_eq!(derived.paths, vec!["a.txt".to_string()]);
        assert!(derived.any_tracked);
        assert!(!derived.all_untracked);
    }
}
