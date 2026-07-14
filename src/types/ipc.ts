/**
 * The discriminant tag of a structured backend error. Mirrors — exactly — the
 * serde `name` tags on the Rust `AppError` enum (`src-tauri/src/error.rs`, whose
 * `serde_tag_shape` unit test pins these camelCase strings). `git` and `io` are
 * the backend's generic catch-alls; the rest are semantic and safe to switch on.
 */
export type AppErrorName =
  | "noRepoOpen"
  | "staleHunk"
  | "nonUtf8File"
  | "identityMissing"
  | "nothingStaged"
  | "emptyMessage"
  | "windowClosed"
  | "validation"
  | "git"
  | "io";

/**
 * A structured error surfaced by a Tauri command rejection. The Rust enum is
 * internally tagged (`#[serde(tag = "name")]`), so every variant arrives as this
 * uniform `{ name, message }` shape. `message` is always the human-readable text
 * (identical to the Rust `Display`), preserved for direct toast display.
 */
export type AppError = {
  name: AppErrorName;
  message: string;
};

export type FileStatus =
  | "modified"
  | "added"
  | "deleted"
  | "renamed"
  | "untracked"
  | "conflicted"
  | "typechange";

export type FileEntry = {
  path: string;
  oldPath?: string;
  status: FileStatus;
  staged: boolean;
  isBinary: boolean;
  additions: number;
  deletions: number;
};

export type RepoStatus = {
  repoName: string;
  /** Canonical working-tree path of the repo. Persistence (the `openRepos` set and
   * per-window restore) is owned entirely by the Rust backend; the frontend no
   * longer writes `lastRepoPath`, so this is display/identity data only. */
  repoPath: string;
  branch: string;
  unstaged: FileEntry[];
  staged: FileEntry[];
  headHasCommits: boolean;
};

export type DiffLineKind = "context" | "add" | "del" | "noNewline";

export type DiffLine = {
  kind: DiffLineKind;
  oldLineNo: number | null;
  newLineNo: number | null;
  content: string;
};

export type Hunk = {
  header: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: DiffLine[];
};

export type FileDiff = {
  path: string;
  oldPath?: string;
  language: string | null;
  isBinary: boolean;
  isUntracked: boolean;
  /** True when the file contains non-UTF-8 bytes, so per-hunk patch staging is
   * unsafe (the lossy text round-trip would corrupt it). Always present. The UI
   * disables hunk-level actions and steers the user to whole-file staging. */
  isLossy: boolean;
  hunks: Hunk[];
};

/** One line of a hunk as sent to the backend patch builder. */
export type HunkPatchLine = {
  kind: DiffLineKind;
  content: string;
};

/**
 * The payload handed to the `stage_hunk` / `unstage_hunk` / `discard_hunk`
 * commands. Mirrors the Rust `HunkPatchPayload` (camelCase). `content` is carried
 * verbatim, including any trailing `\r`, so CRLF hunks round-trip byte-exactly.
 */
export type HunkPatchPayload = {
  path: string;
  // `| undefined` (not just optional) so a caller may build this from a diff
  // whose `oldPath` is absent by copying the field verbatim under
  // exactOptionalPropertyTypes. The key is still optional, so serde/JSON drops
  // it when undefined — the wire contract (None => key omitted) is unchanged.
  oldPath?: string | undefined;
  staged: boolean;
  isUntracked: boolean;
  /** Context-line count the diff was rendered with; the backend re-diffs at this
   * width to verify the hunk is still fresh before applying. */
  contextLines: number;
  header: string;
  lines: HunkPatchLine[];
};
