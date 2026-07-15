use std::path::PathBuf;

use serde::Serialize;
use tauri::{State, WebviewWindow};

use crate::error::{AppError, AppResult};
use crate::git::{
    apply_hunk_verified, build_status, commit as git_commit, file_diff, head_commit_message,
    open_repository, validate_repo_relative_path, ApplyTarget, FileDiff, HunkPatchPayload,
    RepoStatus,
};
use crate::image::{read_image_data, ImageData, MAX_IMAGE_BYTES};
use crate::logging::{Timer, T_CMD};
use crate::state::AppState;

/// A `get_diff` result carrying more than this many total diff lines is logged
/// at `warn` — an oversized payload is a memory-pressure suspect.
const DIFF_LINES_WARN: usize = 20_000;

/// Result of a successful commit: the new commit's hex oid.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub oid: String,
}

/// Clone the repository path bound to `window`, erroring if none is open there.
///
/// Every repo-scoped command resolves its repository this way: Tauri injects the
/// calling `window`, and each window shows its own repository (keyed by label in
/// [`AppState::windows`]). JS call sites are unchanged — the window is not part of
/// the argument payload.
fn window_repo_path(state: &State<'_, AppState>, window: &WebviewWindow) -> AppResult<PathBuf> {
    let windows = state
        .windows
        .lock()
        .map_err(|_| AppError::git("Internal state lock poisoned"))?;
    windows
        .get(window.label())
        .map(|repo| repo.repo_path.clone())
        .ok_or_else(AppError::no_repo_open)
}

/// Validate `path` is a git repository, bind it to the calling window, and return
/// its current status. (Re)starts that window's filesystem watcher.
#[tauri::command]
pub async fn open_repo(
    path: String,
    app: tauri::AppHandle,
    window: WebviewWindow,
    _state: State<'_, AppState>,
) -> AppResult<RepoStatus> {
    let path_buf = PathBuf::from(&path);
    // Run all libgit2 work off the async runtime (M3): resolve the workdir, open
    // the repo, and read status inside the blocking closure. `Repository` is not
    // `Send`, so it is created and dropped entirely within the closure; only the
    // `Send` results cross back.
    let (workdir, status) =
        tauri::async_runtime::spawn_blocking(move || -> AppResult<(PathBuf, RepoStatus)> {
            let workdir = crate::windows::resolve_workdir(&path_buf)?;
            let repo = open_repository(&workdir)?;
            let status = build_status(&repo)?;
            drop(repo);
            Ok((workdir, status))
        })
        .await
        .map_err(|e| AppError::git(format!("Failed to run open task: {e}")))??;

    // Guarded bind: if this window was closed while the open ran, do not
    // resurrect a ghost binding (M2).
    crate::windows::set_window_repo(&app, window.label(), workdir)?;
    crate::windows::persist_open_repos(&app);

    log::info!(
        target: T_CMD,
        "open_repo: label={} path={path:?} unstaged={} staged={}",
        window.label(), status.unstaged.len(), status.staged.len()
    );
    Ok(status)
}

