import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FileDiff, Hunk, RepoStatus } from "../types/ipc";

vi.mock("../api/git", () => ({
  getStatus: vi.fn(),
  getDiff: vi.fn(),
  openRepo: vi.fn(),
  pickRepoFolder: vi.fn(),
  stageFile: vi.fn(),
  unstageFile: vi.fn(),
  discardFile: vi.fn(),
  stageHunk: vi.fn(),
  unstageHunk: vi.fn(),
  discardHunk: vi.fn(),
  commit: vi.fn(),
  getHeadCommitMessage: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: vi.fn(),
}));

// Per-window event subscription: the store listens via
// getCurrentWebviewWindow().listen so each window only refreshes for its repo.
const webviewMocks = vi.hoisted(() => ({ listen: vi.fn() }));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ listen: webviewMocks.listen }),
}));

// Hoisted stable spies so we can assert the native window theme is mirrored and
// the native window title tracks the open repo.
const windowMocks = vi.hoisted(() => ({ setTheme: vi.fn(), setTitle: vi.fn() }));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTheme: windowMocks.setTheme, setTitle: windowMocks.setTitle }),
}));

// Hoisted so the vi.mock factory (itself hoisted) can reference the spies.
const storeMocks = vi.hoisted(() => ({
  get: vi.fn(),
  set: vi.fn(),
  save: vi.fn(),
  delete: vi.fn(),
  load: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-store", () => ({
  load: storeMocks.load,
}));

import { confirm } from "@tauri-apps/plugin-dialog";
import {
  commit,
  discardFile,
  discardHunk,
  getDiff,
  getHeadCommitMessage,
  getStatus,
  openRepo,
  pickRepoFolder,
  stageFile,
  stageHunk,
  unstageFile,
  unstageHunk,
} from "../api/git";
import { useAppStore } from "./useAppStore";

const mockGetStatus = vi.mocked(getStatus);
const mockGetDiff = vi.mocked(getDiff);
const mockOpenRepo = vi.mocked(openRepo);
const mockPickRepoFolder = vi.mocked(pickRepoFolder);
const mockStageFile = vi.mocked(stageFile);
const mockUnstageFile = vi.mocked(unstageFile);
const mockDiscardFile = vi.mocked(discardFile);
const mockStageHunk = vi.mocked(stageHunk);
const mockUnstageHunk = vi.mocked(unstageHunk);
const mockDiscardHunk = vi.mocked(discardHunk);
const mockCommit = vi.mocked(commit);
const mockGetHeadCommitMessage = vi.mocked(getHeadCommitMessage);
const mockConfirm = vi.mocked(confirm);
const mockListen = webviewMocks.listen;

function makeDiff(overrides: Partial<FileDiff> = {}): FileDiff {
  return {
    path: "a.txt",
    language: null,
    isBinary: false,
    isUntracked: false,
    hunks: [],
    ...overrides,
  };
}

/** Build a single-file (plain-click) selection for the given list. */
function sel(path: string, staged = false) {
  return { staged, paths: [path], anchorPath: path, focusedPath: path };
}

/** Build a status whose unstaged list holds the given plain modified files. */
function statusWith(unstaged: string[], staged: string[] = []): RepoStatus {
  const entry = (path: string, isStaged: boolean) => ({
    path,
    status: "modified" as const,
    staged: isStaged,
    isBinary: false,
    additions: 1,
    deletions: 0,
  });
  return {
    repoName: "repo",
    repoPath: "/repos/repo",
    branch: "main",
    headHasCommits: true,
    unstaged: unstaged.map((p) => entry(p, false)),
    staged: staged.map((p) => entry(p, true)),
  };
}

function makeStatus(overrides: Partial<RepoStatus> = {}): RepoStatus {
  return {
    repoName: "repo",
    repoPath: "/repos/repo",
    branch: "main",
    headHasCommits: true,
    unstaged: [
      {
        path: "a.txt",
        status: "modified",
        staged: false,
        isBinary: false,
        additions: 1,
        deletions: 0,
      },
    ],
    staged: [],
    ...overrides,
  };
}

beforeEach(() => {
  useAppStore.setState({
    status: null,
    selection: null,
    contextLines: 3,
    currentDiff: null,
    diffLoading: false,
    loading: false,
    busy: false,
    theme: "system",
    toasts: [],
    commitMessage: "",
    amend: false,
    commitBusy: false,
    commitDraft: "",
    lastPrefill: null,
  });

  // Native window theme + title mirroring resolve by default.
  windowMocks.setTheme.mockResolvedValue(undefined);
  windowMocks.setTitle.mockResolvedValue(undefined);

  // Default plugin-store behaviour: an empty settings store that accepts writes.
  storeMocks.get.mockResolvedValue(undefined);
  storeMocks.set.mockResolvedValue(undefined);
  storeMocks.save.mockResolvedValue(undefined);
  storeMocks.delete.mockResolvedValue(true);
  storeMocks.load.mockResolvedValue({
    get: storeMocks.get,
    set: storeMocks.set,
    save: storeMocks.save,
    delete: storeMocks.delete,
  });
});

