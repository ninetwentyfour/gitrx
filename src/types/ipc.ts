export type FileStatus =
  | "modified"
  | "added"
  | "deleted"
  | "renamed"
  | "untracked"
  | "conflicted"
  | "typechange";

export interface FileEntry {
  path: string;
  oldPath?: string;
  status: FileStatus;
  staged: boolean;
  isBinary: boolean;
  additions: number;
  deletions: number;
}

export interface RepoStatus {
  repoName: string;
  /** Canonical working-tree path of the repo; persisted as `lastRepoPath` so the
   * app can reopen the most-recently-opened repository on next launch. */
  repoPath: string;
  branch: string;
  unstaged: FileEntry[];
  staged: FileEntry[];
  headHasCommits: boolean;
}

export type DiffLineKind = "context" | "add" | "del" | "noNewline";

export interface DiffLine {
  kind: DiffLineKind;
  oldLineNo: number | null;
  newLineNo: number | null;
  content: string;
}

export interface Hunk {
  header: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: DiffLine[];
}

export interface FileDiff {
  path: string;
  oldPath?: string;
  language: string | null;
  isBinary: boolean;
  isUntracked: boolean;
  hunks: Hunk[];
}

/** One line of a hunk as sent to the backend patch builder. */
export interface HunkPatchLine {
  kind: DiffLineKind;
  content: string;
}

/**
 * The payload handed to the `stage_hunk` / `unstage_hunk` / `discard_hunk`
 * commands. Mirrors the Rust `HunkPatchPayload` (camelCase). `content` is carried
 * verbatim, including any trailing `\r`, so CRLF hunks round-trip byte-exactly.
 */
export interface HunkPatchPayload {
  path: string;
  oldPath?: string;
  staged: boolean;
  isUntracked: boolean;
  /** Context-line count the diff was rendered with; the backend re-diffs at this
   * width to verify the hunk is still fresh before applying. */
  contextLines: number;
  header: string;
  lines: HunkPatchLine[];
}