/// Return the status of the calling window's repository, or an error if none.
#[tauri::command]
pub async fn get_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<RepoStatus> {
    let timer = Timer::start();
    let path = window_repo_path(&state, &window)?;
    // libgit2 off the async runtime (M3).
    let status = tauri::async_runtime::spawn_blocking(move || -> AppResult<RepoStatus> {
        let repo = open_repository(&path)?;
        build_status(&repo)
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run status task: {e}")))?;
    match &status {
        Ok(s) => log::debug!(
            target: T_CMD,
            "get_status: label={} unstaged={} staged={} in {}ms",
            window.label(), s.unstaged.len(), s.staged.len(), timer.ms()
        ),
        Err(e) => {
            log::warn!(target: T_CMD, "get_status failed: label={} err={e} in {}ms", window.label(), timer.ms());
        }
    }
    status
}

/// Return the diff of a single file in the calling window's repository.
///
/// `staged = false` diffs index-vs-workdir; `staged = true` diffs HEAD-vs-index.
/// `context_lines` (0-8) controls the number of surrounding context lines.
#[tauri::command]
pub async fn get_diff(
    path: String,
    staged: bool,
    context_lines: u32,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<FileDiff> {
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path)?;

    // Defense in depth: clamp the client-supplied context to the UI's 0..=8 range
    // so a hostile/buggy payload cannot request an enormous context window.
    // (`u32` floors at 0, so only the upper bound needs clamping.)
    let context_lines = context_lines.min(8);

    let timer = Timer::start();
    let log_path = path.clone();
    // libgit2 off the async runtime (M3).
    let diff = tauri::async_runtime::spawn_blocking(move || -> AppResult<FileDiff> {
        let repo = open_repository(&repo_path)?;
        file_diff(&repo, &path, staged, context_lines)
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run diff task: {e}")))?;
    match &diff {
        Ok(d) => {
            let total_lines: usize = d.hunks.iter().map(|h| h.lines.len()).sum();
            // An oversized diff is a memory suspect; surface it at warn.
            if total_lines > DIFF_LINES_WARN {
                log::warn!(
                    target: T_CMD,
                    "get_diff LARGE: path={log_path:?} staged={staged} ctx={context_lines} hunks={} lines={total_lines} binary={} lossy={} in {}ms",
                    d.hunks.len(), d.is_binary, d.is_lossy, timer.ms()
                );
            } else {
                log::debug!(
                    target: T_CMD,
                    "get_diff: path={log_path:?} staged={staged} ctx={context_lines} hunks={} lines={total_lines} binary={} lossy={} in {}ms",
                    d.hunks.len(), d.is_binary, d.is_lossy, timer.ms()
                );
            }
        }
        Err(e) => {
            log::warn!(target: T_CMD, "get_diff failed: path={log_path:?} staged={staged} err={e} in {}ms", timer.ms());
        }
    }
    diff
}

/// Log the outcome of a whole-file mutating command uniformly.
fn log_file_action(action: &str, path: &str, result: &AppResult<()>, ms: u128) {
    match result {
        Ok(()) => log::debug!(target: T_CMD, "{action}: path={path:?} ok in {ms}ms"),
        Err(e) => log::warn!(target: T_CMD, "{action} failed: path={path:?} err={e} in {ms}ms"),
    }
}

/// Stage the whole-file change for `path` in the calling window's repository.
#[tauri::command]
pub async fn stage_file(
    path: String,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path)?;

    let timer = Timer::start();
    let log_path = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        let repo = open_repository(&repo_path)?;
        crate::git::stage_file(&repo, &path)?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run stage task: {e}")))?;
    log_file_action("stage_file", &log_path, &result, timer.ms());
    result
}

/// Unstage the whole-file change for `path`, resetting it back to HEAD.
#[tauri::command]
pub async fn unstage_file(
    path: String,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path)?;

    let timer = Timer::start();
    let log_path = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        let repo = open_repository(&repo_path)?;
        crate::git::unstage_file(&repo, &path)?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run unstage task: {e}")))?;
    log_file_action("unstage_file", &log_path, &result, timer.ms());
    result
}

/// Discard the working-tree changes for `path` (or delete it if untracked).
#[tauri::command]
pub async fn discard_file(
    path: String,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path)?;

    let timer = Timer::start();
    let log_path = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        let repo = open_repository(&repo_path)?;
        crate::git::discard_file(&repo, &path)?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run discard task: {e}")))?;
    log_file_action("discard_file", &log_path, &result, timer.ms());
    result
}

/// Which hunk command is validating its payload direction (see
/// [`validate_hunk_direction`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HunkCommand {
    Stage,
    Unstage,
    Discard,
}

/// Validate that a hunk payload's `staged`/`is_untracked` flags match the command
/// it was sent to, returning the [`ApplyTarget`] to apply with.
///
/// Extracted from the three hunk commands so the direction guards are unit-
/// testable in isolation (the inline versions were not). The freshness re-check
/// in `apply_hunk_verified` still runs afterward — these flags are advisory — but
/// rejecting an obviously wrong direction up front gives the caller a precise
/// error and avoids a pointless re-diff.
fn validate_hunk_direction(
    payload: &HunkPatchPayload,
    command: HunkCommand,
) -> AppResult<ApplyTarget> {
    match command {
        HunkCommand::Stage => {
            if payload.staged {
                return Err(AppError::validation(
                    "stage_hunk expects a hunk from the unstaged diff",
                ));
            }
            if payload.is_untracked {
                return Err(AppError::validation(
                    "Cannot hunk-stage an untracked file; use stage_file to stage it whole instead",
                ));
            }
            Ok(ApplyTarget::Index)
        }
        HunkCommand::Unstage => {
            if !payload.staged {
                return Err(AppError::validation(
                    "unstage_hunk expects a hunk from the staged diff",
                ));
            }
            Ok(ApplyTarget::IndexReverse)
        }
        HunkCommand::Discard => {
            // Order matches the original inline guard: reject untracked before
            // the staged-direction check.
            if payload.is_untracked {
                return Err(AppError::validation(
                    "Cannot discard a hunk of an untracked file; use discard_file to delete it instead",
                ));
            }
            if payload.staged {
                return Err(AppError::validation(
                    "discard_hunk expects a hunk from the unstaged diff",
                ));
            }
            Ok(ApplyTarget::WorkdirReverse)
        }
    }
}

