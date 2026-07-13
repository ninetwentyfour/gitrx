import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { FileList } from "./FileList";
import { useAppStore } from "../store/useAppStore";
import type { RepoStatus } from "../types/ipc";

vi.mock("../api/git", () => ({
  getStatus: vi.fn(),
  getDiff: vi.fn().mockResolvedValue({
    path: "x",
    language: null,
    isBinary: false,
    isUntracked: false,
    hunks: [],
  }),
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
  showFileContextMenu: vi.fn().mockResolvedValue(undefined),
  readImage: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn() }));

import { showFileContextMenu } from "../api/git";

const mockShowMenu = vi.mocked(showFileContextMenu);

function makeStatus(): RepoStatus {
  const f = (path: string, staged: boolean) => ({
    path,
    status: "modified" as const,
    staged,
    isBinary: false,
    additions: 1,
    deletions: 0,
  });
  return {
    repoName: "repo",
    repoPath: "/repos/repo",
    branch: "main",
    headHasCommits: true,
    unstaged: [f("a.txt", false), f("b.txt", false), f("c.txt", false)],
    staged: [f("s1.txt", true)],
  };
}

/** Render both file lists wired to the live store. */
function TwoLists() {
  const status = useAppStore((s) => s.status);
  if (!status) return null;
  return (
    <>
      <FileList title="Unstaged" files={status.unstaged} staged={false} />
      <FileList title="Staged" files={status.staged} staged={true} />
    </>
  );
}

const rowBtn = (name: RegExp) => screen.getByRole("button", { name });

