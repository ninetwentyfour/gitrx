/**
 * useAppStore — Selection & Mutation Tests
 *
 * Everything about the multi-file selection and how index/working-tree mutations
 * move and refresh it: click gestures (plain/cmd/shift, cross-list), the
 * deferred-collapse timer, status reconcile across refresh, whole-file
 * stage/unstage/discard (with the busy re-entrancy guard), and per-hunk
 * stage/unstage/discard (including the untracked whole-file delegation).
 *
 * Key behaviors:
 * - Selection reconcile drops vanished paths, repairs anchor, clears focus+diff
 * - A plain click on a multi-selection member defers the collapse; a refresh
 *   cancels a pending collapse (timer-vs-refresh interleaving)
 * - Mutations follow the focused file to the other list, refresh once, and toast a
 *   captured failure AFTER resyncing; busy guards prevent overlap/double dialogs
 * - Untracked hunk stage/discard delegate to whole-file stageFile/discardFile
 *
 * See also:
 * - `useAppStore.test.ts` for the diff/status sequence guards these rely on
 * - `../components/FileList.test.tsx` for the DOM-level gesture wiring
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { makeDiff, makeFileEntry, makeStatus } from "../test/factories";
import { sampleHunk, sel, statusWith } from "./useAppStore.testkit";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn() }));

import { confirm } from "@tauri-apps/plugin-dialog";
import {
  discardFile,
  discardHunk,
  getDiff,
  getStatus,
  stageFile,
  stageHunk,
  unstageFile,
  unstageHunk,
} from "../api/git";
import { useAppStore } from "./useAppStore";

const mockGetStatus = vi.mocked(getStatus);
const mockGetDiff = vi.mocked(getDiff);
const mockStageFile = vi.mocked(stageFile);
const mockUnstageFile = vi.mocked(unstageFile);
const mockDiscardFile = vi.mocked(discardFile);
const mockStageHunk = vi.mocked(stageHunk);
const mockUnstageHunk = vi.mocked(unstageHunk);
const mockDiscardHunk = vi.mocked(discardHunk);
const mockConfirm = vi.mocked(confirm);

beforeEach(() => {
  useAppStore.setState({
    status: null,
    selection: null,
    contextLines: 3,
    currentDiff: null,
    diffLoading: false,
    loading: false,
    busy: false,
    toasts: [],
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("selection reconcile across refresh", () => {
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

  it("refreshStatus cancels a pending deferred collapse (timer-vs-refresh interleaving)", async () => {
    // A refresh reconciles the selection out from under an armed collapse timer, so
    // refreshStatus must cancel it. The status still contains b.txt, so a surviving
    // stale timer WOULD wrongly collapse onto it — this proves the cancellation.
    vi.useFakeTimers();
    try {
      useAppStore.setState({ status: statusWith(["a.txt", "b.txt"]) });
      mockGetDiff.mockResolvedValue(makeDiff());
      const s = useAppStore.getState();
      s.selectFile("a.txt", false);
      s.selectFile("b.txt", false, { meta: true });
      s.selectFile("b.txt", false); // arms the deferred collapse

      mockGetStatus.mockResolvedValueOnce(statusWith(["a.txt", "b.txt"]));
      await useAppStore.getState().refreshStatus({ silent: true });

      // The selection is intact (both files), and the cancelled collapse never
      // fires to shrink it to just b.txt.
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);
      vi.runAllTimers();
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a deferred collapse whose row vanished before firing is cancelled silently", () => {
    vi.useFakeTimers();
    try {
      useAppStore.setState({ status: statusWith(["a.txt", "b.txt"]) });
      mockGetDiff.mockResolvedValue(makeDiff());
      const s = useAppStore.getState();
      // Build a multi-selection, then a plain click on member b arms the collapse.
      s.selectFile("a.txt", false);
      s.selectFile("b.txt", false, { meta: true });
      s.selectFile("b.txt", false);
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);

      // b.txt vanishes from the status before the deferred collapse fires.
      useAppStore.setState({ status: statusWith(["a.txt"]) });
      vi.runAllTimers();

      // The collapse did NOT pin focus onto the vanished b.txt; nothing collapsed.
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("selectFile", () => {
  it("selectFile() fetches the diff for the selection", async () => {
    const diff = makeDiff({ path: "a.txt", hunks: [] });
    mockGetDiff.mockResolvedValueOnce(diff);

    useAppStore.getState().selectFile("a.txt", false);
    // let the in-flight refreshDiff resolve
    await vi.waitFor(() => expect(useAppStore.getState().currentDiff).toEqual(diff));

    expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 3);
    expect(useAppStore.getState().diffLoading).toBe(false);
  });
});

describe("selection gestures", () => {
  function setup() {
    // Status supplies the list order for shift-ranges; diffs are stubbed.
    useAppStore.setState({
      status: statusWith(["a.txt", "b.txt", "c.txt", "d.txt"], ["s1.txt", "s2.txt"]),
    });
    mockGetDiff.mockResolvedValue(makeDiff());
    return { store: useAppStore.getState() };
  }

  it("plain click selects only the clicked file (anchor + focus)", () => {
    const { store } = setup();
    store.selectFile("b.txt", false);
    expect(useAppStore.getState().selection).toEqual(sel("b.txt"));
  });

  it("re-selecting the focused file does not refetch the diff", () => {
    const { store } = setup();
    store.selectFile("a.txt", false);
    store.selectFile("a.txt", false);
    expect(mockGetDiff).toHaveBeenCalledTimes(1);
  });

  it("cmd-click toggles a file in, moving focus to it, then back out", () => {
    const { store } = setup();
    store.selectFile("a.txt", false);
    store.selectFile("c.txt", false, { meta: true });
    expect(useAppStore.getState().selection).toMatchObject({
      paths: ["a.txt", "c.txt"],
      focusedPath: "c.txt",
    });
    // Toggling off the focused c.txt moves focus to the last remaining file.
    store.selectFile("c.txt", false, { meta: true });
    expect(useAppStore.getState().selection).toMatchObject({
      paths: ["a.txt"],
      focusedPath: "a.txt",
    });
  });

  it("cmd-click toggling off the last file clears the selection", () => {
    const { store } = setup();
    store.selectFile("a.txt", false);
    store.selectFile("a.txt", false, { meta: true });
    expect(useAppStore.getState().selection).toBeNull();
  });

  it("shift-click selects the contiguous range downward from the anchor", () => {
    const { store } = setup();
    store.selectFile("b.txt", false);
    store.selectFile("d.txt", false, { shift: true });
    expect(useAppStore.getState().selection).toEqual({
      staged: false,
      paths: ["b.txt", "c.txt", "d.txt"],
      anchorPath: "b.txt",
      focusedPath: "d.txt",
    });
  });

  it("shift-click ranges upward too, keeping the original anchor", () => {
    const { store } = setup();
    store.selectFile("c.txt", false);
    store.selectFile("a.txt", false, { shift: true });
    expect(useAppStore.getState().selection).toEqual({
      staged: false,
      paths: ["a.txt", "b.txt", "c.txt"],
      anchorPath: "c.txt",
      focusedPath: "a.txt",
    });
  });

  it("selecting in one list clears the other list's selection", () => {
    const { store } = setup();
    store.selectFile("a.txt", false);
    store.selectFile("b.txt", false, { meta: true });
    expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);
    // A (modified) click into the staged list replaces the whole selection.
    store.selectFile("s1.txt", true, { meta: true });
    expect(useAppStore.getState().selection).toEqual(sel("s1.txt", true));
  });
});

describe("whole-file staging", () => {
  it("stageFile() moves the selection to the staged list and refetches the diff", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
    });
    mockStageFile.mockResolvedValueOnce(undefined);
    // After staging, a.txt lives on the staged side.
    mockGetStatus.mockResolvedValueOnce(
      makeStatus({ unstaged: [], staged: [makeFileEntry({ staged: true })] }),
    );
    mockGetDiff.mockResolvedValue(makeDiff({ path: "a.txt" }));

    await useAppStore.getState().stageFile("a.txt");

    expect(mockStageFile).toHaveBeenCalledWith("a.txt");
    expect(useAppStore.getState().selection).toEqual(sel("a.txt", true));
    expect(mockGetDiff).toHaveBeenCalledWith("a.txt", true, 3);
  });

  it("unstageFile() moves the selection to the unstaged list and refetches the diff", async () => {
    const stagedStatus = makeStatus({ unstaged: [], staged: [makeFileEntry({ staged: true })] });
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

describe("discardFile", () => {
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
      status: makeStatus({ unstaged: [makeFileEntry({ path: "u.txt", status: "untracked" })] }),
    });
    mockConfirm.mockResolvedValueOnce(false);

    await useAppStore.getState().discardFile("u.txt");

    expect(mockConfirm).toHaveBeenCalledWith("Delete untracked file u.txt?");
    expect(mockDiscardFile).not.toHaveBeenCalled();
  });

  it("discardFile() toasts the api failure and clears busy", async () => {
    // A confirmed discard whose backend call rejects must surface the message and
    // release the busy flag so the UI does not wedge.
    useAppStore.setState({ status: makeStatus() });
    mockConfirm.mockResolvedValueOnce(true);
    mockDiscardFile.mockRejectedValueOnce("could not remove file");

    await useAppStore.getState().discardFile("a.txt");

    expect(useAppStore.getState().toasts.map((t) => t.message)).toContain("could not remove file");
    expect(useAppStore.getState().busy).toBe(false);
  });

  it("discardFile double-call raises exactly ONE confirm dialog", async () => {
    useAppStore.setState({ status: makeStatus() });
    // A confirm that never resolves keeps the first call 'busy', so the second
    // call must short-circuit on the busy guard instead of prompting again.
    let resolveConfirm!: (v: boolean) => void;
    mockConfirm.mockReturnValueOnce(
      new Promise<boolean>((_resolve) => {
        resolveConfirm = _resolve;
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
});

describe("hunk staging", () => {
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
      status: makeStatus({ unstaged: [makeFileEntry({ path: "u.txt", status: "untracked" })] }),
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

  it("discardHunk double-dispatch raises exactly ONE confirm dialog (busy set before confirm)", async () => {
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
      currentDiff: makeDiff({ path: "a.txt", hunks: [sampleHunk] }),
    });
    // A confirm that never resolves keeps the first dispatch 'busy', so the second
    // must short-circuit on the busy guard rather than opening a second dialog.
    let resolveConfirm!: (v: boolean) => void;
    mockConfirm.mockReturnValueOnce(
      new Promise<boolean>((_resolve) => {
        resolveConfirm = _resolve;
      }),
    );

    const first = useAppStore.getState().discardHunk(sampleHunk);
    const second = useAppStore.getState().discardHunk(sampleHunk); // should be a no-op

    expect(mockConfirm).toHaveBeenCalledTimes(1);

    resolveConfirm(false); // decline, then drain both promises
    await Promise.all([first, second]);

    expect(mockConfirm).toHaveBeenCalledTimes(1);
    expect(mockDiscardHunk).not.toHaveBeenCalled();
    expect(useAppStore.getState().busy).toBe(false);
  });

  it("stageHunk() on an untracked file delegates to stageFile (no patch command)", async () => {
    useAppStore.setState({
      status: makeStatus({
        unstaged: [makeFileEntry({ path: "u.txt", status: "untracked", additions: 3 })],
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
});