/// Stage a single hunk (payload taken from the *unstaged* diff) into the index.
#[tauri::command]
pub async fn stage_hunk(
    payload: HunkPatchPayload,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let target = validate_hunk_direction(&payload, HunkCommand::Stage)?;
    run_hunk(&state, &window, payload, target).await
}

/// Unstage a single hunk (payload taken from the *staged* diff) from the index.
#[tauri::command]
pub async fn unstage_hunk(
    payload: HunkPatchPayload,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let target = validate_hunk_direction(&payload, HunkCommand::Unstage)?;
    run_hunk(&state, &window, payload, target).await
}

/// Discard a single hunk (payload taken from the *unstaged* diff) from the
/// working tree.
///
/// Untracked files have no partial-discard semantics (there is no old blob to
/// revert to); the frontend routes those to `discard_file`. If such a payload
/// still arrives here, fail loudly telling the caller which command to use.
#[tauri::command]
pub async fn discard_hunk(
    payload: HunkPatchPayload,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let target = validate_hunk_direction(&payload, HunkCommand::Discard)?;
    run_hunk(&state, &window, payload, target).await
}

/// Shared driver for the three hunk commands: serialize on the write lock,
/// resolve the window's repository working tree, then hand off to
/// [`apply_hunk_verified`] off the async runtime (it re-diffs, verifies freshness
/// and shells to `git apply`).
///
/// The freshness re-verification (re-running `file_diff` and requiring the hunk
/// to still exist verbatim) happens inside `apply_hunk_verified`, so the
/// frontend's `staged` / `is_untracked` flags are advisory only. `apply_hunk_verified`
/// surfaces a `StaleHunk` variant for "changed since displayed" and a `Git` variant
/// carrying git's stderr ("patch does not apply") — both reach the frontend intact.
async fn run_hunk(
    state: &State<'_, AppState>,
    window: &WebviewWindow,
    payload: HunkPatchPayload,
    target: ApplyTarget,
) -> AppResult<()> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(state, window)?;

    // Validate up front (defense in depth; `apply_hunk_verified` re-checks).
    validate_repo_relative_path(&repo_path, &payload.path)?;
    if let Some(old) = payload.old_path.as_deref() {
        validate_repo_relative_path(&repo_path, old)?;
    }

    let repo = open_repository(&repo_path)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| AppError::validation("Repository has no working tree"))?
        .to_path_buf();
    // `Repository` is not `Send`; drop it before crossing the blocking boundary.
    // `apply_hunk_verified` re-opens its own handle inside the closure.
    drop(repo);

    let timer = Timer::start();
    let log_path = payload.path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        apply_hunk_verified(&workdir, &payload, target)
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run hunk-apply task: {e}")))?;
    match &result {
        Ok(()) => {
            log::debug!(target: T_CMD, "hunk {target:?}: path={log_path:?} ok in {}ms", timer.ms());
        }
        Err(e) => {
            log::warn!(target: T_CMD, "hunk {target:?} failed: path={log_path:?} err={e} in {}ms", timer.ms());
        }
    }
    result
}

