import type { FileDiff, Hunk, HunkPatchPayload } from "../types/ipc";

/**
 * Map a `FileDiff` + one of its `Hunk`s into the `HunkPatchPayload` the backend
 * patch builder expects.
 *
 * Line `content` is copied VERBATIM — including any trailing `\r` — so CRLF hunks
 * round-trip byte-exactly through `git apply`. The display-only line numbers
 * (`oldLineNo` / `newLineNo`) are intentionally dropped; only `kind` and
 * `content` cross the wire. `path`, `oldPath` (renames) and `isUntracked` are
 * carried straight from the diff.
 *
 * `staged` records which diff the hunk came from (unstaged -> false, staged ->
 * true); the backend uses it to sanity-check the apply direction.
 *
 * `contextLines` is the current slider value the diff was rendered with. The
 * backend re-diffs at this exact width to confirm the hunk still matches before
 * applying, so a stale click cannot silently patch the wrong bytes.
 */
export function toHunkPayload(
  diff: FileDiff,
  hunk: Hunk,
  staged: boolean,
  contextLines: number,
): HunkPatchPayload {
  return {
    path: diff.path,
    oldPath: diff.oldPath,
    staged,
    isUntracked: diff.isUntracked,
    contextLines,
    header: hunk.header,
    lines: hunk.lines.map((line) => ({ kind: line.kind, content: line.content })),
  };
}
