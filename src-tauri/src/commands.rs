use std::path::PathBuf;

use serde::Serialize;
use tauri::{State, WebviewWindow};

use crate::git::{
    apply_hunk_verified, build_status, commit as git_commit, file_diff, head_commit_message,
    open_repository, validate_repo_relative_path, ApplyTarget, FileDiff, HunkPatchPayload,
    RepoStatus,
};
use crate::image::{read_image_data, ImageData, MAX_IMAGE_BYTES};
use crate::state::AppState;

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
fn window_repo_path(
    state: &State<'_, AppState>,
    window: &WebviewWindow,
) -> Result<PathBuf, String> {
    let windows = state
        .windows
        .lock()
        .map_err(|_| "Internal state lock poisoned".to_string())?;
    windows
        .get(window.label())
        .map(|repo| repo.repo_path.clone())
        .ok_or_else(|| "No repository open".to_string())
}

/// Validate `path` is a git repository, bind it to the calling window, and return
/// its current status. (Re)starts that window's filesystem watcher.
#[tauri::command]
pub async fn open_repo(
    path: String,
    app: tauri::AppHandle,
    window: WebviewWindow,
    _state: State<'_, AppState>,
) -> Result<RepoStatus, String> {
    let workdir = crate::windows::resolve_workdir(&PathBuf::from(&path)).map_err(String::from)?;
    let repo = open_repository(&workdir)?;
    let status = build_status(&repo)?;
    // `Repository` is not `Send`; drop it before touching shared state / awaits.
    drop(repo);

    crate::windows::set_window_repo(&app, window.label(), workdir);
    crate::windows::persist_open_repos(&app);

    Ok(status)
}

/// Return the status of the calling window's repository, or an error if none.
#[tauri::command]
pub async fn get_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<RepoStatus, String> {
    let path = window_repo_path(&state, &window)?;
    let repo = open_repository(&path)?;
    Ok(build_status(&repo)?)
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
) -> Result<FileDiff, String> {
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path).map_err(String::from)?;

    let repo = open_repository(&repo_path)?;
    Ok(file_diff(&repo, &path, staged, context_lines)?)
}

/// Stage the whole-file change for `path` in the calling window's repository.
#[tauri::command]
pub async fn stage_file(
    path: String,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path).map_err(String::from)?;

    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let repo = open_repository(&repo_path)?;
        crate::git::stage_file(&repo, &path)?;
        Ok(())
    })
    .await
    .map_err(|e| format!("Failed to run stage task: {e}"))?
}

/// Unstage the whole-file change for `path`, resetting it back to HEAD.
#[tauri::command]
pub async fn unstage_file(
    path: String,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path).map_err(String::from)?;

    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let repo = open_repository(&repo_path)?;
        crate::git::unstage_file(&repo, &path)?;
        Ok(())
    })
    .await
    .map_err(|e| format!("Failed to run unstage task: {e}"))?
}

/// Discard the working-tree changes for `path` (or delete it if untracked).
#[tauri::command]
pub async fn discard_file(
    path: String,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path).map_err(String::from)?;

    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let repo = open_repository(&repo_path)?;
        crate::git::discard_file(&repo, &path)?;
        Ok(())
    })
    .await
    .map_err(|e| format!("Failed to run discard task: {e}"))?
}

/// Stage a single hunk (payload taken from the *unstaged* diff) into the index.
#[tauri::command]
pub async fn stage_hunk(
    payload: HunkPatchPayload,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if payload.staged {
        return Err("stage_hunk expects a hunk from the unstaged diff".to_string());
    }
    if payload.is_untracked {
        return Err(
            "Cannot hunk-stage an untracked file; use stage_file to stage it whole instead"
                .to_string(),
        );
    }
    run_hunk(&state, &window, payload, ApplyTarget::Index).await
}

/// Unstage a single hunk (payload taken from the *staged* diff) from the index.
#[tauri::command]
pub async fn unstage_hunk(
    payload: HunkPatchPayload,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if !payload.staged {
        return Err("unstage_hunk expects a hunk from the staged diff".to_string());
    }
    run_hunk(&state, &window, payload, ApplyTarget::IndexReverse).await
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
) -> Result<(), String> {
    if payload.is_untracked {
        return Err(
            "Cannot discard a hunk of an untracked file; use discard_file to delete it instead"
                .to_string(),
        );
    }
    if payload.staged {
        return Err("discard_hunk expects a hunk from the unstaged diff".to_string());
    }
    run_hunk(&state, &window, payload, ApplyTarget::WorkdirReverse).await
}

/// Shared driver for the three hunk commands: serialize on the write lock,
/// resolve the window's repository working tree, then hand off to
/// [`apply_hunk_verified`] off the async runtime (it re-diffs, verifies freshness
/// and shells to `git apply`).
///
/// The freshness re-verification (re-running `file_diff` and requiring the hunk
/// to still exist verbatim) happens inside `apply_hunk_verified`, so the
/// frontend's `staged` / `is_untracked` flags are advisory only. git's stderr
/// rides `AppError` -> `String`, so "patch does not apply" and the
/// "changed since displayed" message reach the frontend intact.
async fn run_hunk(
    state: &State<'_, AppState>,
    window: &WebviewWindow,
    payload: HunkPatchPayload,
    target: ApplyTarget,
) -> Result<(), String> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(state, window)?;

    // Validate up front (defense in depth; `apply_hunk_verified` re-checks).
    validate_repo_relative_path(&repo_path, &payload.path).map_err(String::from)?;
    if let Some(old) = payload.old_path.as_deref() {
        validate_repo_relative_path(&repo_path, old).map_err(String::from)?;
    }

    let repo = open_repository(&repo_path)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| "Repository has no working tree".to_string())?
        .to_path_buf();
    // `Repository` is not `Send`; drop it before crossing the blocking boundary.
    // `apply_hunk_verified` re-opens its own handle inside the closure.
    drop(repo);

    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        apply_hunk_verified(&workdir, &payload, target).map_err(String::from)
    })
    .await
    .map_err(|e| format!("Failed to run hunk-apply task: {e}"))?
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
) -> Result<CommitResult, String> {
    let _guard = state.write_lock.lock().await;
    let repo_path = window_repo_path(&state, &window)?;
    let repo = open_repository(&repo_path)?;
    let oid = git_commit(&repo, &message, amend)?;
    Ok(CommitResult { oid })
}

/// Return HEAD commit's full message (used to prefill the amend editor), or an
/// empty string on an unborn HEAD.
#[tauri::command]
pub async fn get_head_commit_message(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let repo_path = window_repo_path(&state, &window)?;
    let repo = open_repository(&repo_path)?;
    Ok(head_commit_message(&repo)?)
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
) -> Result<ImageData, String> {
    let repo_path = window_repo_path(&state, &window)?;
    validate_repo_relative_path(&repo_path, &path).map_err(String::from)?;

    tauri::async_runtime::spawn_blocking(move || -> Result<ImageData, String> {
        let repo = open_repository(&repo_path)?;
        Ok(read_image_data(&repo, &path, staged, MAX_IMAGE_BYTES)?)
    })
    .await
    .map_err(|e| format!("Failed to run read_image task: {e}"))?
}