beforeEach(() => {
  useAppStore.setState({
    status: makeStatus(),
    selection: null,
    currentDiff: null,
    busy: false,
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("FileList interactions", () => {
  it("plain click selects only the clicked row", () => {
    render(<TwoLists />);
    fireEvent.click(rowBtn(/a\.txt/));
    expect(useAppStore.getState().selection).toMatchObject({ staged: false, paths: ["a.txt"] });
    expect(rowBtn(/a\.txt/)).toHaveClass("is-selected");
  });

  it("cmd-click adds rows; both show the selected style", () => {
    render(<TwoLists />);
    fireEvent.click(rowBtn(/a\.txt/));
    fireEvent.click(rowBtn(/c\.txt/), { metaKey: true });
    expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "c.txt"]);
    expect(rowBtn(/a\.txt/)).toHaveClass("is-selected");
    expect(rowBtn(/c\.txt/)).toHaveClass("is-selected");
  });

  it("shift-click selects a contiguous range", () => {
    render(<TwoLists />);
    fireEvent.click(rowBtn(/a\.txt/));
    fireEvent.click(rowBtn(/c\.txt/), { shiftKey: true });
    expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt", "c.txt"]);
  });

  it("selecting in the staged list clears the unstaged selection", () => {
    render(<TwoLists />);
    fireEvent.click(rowBtn(/a\.txt/));
    fireEvent.click(rowBtn(/s1\.txt/));
    expect(useAppStore.getState().selection).toMatchObject({ staged: true, paths: ["s1.txt"] });
    expect(rowBtn(/a\.txt/)).not.toHaveClass("is-selected");
    expect(rowBtn(/s1\.txt/)).toHaveClass("is-selected");
  });

  it("double-click on an unselected row stages just that row", () => {
    const stageFiles = vi.fn();
    useAppStore.setState({ stageFiles });
    render(<TwoLists />);

    fireEvent.doubleClick(rowBtn(/c\.txt/));
    expect(stageFiles).toHaveBeenCalledWith(["c.txt"]);
  });

  it("double-click on a selected row stages the whole selection", () => {
    const stageFiles = vi.fn();
    useAppStore.setState({ stageFiles });
    render(<TwoLists />);

    fireEvent.click(rowBtn(/a\.txt/));
    fireEvent.click(rowBtn(/b\.txt/), { metaKey: true });
    fireEvent.doubleClick(rowBtn(/b\.txt/));
    expect(stageFiles).toHaveBeenCalledWith(["a.txt", "b.txt"]);
  });

  it("a plain click on a multi-selection member defers the collapse", () => {
    vi.useFakeTimers();
    try {
      render(<TwoLists />);

      // Build a multi-selection a + b.
      fireEvent.click(rowBtn(/a\.txt/));
      fireEvent.click(rowBtn(/b\.txt/), { metaKey: true });
      // A plain click on member b must NOT collapse immediately (no flash).
      fireEvent.click(rowBtn(/b\.txt/));
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);
      expect(rowBtn(/a\.txt/)).toHaveClass("is-selected");
      // Only once the timer elapses does it collapse to the clicked row.
      vi.runAllTimers();
      expect(useAppStore.getState().selection?.paths).toEqual(["b.txt"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("double-click before the collapse timer stages the whole selection and never collapses", () => {
    vi.useFakeTimers();
    try {
      const stageFiles = vi.fn();
      useAppStore.setState({ stageFiles });
      render(<TwoLists />);

      // Build a multi-selection a + b, then start a (deferred) collapse on b.
      fireEvent.click(rowBtn(/a\.txt/));
      fireEvent.click(rowBtn(/b\.txt/), { metaKey: true });
      fireEvent.click(rowBtn(/b\.txt/));
      // The double-click cancels the pending collapse and acts on the intact set.
      fireEvent.doubleClick(rowBtn(/b\.txt/));
      expect(stageFiles).toHaveBeenCalledWith(["a.txt", "b.txt"]);

      // The collapse must never fire afterwards.
      vi.runAllTimers();
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("double-click on a non-member row (collapses instantly) stages just that row", () => {
    const stageFiles = vi.fn();
    useAppStore.setState({ stageFiles });
    render(<TwoLists />);

    fireEvent.click(rowBtn(/a\.txt/));
    fireEvent.click(rowBtn(/b\.txt/), { metaKey: true });
    // Plain-clicking c (not in [a, b]) collapses instantly — no jank to defer.
    fireEvent.click(rowBtn(/c\.txt/));
    expect(useAppStore.getState().selection?.paths).toEqual(["c.txt"]);
    fireEvent.doubleClick(rowBtn(/c\.txt/));

    expect(stageFiles).toHaveBeenCalledWith(["c.txt"]);
  });

  it("a cmd-click cancels a pending deferred collapse", () => {
    vi.useFakeTimers();
    try {
      render(<TwoLists />);

      fireEvent.click(rowBtn(/a\.txt/));
      fireEvent.click(rowBtn(/b\.txt/), { metaKey: true }); // [a, b]
      fireEvent.click(rowBtn(/b\.txt/)); // plain click on member → defer collapse
      // The cmd-click cancels the pending collapse; the selection was never
      // collapsed, so c is added to the full set.
      fireEvent.click(rowBtn(/c\.txt/), { metaKey: true });
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt", "c.txt"]);
      // The stale collapse must not fire and clobber the selection.
      vi.runAllTimers();
      expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt", "c.txt"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("double-click in the staged list unstages", () => {
    const unstageFiles = vi.fn();
    useAppStore.setState({ unstageFiles });
    render(<TwoLists />);

    fireEvent.doubleClick(rowBtn(/s1\.txt/));
    expect(unstageFiles).toHaveBeenCalledWith(["s1.txt"]);
  });

  it("right-click on an unselected row selects it then opens the native menu", () => {
    render(<TwoLists />);
    fireEvent.contextMenu(rowBtn(/b\.txt/));

    expect(useAppStore.getState().selection).toMatchObject({ staged: false, paths: ["b.txt"] });
    expect(mockShowMenu).toHaveBeenCalledWith(["b.txt"], false);
  });

  it("right-click within a multi-selection sends all selected paths", () => {
    render(<TwoLists />);
    fireEvent.click(rowBtn(/a\.txt/));
    fireEvent.click(rowBtn(/b\.txt/), { metaKey: true });
    fireEvent.contextMenu(rowBtn(/b\.txt/));

    expect(mockShowMenu).toHaveBeenCalledWith(["a.txt", "b.txt"], false);
  });
});
