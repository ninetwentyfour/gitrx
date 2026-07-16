/**
 * FileList Interaction Tests
 *
 * The staging-panel list wired to the live store: click/cmd/shift selection,
 * double-click stage/unstage, the deferred-collapse timer dance, and the
 * right-click native menu bridge.
 *
 * Key behaviors:
 * - Plain/cmd/shift clicks build the expected multi-selection and clear the
 *   other list on cross-list selection
 * - A plain click on a multi-selection member DEFERS the collapse; a following
 *   double-click / cmd-click cancels the pending collapse
 * - Double-click stages/unstages the acted rows; right-click selects then opens
 *   the native menu, toasting a normalized failure message
 */
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { FileList } from "./FileList";
import { useFileListKeyboardNav } from "../hooks/useFileListKeyboardNav";
import { useAppStore } from "../store/useAppStore";
import { makeFileEntry, makeStatus as baseStatus } from "../test/factories";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn() }));

import { getDiff, showFileContextMenu } from "../api/git";

const mockShowMenu = vi.mocked(showFileContextMenu);

function makeStatus() {
  const f = (path: string, staged: boolean) => makeFileEntry({ path, staged });
  return baseStatus({
    unstaged: [f("a.txt", false), f("b.txt", false), f("c.txt", false)],
    staged: [f("s1.txt", true)],
  });
}

/** Render both file lists wired to the live store. */
function TwoLists() {
  const status = useAppStore((s) => s.status);
  if (!status) return null;
  return (
    <>
      <FileList title="Unstaged" files={status.unstaged} staged={false} />
      <FileList title="Staged" files={status.staged} staged />
    </>
  );
}

const rowBtn = (name: RegExp) => screen.getByRole("button", { name });

// The two store actions FileList tests replace in-place; snapshotted so the
// afterEach can restore the real implementations and keep the mutation from
// leaking file-wide (a per-file assignment to stageFiles/unstageFiles otherwise
// persists across tests once set).
const realStageFiles = useAppStore.getState().stageFiles;
const realUnstageFiles = useAppStore.getState().unstageFiles;

beforeEach(() => {
  vi.mocked(getDiff).mockResolvedValue({
    path: "x",
    language: null,
    isBinary: false,
    isUntracked: false,
    isLossy: false,
    hunks: [],
  });
  vi.mocked(showFileContextMenu).mockResolvedValue(undefined);
  useAppStore.setState({
    status: makeStatus(),
    selection: null,
    currentDiff: null,
    busy: false,
    toasts: [],
  });
});

afterEach(() => {
  vi.clearAllMocks();
  // Restore the real store actions in case a test swapped in a spy.
  useAppStore.setState({ stageFiles: realStageFiles, unstageFiles: realUnstageFiles });
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

  it("a context-menu failure toasts the normalized message, not String(err)", async () => {
    // An Error rejection must surface `err.message` (via the store's toMessage),
    // not the `String(err)` form ("Error: ...").
    mockShowMenu.mockRejectedValueOnce(new Error("menu boom"));
    render(<TwoLists />);
    fireEvent.contextMenu(rowBtn(/b\.txt/));

    await waitFor(() =>
      expect(useAppStore.getState().toasts.map((t) => t.message)).toContain("menu boom"),
    );
    expect(useAppStore.getState().toasts.map((t) => t.message)).not.toContain("Error: menu boom");
  });
});