/// Create a commit from the current index (or amend HEAD when `amend`).
///
/// Serializes on the write lock like the other index mutations so a commit
/// cannot interleave with an in-flight stage/unstage.
#[tauri::command]
pub async fn commit(
    message: String,
    amend: bool,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<CommitResult> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    let timer = Timer::start();
    // libgit2 off the async runtime (M3); the write lock is held across the await
    // so the commit still serializes against other index mutations.
    let result = tauri::async_runtime::spawn_blocking(move || -> AppResult<String> {
        let repo = open_repository(&repo_path)?;
        git_commit(&repo, &message, amend)
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run commit task: {e}")))?;
    match &result {
        Ok(oid) => {
            log::info!(target: T_CMD, "commit: label={} amend={amend} oid={oid} in {}ms", window.label(), timer.ms());
        }
        Err(e) => {
            log::warn!(target: T_CMD, "commit failed: label={} amend={amend} err={e} in {}ms", window.label(), timer.ms());
        }
    }
    Ok(CommitResult { oid: result? })
}

/// Return HEAD commit's full message (used to prefill the amend editor), or an
/// empty string on an unborn HEAD.
#[tauri::command]
pub async fn get_head_commit_message(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<String> {
    let repo_path = window_repo_path(&state, &window)?;
    // libgit2 off the async runtime (M3).
    tauri::async_runtime::spawn_blocking(move || -> AppResult<String> {
        let repo = open_repository(&repo_path)?;
        head_commit_message(&repo)
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run head-message task: {e}")))?
}

/// Read an image file (working tree or staged index blob) and return it
/// base64-encoded with a MIME type for inline preview.
///
/// Read-only: no write lock. `staged = false` reads the working-tree bytes;
/// `staged = true` reads the blob recorded in the index. The extension must be
/// in the image allow-list and the payload is capped at 20 MB.
#[tauri::command]
pub async fn read_image(
    path: String,
    staged: bool,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> AppResult<ImageData> {
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path)?;

    let timer = Timer::start();
    let log_path = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> AppResult<ImageData> {
        let repo = open_repository(&repo_path)?;
        read_image_data(&repo, &path, staged, MAX_IMAGE_BYTES)
    })
    .await
    .map_err(|e| AppError::git(format!("Failed to run read_image task: {e}")))?;
    match &result {
        // `base64.len()` is the payload size crossing the IPC bridge (~4/3 the raw
        // image bytes) — the memory-relevant number for a preview.
        Ok(data) => log::debug!(
            target: T_CMD,
            "read_image: path={log_path:?} staged={staged} mime={} b64_len={} in {}ms",
            data.mime_type, data.base64.len(), timer.ms()
        ),
        Err(e) => {
            log::warn!(target: T_CMD, "read_image failed: path={log_path:?} staged={staged} err={e} in {}ms", timer.ms());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal payload with the two flags that drive direction validation.
    fn payload(staged: bool, is_untracked: bool) -> HunkPatchPayload {
        HunkPatchPayload {
            path: "file.txt".to_string(),
            old_path: None,
            staged,
            is_untracked,
            context_lines: 3,
            header: "@@ -1,1 +1,1 @@".to_string(),
            lines: Vec::new(),
        }
    }

    // ---- Stage ----

    #[test]
    fn stage_accepts_unstaged_tracked_hunk() {
        let target = validate_hunk_direction(&payload(false, false), HunkCommand::Stage);
        assert!(matches!(target, Ok(ApplyTarget::Index)));
    }

    #[test]
    fn stage_rejects_a_staged_hunk() {
        let err = validate_hunk_direction(&payload(true, false), HunkCommand::Stage).unwrap_err();
        assert!(err.to_string().contains("unstaged diff"), "{err}");
    }

    #[test]
    fn stage_rejects_an_untracked_hunk() {
        let err = validate_hunk_direction(&payload(false, true), HunkCommand::Stage).unwrap_err();
        assert!(err.to_string().contains("stage_file"), "{err}");
    }

    // ---- Unstage ----

    #[test]
    fn unstage_accepts_a_staged_hunk() {
        let target = validate_hunk_direction(&payload(true, false), HunkCommand::Unstage);
        assert!(matches!(target, Ok(ApplyTarget::IndexReverse)));
    }

    #[test]
    fn unstage_rejects_an_unstaged_hunk() {
        let err =
            validate_hunk_direction(&payload(false, false), HunkCommand::Unstage).unwrap_err();
        assert!(err.to_string().contains("staged diff"), "{err}");
    }

    // ---- Discard ----

    #[test]
    fn discard_accepts_unstaged_tracked_hunk() {
        let target = validate_hunk_direction(&payload(false, false), HunkCommand::Discard);
        assert!(matches!(target, Ok(ApplyTarget::WorkdirReverse)));
    }

    #[test]
    fn discard_rejects_an_untracked_hunk_before_checking_direction() {
        // Untracked takes precedence even when also staged.
        let err = validate_hunk_direction(&payload(true, true), HunkCommand::Discard).unwrap_err();
        assert!(err.to_string().contains("discard_file"), "{err}");
    }

    #[test]
    fn discard_rejects_a_staged_hunk() {
        let err = validate_hunk_direction(&payload(true, false), HunkCommand::Discard).unwrap_err();
        assert!(err.to_string().contains("unstaged diff"), "{err}");
    }
}
