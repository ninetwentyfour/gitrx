import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { FileDiff, HunkPatchPayload, RepoStatus } from "../types/ipc";

/** Open a git repository at the given filesystem path and return its status. */
export function openRepo(path: string): Promise<RepoStatus> {
  return invoke<RepoStatus>("open_repo", { path });
}

/**
 * Fetch the status of the currently open repository.
 * Rejects with a structured `AppError` whose `name` is `"noRepoOpen"` when no
 * repo is bound to this window (see `src/lib/errors.ts`).
 */
export function getStatus(): Promise<RepoStatus> {
  return invoke<RepoStatus>("get_status");
}

/**
 * Fetch the unified diff for a single file.
 * @param path repository-relative file path
 * @param staged whether to diff the staged (index) or unstaged (working tree) version
 * @param contextLines number of surrounding context lines per hunk (0–8)
 */
export function getDiff(path: string, staged: boolean, contextLines: number): Promise<FileDiff> {
  return invoke<FileDiff>("get_diff", { path, staged, contextLines });
}

/** Stage the whole-file change for `path` in the active repository. */
export function stageFile(path: string): Promise<void> {
  return invoke<void>("stage_file", { path });
}

/** Unstage the whole-file change for `path`, resetting it back to HEAD. */
export function unstageFile(path: string): Promise<void> {
  return invoke<void>("unstage_file", { path });
}

/** Discard the working-tree changes for `path` (or delete it if untracked). */
export function discardFile(path: string): Promise<void> {
  return invoke<void>("discard_file", { path });
}

/**
 * Ask the backend to show a native context menu for the given selected files.
 * The backend performs any chosen action itself and then emits `repo-changed`;
 * the frontend only fires the request (and surfaces a failure to open the menu).
 */
export function showFileContextMenu(paths: string[], staged: boolean): Promise<void> {
  return invoke<void>("show_file_context_menu", { paths, staged });
}

/**
 * Read an image blob for preview. Unstaged reads the working-tree version;
 * staged reads the index (blob) version. Resolves to the MIME type and the
 * base64-encoded bytes for a `data:` URL.
 */
export function readImage(
  path: string,
  staged: boolean,
): Promise<{ mimeType: string; base64: string }> {
  return invoke<{ mimeType: string; base64: string }>("read_image", { path, staged });
}

/** Stage a single hunk (payload taken from the unstaged diff) into the index. */
export function stageHunk(payload: HunkPatchPayload): Promise<void> {
  return invoke<void>("stage_hunk", { payload });
}

/** Unstage a single hunk (payload taken from the staged diff) from the index. */
export function unstageHunk(payload: HunkPatchPayload): Promise<void> {
  return invoke<void>("unstage_hunk", { payload });
}

/** Discard a single hunk (payload taken from the unstaged diff) from the working tree. */
export function discardHunk(payload: HunkPatchPayload): Promise<void> {
  return invoke<void>("discard_hunk", { payload });
}

/**
 * Create a commit from the current index (or amend HEAD when `amend`).
 * Resolves to the new commit's hex oid.
 */
export function commit(message: string, amend: boolean): Promise<{ oid: string }> {
  return invoke<{ oid: string }>("commit", { message, amend });
}

/** Fetch HEAD commit's full message (to prefill the amend editor); "" if unborn. */
export function getHeadCommitMessage(): Promise<string> {
  return invoke<string>("get_head_commit_message");
}

/**
 * Prompt the user to pick a repository folder.
 * Resolves to the selected absolute path, or null if the picker was cancelled.
 */
export async function pickRepoFolder(): Promise<string | null> {
  const selection = await open({ directory: true });
  if (typeof selection === "string") return selection;
  return null;
}