describe("FileList keyboard navigation", () => {
  // Mounts the ONE window-level keyboard-nav listener (App's job in production)
  // around whatever list(s) a test renders, so arrows are actually wired.
  function WithNav({ children }: { children: ReactNode }) {
    useFileListKeyboardNav();
    return children;
  }

  // Dispatch an arrow keydown from document.body — deliberately reproducing the
  // FIELD condition the old container handler died under: in WKWebView a mouse
  // click never focuses the row <button>, so focus stays on <body> and the keydown
  // fires there, never reaching the list. jsdom (unlike WebKit) DOES focus a button
  // on click, so we blur it first to make <body> the genuine event target.
  const arrowOnBody = (key: "ArrowDown" | "ArrowUp", shiftKey = false) => {
    (document.activeElement as HTMLElement | null)?.blur();
    fireEvent.keyDown(document.body, { key, shiftKey });
  };

  it("ArrowDown selects the next path (keydown on body, not the row)", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/a\.txt/));
    arrowOnBody("ArrowDown");
    expect(useAppStore.getState().selection).toMatchObject({
      staged: false,
      paths: ["b.txt"],
      focusedPath: "b.txt",
    });
  });

  it("ArrowUp selects the previous path", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/b\.txt/));
    arrowOnBody("ArrowUp");
    expect(useAppStore.getState().selection).toMatchObject({
      staged: false,
      paths: ["a.txt"],
      focusedPath: "a.txt",
    });
  });

  it("ArrowUp on the first row is a no-op (selection unchanged)", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/a\.txt/));
    const before = useAppStore.getState().selection;
    arrowOnBody("ArrowUp");
    expect(useAppStore.getState().selection).toEqual(before);
  });

  it("ArrowDown on the last row is a no-op (selection unchanged)", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/c\.txt/));
    const before = useAppStore.getState().selection;
    arrowOnBody("ArrowDown");
    expect(useAppStore.getState().selection).toEqual(before);
  });

  it("Shift+ArrowDown extends the selection and preserves the anchor", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/a\.txt/));
    arrowOnBody("ArrowDown", true);
    expect(useAppStore.getState().selection).toMatchObject({
      paths: ["a.txt", "b.txt"],
      anchorPath: "a.txt",
      focusedPath: "b.txt",
    });
  });

  it("moves roving DOM focus onto the newly selected row's button", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/a\.txt/));
    arrowOnBody("ArrowDown");
    expect(document.activeElement).toBe(rowBtn(/b\.txt/));
  });

  it("does nothing when there is no selection", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    arrowOnBody("ArrowDown");
    expect(useAppStore.getState().selection).toBeNull();
  });

  it("navigates the STAGED list's order when the selection lives there (cross-list)", () => {
    // The global hook must key off `selection.staged` and the staged order — never
    // the unstaged list — so a selection on the staged side moves within it.
    useAppStore.setState({
      status: baseStatus({
        unstaged: [makeFileEntry({ path: "a.txt", staged: false })],
        staged: [
          makeFileEntry({ path: "s1.txt", staged: true }),
          makeFileEntry({ path: "s2.txt", staged: true }),
        ],
      }),
      selection: null,
    });
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/s1\.txt/));
    arrowOnBody("ArrowDown");
    expect(useAppStore.getState().selection).toMatchObject({
      staged: true,
      paths: ["s2.txt"],
      focusedPath: "s2.txt",
    });
  });

  it("ignores the arrow while a <textarea> is focused (commit message keeps its own nav)", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/a\.txt/));
    const before = useAppStore.getState().selection;

    const textarea = document.createElement("textarea");
    document.body.append(textarea);
    textarea.focus();
    // Target is the textarea; the guard must let the keystroke through untouched.
    fireEvent.keyDown(textarea, { key: "ArrowDown" });

    expect(useAppStore.getState().selection).toEqual(before);
    textarea.remove();
  });

  it("ignores the arrow while an input[type=range] is focused (context slider steps)", () => {
    render(
      <WithNav>
        <TwoLists />
      </WithNav>,
    );
    fireEvent.click(rowBtn(/a\.txt/));
    const before = useAppStore.getState().selection;

    const range = document.createElement("input");
    range.type = "range";
    document.body.append(range);
    range.focus();
    fireEvent.keyDown(range, { key: "ArrowDown" });

    expect(useAppStore.getState().selection).toEqual(before);
    range.remove();
  });

  it("skips the presentational untracked divider when navigating", () => {
    // Mixed tracked+untracked list: arrowing off the last tracked row lands on the
    // first untracked row, since the divider is absent from the ordered paths.
    const tracked = (path: string) => makeFileEntry({ path, staged: false, status: "modified" });
    const untracked = (path: string) => makeFileEntry({ path, staged: false, status: "untracked" });
    const mixed = [tracked("a.txt"), tracked("b.txt"), untracked("u1.txt"), untracked("u2.txt")];
    useAppStore.setState({ status: baseStatus({ unstaged: mixed, staged: [] }), selection: null });
    render(
      <WithNav>
        <FileList title="Unstaged" files={mixed} staged={false} />
      </WithNav>,
    );

    fireEvent.click(rowBtn(/b\.txt/));
    arrowOnBody("ArrowDown");
    expect(useAppStore.getState().selection).toMatchObject({
      paths: ["u1.txt"],
      focusedPath: "u1.txt",
    });
  });
});

