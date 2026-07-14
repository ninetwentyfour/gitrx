/**
 * Shared Test Factories & Mocks
 *
 * Typed builders for the IPC contract types plus the canonical `../api/git`
 * vi.mock surface. Every builder uses the partial-override pattern: callers pass
 * only the fields that matter to their assertion and the builder fills the rest
 * with sensible defaults. Because the builders are typed against
 * `src/types/ipc.ts`, a contract drift (a renamed or removed field) fails
 * compilation here rather than silently rotting per-file fixtures.
 *
 * Key behaviors:
 * - makeFileEntry / makeStatus / makeDiff / makeHunk / makeDiffLine: partial-override
 *   builders returning fresh objects per call
 * - mockGitApi(): the FULL `../api/git` mock surface (every export present), so no
 *   per-file mock can drift out of sync with the real module (the historical
 *   hazard: DiffViewer's mock lacked `commit` / `getHeadCommitMessage`)
 *
 * See also:
 * - `src/types/ipc.ts` for the contract these mirror
 * - `src/api/git.ts` for the export surface `mockGitApi()` must cover
 */
import { vi } from "vitest";
import type { AppError, DiffLine, FileDiff, FileEntry, Hunk, RepoStatus } from "../types/ipc";

/**
 * A structured backend error, exactly as a `Result<T, AppError>` command
 * rejection deserializes it (`{ name, message }`). Defaults to the `noRepoOpen`
 * variant with its canonical message, since that is the discriminated case the
 * store branches on; override `name`/`message` for other variants. Reject with
 * this (not a bare string) wherever the code path discriminates on `err.name`.
 */
export function makeAppError(overrides: Partial<AppError> = {}): AppError {
  return {
    name: "noRepoOpen",
    message: "No repository open",
    ...overrides,
  };
}

/** A single working-tree/index file entry. Defaults to an unstaged modification. */
export function makeFileEntry(overrides: Partial<FileEntry> = {}): FileEntry {
  return {
    path: "a.txt",
    status: "modified",
    staged: false,
    isBinary: false,
    additions: 1,
    deletions: 0,
    ...overrides,
  };
}

/** A repo status with one unstaged modified file and nothing staged. */
export function makeStatus(overrides: Partial<RepoStatus> = {}): RepoStatus {
  return {
    repoName: "repo",
    repoPath: "/repos/repo",
    branch: "main",
    headHasCommits: true,
    unstaged: [makeFileEntry()],
    staged: [],
    ...overrides,
  };
}

/** A single diff line. Defaults to a context line at old/new 1. */
export function makeDiffLine(overrides: Partial<DiffLine> = {}): DiffLine {
  return {
    kind: "context",
    oldLineNo: 1,
    newLineNo: 1,
    content: "a",
    ...overrides,
  };
}

/** A hunk with a 3-line context/del/add body (`a` / `b` -> `B`). */
export function makeHunk(overrides: Partial<Hunk> = {}): Hunk {
  return {
    header: "@@ -1,3 +1,3 @@",
    oldStart: 1,
    oldLines: 3,
    newStart: 1,
    newLines: 3,
    lines: [
      makeDiffLine({ kind: "context", oldLineNo: 1, newLineNo: 1, content: "a" }),
      makeDiffLine({ kind: "del", oldLineNo: 2, newLineNo: null, content: "b" }),
      makeDiffLine({ kind: "add", oldLineNo: null, newLineNo: 2, content: "B" }),
    ],
    ...overrides,
  };
}

/** A non-binary, tracked text diff with no hunks. */
export function makeDiff(overrides: Partial<FileDiff> = {}): FileDiff {
  return {
    path: "a.txt",
    language: null,
    isBinary: false,
    isUntracked: false,
    isLossy: false,
    hunks: [],
    ...overrides,
  };
}

/**
 * The full mock surface for `../api/git`: every export is a fresh `vi.fn()`.
 * Consume as the vi.mock factory itself — `vi.mock("../api/git", () =>
 * mockGitApi())` — so a suite can never omit an export and let production code
 * call an undefined mock. Suites that need default resolutions configure the
 * individual spies via `vi.mocked(...)` in their setup.
 */
export function mockGitApi() {
  return {
    openRepo: vi.fn(),
    getStatus: vi.fn(),
    getDiff: vi.fn(),
    stageFile: vi.fn(),
    unstageFile: vi.fn(),
    discardFile: vi.fn(),
    showFileContextMenu: vi.fn(),
    readImage: vi.fn(),
    stageHunk: vi.fn(),
    unstageHunk: vi.fn(),
    discardHunk: vi.fn(),
    commit: vi.fn(),
    getHeadCommitMessage: vi.fn(),
    pickRepoFolder: vi.fn(),
  };
}