afterEach(() => {
  vi.clearAllMocks();
  document.documentElement.removeAttribute("data-theme");
});

/** A promise whose resolution is controllable from the outside. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

describe("useAppStore", () => {
  it("initialize() shows no-repo state (no toast) when getStatus rejects", async () => {
    mockGetStatus.mockRejectedValueOnce("No repository open");
    await useAppStore.getState().initialize();

    const state = useAppStore.getState();
    expect(state.status).toBeNull();
    expect(state.toasts).toEqual([]);
  });

  it("initialize() stores status on success", async () => {
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    await useAppStore.getState().initialize();

    expect(useAppStore.getState().status?.repoName).toBe("repo");
  });

  // Reopen-on-launch and repo-path persistence moved entirely to the Rust
  // backend (per-window restore + `openRepos`), so `initialize()` no longer
  // writes `lastRepoPath` or tries to reopen a remembered repo: it just reflects
  // whatever repo the backend bound to this window.
  it("initialize() does NOT reopen a repo or write lastRepoPath (backend owns restore)", async () => {
    mockGetStatus.mockRejectedValueOnce("No repository open");
    await useAppStore.getState().initialize();

    expect(mockOpenRepo).not.toHaveBeenCalled();
    expect(storeMocks.set).not.toHaveBeenCalledWith("lastRepoPath", expect.anything());
    expect(useAppStore.getState().status).toBeNull();
    expect(useAppStore.getState().toasts).toEqual([]);
  });

  it("initialize() does not persist on a successful status load", async () => {
    mockGetStatus.mockResolvedValueOnce(makeStatus({ repoPath: "/repos/live" }));
    await useAppStore.getState().initialize();

    expect(useAppStore.getState().status?.repoName).toBe("repo");
    expect(storeMocks.set).not.toHaveBeenCalledWith("lastRepoPath", expect.anything());
  });

  it("keeps a still-present selection across refresh", async () => {
    useAppStore.setState({ selection: sel("a.txt") });
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    await useAppStore.getState().refreshStatus();

    expect(useAppStore.getState().selection).toEqual(sel("a.txt"));
  });

  it("clears a selection that no longer exists after refresh", async () => {
    useAppStore.setState({ selection: sel("gone.txt") });
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    await useAppStore.getState().refreshStatus();

    expect(useAppStore.getState().selection).toBeNull();
  });

  it("drops vanished paths but keeps survivors, clearing the diff when focus vanishes", async () => {
    useAppStore.setState({
      selection: {
        staged: false,
        paths: ["a.txt", "b.txt"],
        anchorPath: "a.txt",
        focusedPath: "b.txt",
      },
      currentDiff: makeDiff({ path: "b.txt" }),
    });
    // Only a.txt survives; the focused b.txt is gone.
    mockGetStatus.mockResolvedValueOnce(statusWith(["a.txt"]));
    await useAppStore.getState().refreshStatus();

    const { selection, currentDiff } = useAppStore.getState();
    expect(selection?.paths).toEqual(["a.txt"]);
    expect(selection?.focusedPath).toBeNull();
    expect(currentDiff).toBeNull();
  });

  it("openRepoViaPicker() is a no-op when the picker is cancelled", async () => {
    mockPickRepoFolder.mockResolvedValueOnce(null);
    await useAppStore.getState().openRepoViaPicker();

    expect(mockOpenRepo).not.toHaveBeenCalled();
    expect(useAppStore.getState().status).toBeNull();
  });

  it("openRepoViaPicker() surfaces backend errors as a toast", async () => {
    mockPickRepoFolder.mockResolvedValueOnce("/some/path");
    mockOpenRepo.mockRejectedValueOnce("boom");
    await useAppStore.getState().openRepoViaPicker();

    const { toasts, loading } = useAppStore.getState();
    expect(toasts.map((t) => t.message)).toContain("boom");
    expect(loading).toBe(false);
  });

  it("openRepoViaPicker() opens the repo without writing lastRepoPath (backend persists)", async () => {
    mockPickRepoFolder.mockResolvedValueOnce("/some/path");
    mockOpenRepo.mockResolvedValueOnce(makeStatus({ repoPath: "/repos/opened" }));

    await useAppStore.getState().openRepoViaPicker();

    expect(mockOpenRepo).toHaveBeenCalledWith("/some/path");
    expect(useAppStore.getState().status?.repoName).toBe("repo");
    // The Rust `open_repo` command binds + persists; the frontend no longer does.
    expect(storeMocks.set).not.toHaveBeenCalledWith("lastRepoPath", expect.anything());
  });

  it("selectFile() fetches the diff for the selection", async () => {
    const diff = makeDiff({ path: "a.txt", hunks: [] });
    mockGetDiff.mockResolvedValueOnce(diff);

    useAppStore.getState().selectFile("a.txt", false);
    // let the in-flight refreshDiff resolve
    await vi.waitFor(() => expect(useAppStore.getState().currentDiff).toEqual(diff));

    expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 3);
    expect(useAppStore.getState().diffLoading).toBe(false);
  });

  it("setContextLines() debounces and refetches with the new value", async () => {
    vi.useFakeTimers();
    try {
      useAppStore.setState({ selection: sel("a.txt") });
      mockGetDiff.mockResolvedValue(makeDiff({ path: "a.txt" }));

      useAppStore.getState().setContextLines(6);
      useAppStore.getState().setContextLines(7); // supersedes the first

      expect(useAppStore.getState().contextLines).toBe(7);
      expect(mockGetDiff).not.toHaveBeenCalled(); // still debouncing

      await vi.advanceTimersByTimeAsync(120);

      expect(mockGetDiff).toHaveBeenCalledTimes(1);
      expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 7);
    } finally {
      vi.useRealTimers();
    }
  });

  it("ignores a stale diff response that resolves after a newer one", async () => {
    useAppStore.setState({ selection: sel("a.txt") });

    const first = deferred<FileDiff>();
    const second = deferred<FileDiff>();
    mockGetDiff.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const p1 = useAppStore.getState().refreshDiff();
    const p2 = useAppStore.getState().refreshDiff();

    const newer = makeDiff({ path: "a.txt", language: "newer" });
    const older = makeDiff({ path: "a.txt", language: "older" });

    // Resolve the newer request first, then the stale older one.
    second.resolve(newer);
    await p2;
    first.resolve(older);
    await p1;

    expect(useAppStore.getState().currentDiff?.language).toBe("newer");
  });

  it("stageFile() moves the selection to the staged list and refetches the diff", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
    });
    mockStageFile.mockResolvedValueOnce(undefined);
    // After staging, a.txt lives on the staged side.
    mockGetStatus.mockResolvedValueOnce(
      makeStatus({
        unstaged: [],
        staged: [
          {
            path: "a.txt",
            status: "modified",
            staged: true,
            isBinary: false,
            additions: 1,
            deletions: 0,
          },
        ],
      }),
    );
    mockGetDiff.mockResolvedValue(makeDiff({ path: "a.txt" }));

    await useAppStore.getState().stageFile("a.txt");

    expect(mockStageFile).toHaveBeenCalledWith("a.txt");
    expect(useAppStore.getState().selection).toEqual(sel("a.txt", true));
    expect(mockGetDiff).toHaveBeenCalledWith("a.txt", true, 3);
  });

  it("unstageFile() moves the selection to the unstaged list and refetches the diff", async () => {
    const stagedStatus = makeStatus({
      unstaged: [],
      staged: [
        {
          path: "a.txt",
          status: "modified",
          staged: true,
          isBinary: false,
          additions: 1,
          deletions: 0,
        },
      ],
    });
    useAppStore.setState({
      status: stagedStatus,
      selection: sel("a.txt", true),
    });
    mockUnstageFile.mockResolvedValueOnce(undefined);
    // After unstaging, a.txt is back on the unstaged side (makeStatus default).
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    mockGetDiff.mockResolvedValue(makeDiff({ path: "a.txt" }));

    await useAppStore.getState().unstageFile("a.txt");

    expect(mockUnstageFile).toHaveBeenCalledWith("a.txt");
    expect(useAppStore.getState().selection).toEqual(sel("a.txt", false));
    expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 3);
  });

  it("discardFile() asks for confirmation and is a no-op when declined", async () => {
    useAppStore.setState({ status: makeStatus() });
    mockConfirm.mockResolvedValueOnce(false);

    await useAppStore.getState().discardFile("a.txt");

    expect(mockConfirm).toHaveBeenCalledWith(
      "Discard all changes to a.txt? This cannot be undone.",
    );
    expect(mockDiscardFile).not.toHaveBeenCalled();
  });

  it("discardFile() calls the api and refreshes when confirmed", async () => {
    useAppStore.setState({ status: makeStatus() });
    mockConfirm.mockResolvedValueOnce(true);
    mockDiscardFile.mockResolvedValueOnce(undefined);
    mockGetStatus.mockResolvedValueOnce(makeStatus({ unstaged: [] }));

    await useAppStore.getState().discardFile("a.txt");

    expect(mockDiscardFile).toHaveBeenCalledWith("a.txt");
    expect(mockGetStatus).toHaveBeenCalled();
  });

  it("discardFile() uses untracked wording for untracked files", async () => {
    useAppStore.setState({
      status: makeStatus({
        unstaged: [
          {
            path: "u.txt",
            status: "untracked",
            staged: false,
            isBinary: false,
            additions: 1,
            deletions: 0,
          },
        ],
      }),
    });
    mockConfirm.mockResolvedValueOnce(false);

    await useAppStore.getState().discardFile("u.txt");

    expect(mockConfirm).toHaveBeenCalledWith("Delete untracked file u.txt?");
    expect(mockDiscardFile).not.toHaveBeenCalled();
  });

  describe("selection gestures", () => {
    beforeEach(() => {
      // Status supplies the list order for shift-ranges; diffs are stubbed.
      useAppStore.setState({
        status: statusWith(["a.txt", "b.txt", "c.txt", "d.txt"], ["s1.txt", "s2.txt"]),
      });
      mockGetDiff.mockResolvedValue(makeDiff());
    });

    it("plain click selects only the clicked file (anchor + focus)", () => {
      useAppStore.getState().selectFile("b.txt", false);
      expect(useAppStore.getState().selection).toEqual(sel("b.txt"));
    });

    it("re-selecting the focused file does not refetch the diff", () => {
      const s = useAppStore.getState();
      s.selectFile("a.txt", false);
      s.selectFile("a.txt", false);
      expect(mockGetDiff).toHaveBeenCalledTimes(1);
    });

    it("cmd-click toggles a file in, moving focus to it, then back out", () => {
      const s = useAppStore.getState();
      s.selectFile("a.txt", false);
      s.selectFile("c.txt", false, { meta: true });
      expect(useAppStore.getState().selection).toMatchObject({
        paths: ["a.txt", "c.txt"],
        focusedPath: "c.txt",
      });
      // Toggling off the focused c.txt moves focus to the last remaining file.
      s.selectFile("c.txt", false, { meta: true });
      expect(useAppStore.getState().selection).toMatchObject({
        paths: ["a.txt"],
        focusedPath: "a.txt",
      });
    });

    it("cmd-click toggling off the last file clears the selection", () => {
      const s = useAppStore.getState();
      s.selectFile("a.txt", false);
      s.selectFile("a.txt", false, { meta: true });
      expect(useAppStore.getState().selection).toBeNull();
    });

    it("shift-click selects the contiguous range downward from the anchor", () => {
      const s = useAppStore.getState();
      s.selectFile("b.txt", false);
      s.selectFile("d.txt", false, { shift: true });
      expect(useAppStore.getState().selection).toEqual({
        staged: false,
        paths: ["b.txt", "c.txt", "d.txt"],
        anchorPath: "b.txt",
        focusedPath: "d.txt",
      });
    });

    it("shift-click ranges upward too, keeping the original anchor", () => {
      const s = useAppStore.getState();
      s.selectFile("c.txt", false);
      s.selectFile("a.txt", false, { shift: true });
      expect(useAppStore.getState().selection).toEqual({
        staged: false,
        paths: ["a.txt", "b.txt", "c.txt"],
        anchorPath: "c.txt",
        focusedPath: "a.txt",
      });
    });

    it("selecting in one list clears the other list's selection", () => {
      const s = useAppStore.getState();
      s.selectFile("a.txt", false);
      s.selectFile("b.txt", false, { meta: true });
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);
      // A (modified) click into the staged list replaces the whole selection.
      s.selectFile("s1.txt", true, { meta: true });
      expect(useAppStore.getState().selection).toEqual(sel("s1.txt", true));
    });
  });

  describe("batch staging", () => {
    it("stageFiles stages each path sequentially then refreshes once", async () => {
      useAppStore.setState({ status: statusWith(["a.txt", "b.txt"]) });
      mockStageFile.mockResolvedValue(undefined);
      mockGetStatus.mockResolvedValueOnce(statusWith([], ["a.txt", "b.txt"]));

      await useAppStore.getState().stageFiles(["a.txt", "b.txt"]);

      expect(mockStageFile).toHaveBeenCalledTimes(2);
      expect(mockStageFile).toHaveBeenNthCalledWith(1, "a.txt");
      expect(mockStageFile).toHaveBeenNthCalledWith(2, "b.txt");
      expect(mockGetStatus).toHaveBeenCalledTimes(1);
    });

    it("stageFiles continues past a mid-batch error and toasts the first message", async () => {
      useAppStore.setState({ status: statusWith(["a.txt", "b.txt", "c.txt"]) });
      mockStageFile
        .mockResolvedValueOnce(undefined)
        .mockRejectedValueOnce("permission denied")
        .mockResolvedValueOnce(undefined);
      mockGetStatus.mockResolvedValueOnce(statusWith([]));

      await useAppStore.getState().stageFiles(["a.txt", "b.txt", "c.txt"]);

      // Every file is attempted despite the middle failure.
      expect(mockStageFile).toHaveBeenCalledTimes(3);
      expect(useAppStore.getState().toasts.map((t) => t.message)).toContain("permission denied");
    });

    it("unstageFiles moves the focused file's selection to the unstaged list", async () => {
      useAppStore.setState({
        status: statusWith([], ["a.txt", "b.txt"]),
        selection: {
          staged: true,
          paths: ["a.txt", "b.txt"],
          anchorPath: "a.txt",
          focusedPath: "b.txt",
        },
      });
      mockUnstageFile.mockResolvedValue(undefined);
      mockGetStatus.mockResolvedValueOnce(statusWith(["a.txt", "b.txt"]));
      mockGetDiff.mockResolvedValue(makeDiff({ path: "b.txt" }));

      await useAppStore.getState().unstageFiles(["a.txt", "b.txt"]);

      expect(mockUnstageFile).toHaveBeenCalledTimes(2);
      expect(useAppStore.getState().selection).toEqual(sel("b.txt", false));
      expect(mockGetDiff).toHaveBeenCalledWith("b.txt", false, 3);
    });

    it("stageFiles is a no-op while another mutation is busy", async () => {
      useAppStore.setState({ status: statusWith(["a.txt"]), busy: true });
      await useAppStore.getState().stageFiles(["a.txt"]);
      expect(mockStageFile).not.toHaveBeenCalled();
    });
  });

  const sampleHunk: Hunk = {
    header: "@@ -1,3 +1,3 @@",
    oldStart: 1,
    oldLines: 3,
    newStart: 1,
    newLines: 3,
    lines: [
      { kind: "context", oldLineNo: 1, newLineNo: 1, content: "a" },
      { kind: "del", oldLineNo: 2, newLineNo: null, content: "b" },
      { kind: "add", oldLineNo: null, newLineNo: 2, content: "B" },
    ],
  };

  it("stageHunk() sends the mapped payload and refreshes status + diff", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
      currentDiff: makeDiff({ path: "a.txt", hunks: [sampleHunk] }),
    });
    mockStageHunk.mockResolvedValueOnce(undefined);
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt", hunks: [sampleHunk] }));

    await useAppStore.getState().stageHunk(sampleHunk);

    expect(mockStageHunk).toHaveBeenCalledWith({
      path: "a.txt",
      oldPath: undefined,
      staged: false,
      isUntracked: false,
      contextLines: 3,
      header: "@@ -1,3 +1,3 @@",
      lines: [
        { kind: "context", content: "a" },
        { kind: "del", content: "b" },
        { kind: "add", content: "B" },
      ],
    });
    expect(mockGetStatus).toHaveBeenCalled();
    expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 3);
    expect(useAppStore.getState().busy).toBe(false);
    expect(useAppStore.getState().toasts).toEqual([]);
  });

  it("unstageHunk() sends a payload flagged staged=true", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt", true),
      currentDiff: makeDiff({ path: "a.txt", hunks: [sampleHunk] }),
    });
    mockUnstageHunk.mockResolvedValueOnce(undefined);
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt", hunks: [sampleHunk] }));

    await useAppStore.getState().unstageHunk(sampleHunk);

    expect(mockUnstageHunk).toHaveBeenCalledWith(
      expect.objectContaining({ path: "a.txt", staged: true }),
    );
  });

  it("stageHunk() keeps the git error AND still resyncs on failure", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
      currentDiff: makeDiff({ path: "a.txt", hunks: [sampleHunk] }),
    });
    mockStageHunk.mockRejectedValueOnce("error: patch does not apply");
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt", hunks: [sampleHunk] }));

    await useAppStore.getState().stageHunk(sampleHunk);

    // The error is toasted after the resync (silent refreshes don't bury it).
    expect(useAppStore.getState().toasts.map((t) => t.message)).toContain(
      "error: patch does not apply",
    );
    // ...and status + diff were refreshed anyway (stale-click recovery).
    expect(mockGetStatus).toHaveBeenCalled();
    expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 3);
    expect(useAppStore.getState().busy).toBe(false);
  });

  it("discardHunk() confirms first and is a no-op when declined", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
      currentDiff: makeDiff({ path: "a.txt", hunks: [sampleHunk] }),
    });
    mockConfirm.mockResolvedValueOnce(false);

    await useAppStore.getState().discardHunk(sampleHunk);

    expect(mockConfirm).toHaveBeenCalledWith("Discard this hunk of a.txt? This cannot be undone.");
    expect(mockDiscardHunk).not.toHaveBeenCalled();
  });

  it("discardHunk() calls the api and refreshes when confirmed", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
      currentDiff: makeDiff({ path: "a.txt", hunks: [sampleHunk] }),
    });
    mockConfirm.mockResolvedValueOnce(true);
    mockDiscardHunk.mockResolvedValueOnce(undefined);
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt", hunks: [sampleHunk] }));

    await useAppStore.getState().discardHunk(sampleHunk);

    expect(mockDiscardHunk).toHaveBeenCalledWith(
      expect.objectContaining({ path: "a.txt", staged: false }),
    );
    expect(mockGetStatus).toHaveBeenCalled();
  });

  it("discardHunk() on an untracked file delegates to discardFile with ONE dialog", async () => {
    useAppStore.setState({
      status: makeStatus({
        unstaged: [
          {
            path: "u.txt",
            status: "untracked",
            staged: false,
            isBinary: false,
            additions: 1,
            deletions: 0,
          },
        ],
      }),
      selection: sel("u.txt"),
      currentDiff: makeDiff({ path: "u.txt", isUntracked: true, hunks: [sampleHunk] }),
    });
    mockConfirm.mockResolvedValueOnce(false); // decline the (single) delete dialog

    await useAppStore.getState().discardHunk(sampleHunk);

    // Exactly one dialog, and it is the file-level untracked wording (delegation),
    // never the per-hunk prompt.
    expect(mockConfirm).toHaveBeenCalledTimes(1);
    expect(mockConfirm).toHaveBeenCalledWith("Delete untracked file u.txt?");
    // No hunk-level discard is attempted for untracked files.
    expect(mockDiscardHunk).not.toHaveBeenCalled();
  });

  it("stageHunk() on an untracked file delegates to stageFile (no patch command)", async () => {
    useAppStore.setState({
      status: makeStatus({
        unstaged: [
          {
            path: "u.txt",
            status: "untracked",
            staged: false,
            isBinary: false,
            additions: 3,
            deletions: 0,
          },
        ],
      }),
      selection: sel("u.txt"),
      currentDiff: makeDiff({ path: "u.txt", isUntracked: true, hunks: [sampleHunk] }),
    });
    mockStageFile.mockResolvedValueOnce(undefined);
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    mockGetDiff.mockResolvedValue(makeDiff({ path: "u.txt" }));

    await useAppStore.getState().stageHunk(sampleHunk);

    // Whole-file staging (mode-safe) is used; the synthesized-patch command is
    // never invoked for an untracked file.
    expect(mockStageFile).toHaveBeenCalledWith("u.txt");
    expect(mockStageHunk).not.toHaveBeenCalled();
  });

  it("applyHunk is a no-op when a mutation is already in flight (busy)", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
      currentDiff: makeDiff({ path: "a.txt", hunks: [sampleHunk] }),
      busy: true, // pretend a mutation is already running
    });

    await useAppStore.getState().stageHunk(sampleHunk);

    // The guard short-circuits before any backend call.
    expect(mockStageHunk).not.toHaveBeenCalled();
  });

  it("discardFile double-call raises exactly ONE confirm dialog", async () => {
    useAppStore.setState({ status: makeStatus() });
    // A confirm that never resolves keeps the first call 'busy', so the second
    // call must short-circuit on the busy guard instead of prompting again.
    let resolveConfirm!: (v: boolean) => void;
    mockConfirm.mockReturnValueOnce(
      new Promise<boolean>((r) => {
        resolveConfirm = r;
      }),
    );

    const first = useAppStore.getState().discardFile("a.txt");
    const second = useAppStore.getState().discardFile("a.txt"); // should be a no-op

    expect(mockConfirm).toHaveBeenCalledTimes(1);

    // Let the first dialog resolve (declined) and drain both promises.
    resolveConfirm(false);
    await Promise.all([first, second]);

    expect(mockConfirm).toHaveBeenCalledTimes(1);
    expect(mockDiscardFile).not.toHaveBeenCalled();
    expect(useAppStore.getState().busy).toBe(false);
  });

  it("setAmend(true) prefills the message from HEAD when the box is empty", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "" });
    mockGetHeadCommitMessage.mockResolvedValueOnce("previous subject\n\nbody\n");

    await useAppStore.getState().setAmend(true);

    expect(mockGetHeadCommitMessage).toHaveBeenCalled();
    expect(useAppStore.getState().amend).toBe(true);
    expect(useAppStore.getState().commitMessage).toBe("previous subject\n\nbody\n");
  });

  it("setAmend keeps a real draft, then restores it when toggled back off", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "my work in progress" });

    // Turning ON must NOT clobber a real draft (and must not fetch HEAD).
    await useAppStore.getState().setAmend(true);
    expect(mockGetHeadCommitMessage).not.toHaveBeenCalled();
    expect(useAppStore.getState().commitMessage).toBe("my work in progress");

    // Turning OFF restores the pre-amend draft.
    await useAppStore.getState().setAmend(false);
    expect(useAppStore.getState().amend).toBe(false);
    expect(useAppStore.getState().commitMessage).toBe("my work in progress");
  });

  it("setAmend(false) restores the pre-amend draft after a prefill", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "draft text" });
    mockGetHeadCommitMessage.mockResolvedValueOnce("HEAD message");

    // Empty? No — draft is real, so no prefill. Force the empty-box branch:
    useAppStore.setState({ commitMessage: "" });
    await useAppStore.getState().setAmend(true);
    expect(useAppStore.getState().commitMessage).toBe("HEAD message");

    await useAppStore.getState().setAmend(false);
    // Restores the (empty) draft that existed when amend was toggled on.
    expect(useAppStore.getState().commitMessage).toBe("");
  });

  it("doCommit() success clears the message, resets amend, and refreshes", async () => {
    useAppStore.setState({
      status: makeStatus(),
      commitMessage: "ship it",
      amend: false,
    });
    mockCommit.mockResolvedValueOnce({ oid: "abc123" });
    mockGetStatus.mockResolvedValueOnce(makeStatus({ unstaged: [], staged: [] }));

    await useAppStore.getState().doCommit();

    expect(mockCommit).toHaveBeenCalledWith("ship it", false);
    expect(useAppStore.getState().commitMessage).toBe("");
    expect(useAppStore.getState().amend).toBe(false);
    expect(useAppStore.getState().commitBusy).toBe(false);
    expect(mockGetStatus).toHaveBeenCalled();
  });

  it("doCommit() toasts the error and does not clear the message on failure", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "oops", amend: false });
    mockCommit.mockRejectedValueOnce("No staged changes to commit");

    await useAppStore.getState().doCommit();

    expect(useAppStore.getState().toasts.map((t) => t.message)).toContain(
      "No staged changes to commit",
    );
    expect(useAppStore.getState().commitMessage).toBe("oops");
    expect(useAppStore.getState().commitBusy).toBe(false);
  });

  describe("initWatcher", () => {
    /** Capture the store's event handlers keyed by event name. */
    function wireListen() {
      const handlers: Record<string, (event: { payload: unknown }) => void> = {};
      mockListen.mockImplementation((event: string, handler: (e: never) => void) => {
        handlers[event] = handler as (e: { payload: unknown }) => void;
        return Promise.resolve(() => {});
      });
      return handlers;
    }

    it("subscribes to repo-changed and watch-error", async () => {
      wireListen();
      await useAppStore.getState().initWatcher();

      expect(mockListen).toHaveBeenCalledWith("repo-changed", expect.any(Function));
      expect(mockListen).toHaveBeenCalledWith("watch-error", expect.any(Function));
    });

    it("a repo-changed event refreshes status (no diff when nothing is selected)", async () => {
      const handlers = wireListen();
      await useAppStore.getState().initWatcher();
      mockGetStatus.mockResolvedValueOnce(makeStatus());

      handlers["repo-changed"]({ payload: { reason: "fs" } });

      await vi.waitFor(() => expect(mockGetStatus).toHaveBeenCalledTimes(1));
      expect(mockGetDiff).not.toHaveBeenCalled();
    });

    it("a repo-changed event also refreshes the diff when a file is selected", async () => {
      const handlers = wireListen();
      useAppStore.setState({
        status: makeStatus(),
        selection: sel("a.txt"),
      });
      await useAppStore.getState().initWatcher();
      mockGetStatus.mockResolvedValueOnce(makeStatus());
      mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt" }));

      handlers["repo-changed"]({ payload: { reason: "index" } });

      await vi.waitFor(() => expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 3));
      expect(mockGetStatus).toHaveBeenCalledTimes(1);
    });

    it("skips refresh while a mutation is busy", async () => {
      const handlers = wireListen();
      useAppStore.setState({ status: makeStatus(), busy: true });
      await useAppStore.getState().initWatcher();

      handlers["repo-changed"]({ payload: { reason: "index" } });
      await Promise.resolve();

      expect(mockGetStatus).not.toHaveBeenCalled();
    });

    it("skips refresh while a commit is in flight", async () => {
      const handlers = wireListen();
      useAppStore.setState({ status: makeStatus(), commitBusy: true });
      await useAppStore.getState().initWatcher();

      handlers["repo-changed"]({ payload: { reason: "head" } });
      await Promise.resolve();

      expect(mockGetStatus).not.toHaveBeenCalled();
    });

    it("watch-error is logged, not surfaced in the error banner", async () => {
      const handlers = wireListen();
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      await useAppStore.getState().initWatcher();

      handlers["watch-error"]({ payload: "FSEvents blew up" });

      expect(warn).toHaveBeenCalled();
      expect(useAppStore.getState().toasts).toEqual([]);
      warn.mockRestore();
    });
  });

  describe("theme", () => {
    it("setTheme('dark') sets data-theme and persists the choice", async () => {
      useAppStore.getState().setTheme("dark");

      expect(useAppStore.getState().theme).toBe("dark");
      expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
      await vi.waitFor(() => expect(storeMocks.set).toHaveBeenCalledWith("theme", "dark"));
      expect(storeMocks.save).toHaveBeenCalled();
    });

    it("setTheme('system') removes the data-theme attribute", () => {
      document.documentElement.setAttribute("data-theme", "dark");

      useAppStore.getState().setTheme("system");

      expect(useAppStore.getState().theme).toBe("system");
      expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
    });

    it("mirrors the theme onto the native window: 'dark' -> 'dark', 'system' -> null", () => {
      useAppStore.getState().setTheme("dark");
      expect(windowMocks.setTheme).toHaveBeenCalledWith("dark");

      useAppStore.getState().setTheme("system");
      expect(windowMocks.setTheme).toHaveBeenCalledWith(null);
    });

    it("persistence failures never throw and do not block the UI", async () => {
      storeMocks.save.mockRejectedValueOnce("disk full");
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

      expect(() => useAppStore.getState().setTheme("light")).not.toThrow();
      expect(useAppStore.getState().theme).toBe("light");
      await vi.waitFor(() => expect(warn).toHaveBeenCalled());
      warn.mockRestore();
    });

    it("initialize() applies the persisted theme on startup", async () => {
      storeMocks.get.mockResolvedValueOnce("dark");
      mockGetStatus.mockRejectedValueOnce("No repository open");

      await useAppStore.getState().initialize();

      expect(useAppStore.getState().theme).toBe("dark");
      expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    });

    it("initialize() falls back to system when nothing is persisted", async () => {
      storeMocks.get.mockResolvedValueOnce(undefined);
      mockGetStatus.mockRejectedValueOnce("No repository open");

      await useAppStore.getState().initialize();

      expect(useAppStore.getState().theme).toBe("system");
      expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
    });
  });

  describe("window title", () => {
    it("initialize() sets the title to '<repoName> — <branch>' on a status load", async () => {
      mockGetStatus.mockResolvedValueOnce(makeStatus({ repoName: "gitrx", branch: "main" }));

      await useAppStore.getState().initialize();

      expect(windowMocks.setTitle).toHaveBeenCalledWith("gitrx — main");
    });

    it("initialize() sets the title to 'gitrx' in the no-repo state", async () => {
      mockGetStatus.mockRejectedValueOnce("No repository open");

      await useAppStore.getState().initialize();

      expect(windowMocks.setTitle).toHaveBeenCalledWith("gitrx");
    });

    it("refreshStatus() updates the title so a branch change is reflected", async () => {
      mockGetStatus.mockResolvedValueOnce(makeStatus({ repoName: "gitrx", branch: "feature/x" }));

      await useAppStore.getState().refreshStatus();

      expect(windowMocks.setTitle).toHaveBeenCalledWith("gitrx — feature/x");
    });

    it("openRepoViaPicker() sets the title for the freshly opened repo", async () => {
      mockPickRepoFolder.mockResolvedValueOnce("/some/path");
      mockOpenRepo.mockResolvedValueOnce(makeStatus({ repoName: "opened", branch: "dev" }));

      await useAppStore.getState().openRepoViaPicker();

      expect(windowMocks.setTitle).toHaveBeenCalledWith("opened — dev");
    });

    it("a title set failure never throws (swallowed like the theme mirror)", async () => {
      windowMocks.setTitle.mockRejectedValueOnce("no window");
      mockGetStatus.mockResolvedValueOnce(makeStatus());

      await expect(useAppStore.getState().initialize()).resolves.toBeUndefined();
    });
  });

  describe("toasts", () => {
    it("pushToast appends and caps the stack at 4, dropping the oldest", () => {
      const { pushToast } = useAppStore.getState();
      for (let i = 1; i <= 5; i++) pushToast(`msg ${i}`);

      const { toasts } = useAppStore.getState();
      expect(toasts).toHaveLength(4);
      expect(toasts.map((t) => t.message)).toEqual(["msg 2", "msg 3", "msg 4", "msg 5"]);
    });

    it("a toast auto-dismisses after 6s", () => {
      vi.useFakeTimers();
      try {
        useAppStore.getState().pushToast("boom");
        expect(useAppStore.getState().toasts).toHaveLength(1);

        vi.advanceTimersByTime(6000);

        expect(useAppStore.getState().toasts).toHaveLength(0);
      } finally {
        vi.useRealTimers();
      }
    });

    it("dismissToast removes only the targeted toast", () => {
      const { pushToast } = useAppStore.getState();
      pushToast("first");
      pushToast("second");
      const [first] = useAppStore.getState().toasts;

      useAppStore.getState().dismissToast(first.id);

      const messages = useAppStore.getState().toasts.map((t) => t.message);
      expect(messages).toEqual(["second"]);
    });
  });
});