describe("FileList untracked group divider", () => {
  const tracked = (path: string) => makeFileEntry({ path, staged: false, status: "modified" });
  const untracked = (path: string) => makeFileEntry({ path, staged: false, status: "untracked" });

  // Backend already sorts tracked-then-untracked; the fixtures mirror that order.
  const mixed = [tracked("a.txt"), tracked("b.txt"), untracked("u1.txt"), untracked("u2.txt")];

  it("renders a divider between the tracked and untracked groups", () => {
    const { container } = render(<FileList title="Unstaged" files={mixed} staged={false} />);
    const divider = container.querySelector(".file-list__divider");
    expect(divider).toBeInTheDocument();
    // It sits immediately after the last tracked row and before the first untracked.
    expect(divider?.previousElementSibling).toHaveTextContent("b.txt");
    expect(divider?.nextElementSibling).toHaveTextContent("u1.txt");
  });

  it("keeps the divider out of the accessibility/interaction tree", () => {
    const { container } = render(<FileList title="Unstaged" files={mixed} staged={false} />);
    const divider = container.querySelector(".file-list__divider");
    expect(divider).toHaveAttribute("aria-hidden", "true");
    // No focusable/interactive element inside it.
    expect(divider?.querySelector("button, a, [tabindex]")).toBeNull();
  });

  it("renders no divider when every entry is tracked", () => {
    const { container } = render(
      <FileList title="Unstaged" files={[tracked("a.txt"), tracked("b.txt")]} staged={false} />,
    );
    expect(container.querySelector(".file-list__divider")).toBeNull();
  });

  it("renders no divider when every entry is untracked", () => {
    const { container } = render(
      <FileList
        title="Unstaged"
        files={[untracked("u1.txt"), untracked("u2.txt")]}
        staged={false}
      />,
    );
    expect(container.querySelector(".file-list__divider")).toBeNull();
  });

  it("never renders a divider for the staged list (driven by untracked presence, not the staged prop)", () => {
    const { container } = render(
      <FileList
        title="Staged"
        files={[makeFileEntry({ path: "s.txt", staged: true, status: "modified" })]}
        staged
      />,
    );
    expect(container.querySelector(".file-list__divider")).toBeNull();
  });

  it("shift-click spanning the divider selects the contiguous tracked+untracked range", () => {
    // The divider is presentational, so `orderedPaths` (derived from status.unstaged)
    // is unbroken: a range from a tracked row into an untracked row is contiguous.
    useAppStore.setState({
      status: baseStatus({ unstaged: mixed, staged: [] }),
      selection: null,
    });
    render(<FileList title="Unstaged" files={mixed} staged={false} />);

    fireEvent.click(rowBtn(/a\.txt/));
    fireEvent.click(rowBtn(/u1\.txt/), { shiftKey: true });
    expect(useAppStore.getState().selection?.paths).toEqual(["a.txt", "b.txt", "u1.txt"]);
  });
});
