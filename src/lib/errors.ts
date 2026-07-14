/**
 * Normalization for Tauri command rejections into the structured {@link AppError}
 * discriminated union.
 *
 * Every `#[tauri::command]` now returns `Result<T, AppError>`, so a rejected
 * `invoke(...)` promise carries the serialized `{ name, message }` object. This
 * module is the single boundary that turns an `unknown` rejection into a typed
 * `AppError` the store can `switch (err.name)` on — instead of the old, fragile
 * `message.includes("...")` string matching.
 *
 * Anything that is not a well-formed `AppError` (a legacy string rejection, a raw
 * JS `Error` thrown before the IPC boundary, a dialog-plugin failure) is folded
 * into the generic `git` catch-all with its text preserved for display.
 */
import type { AppError, AppErrorName } from "../types/ipc";

/** Every valid discriminant tag, mirroring the Rust enum (see `ipc.ts`). */
const APP_ERROR_NAMES: ReadonlySet<AppErrorName> = new Set([
  "noRepoOpen",
  "staleHunk",
  "nonUtf8File",
  "identityMissing",
  "nothingStaged",
  "emptyMessage",
  "windowClosed",
  "validation",
  "git",
  "io",
]);

/** Whether `value` is already a well-formed structured `AppError`. */
function isAppError(value: unknown): value is AppError {
  return (
    typeof value === "object" &&
    value !== null &&
    "name" in value &&
    "message" in value &&
    typeof (value as { name: unknown }).name === "string" &&
    typeof (value as { message: unknown }).message === "string" &&
    APP_ERROR_NAMES.has((value as { name: string }).name as AppErrorName)
  );
}

/** Extract a displayable message from any non-`AppError` rejection value. */
function messageOf(value: unknown): string {
  if (typeof value === "string") return value;
  if (value instanceof Error) return value.message;
  return String(value);
}

/**
 * Normalize any thrown/rejected value into a structured {@link AppError}.
 *
 * A serialized command rejection passes through unchanged; everything else
 * (legacy strings, `Error` instances, unexpected shapes) becomes the generic
 * `git` variant with its message preserved.
 */
export function toAppError(e: unknown): AppError {
  if (isAppError(e)) return e;
  return { name: "git", message: messageOf(e) };
}

/**
 * Whether a rejection is the expected "no repository bound to this window" state
 * (a plain launch with nothing to restore), which the UI renders silently rather
 * than as a failure. A one-line discriminant check on the normalized error.
 */
export function isNoRepoError(e: unknown): boolean {
  return toAppError(e).name === "noRepoOpen";
}
