import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { HunkView } from "./HunkView";
import { useAppStore } from "../store/useAppStore";
import type { Hunk } from "../types/ipc";

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
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: vi.fn(),
}));

const hunk: Hunk = {
  header: "@@ -1,3 +1,3 @@ fn main()",
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

beforeEach(() => {
  useAppStore.setState({
    busy: false,
    stageHunk: vi.fn(),
    unstageHunk: vi.fn(),
    discardHunk: vi.fn(),
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("HunkView", () => {
  it("shows enabled Discard + Stage for an unstaged hunk and calls the actions", () => {
    const stageHunk = vi.fn();
    const discardHunk = vi.fn();
    useAppStore.setState({ stageHunk, discardHunk });

    render(<HunkView hunk={hunk} staged={false} />);

    const stage = screen.getByRole("button", { name: "Stage" });
    const discard = screen.getByRole("button", { name: "Discard" });
    expect(stage).toBeEnabled();
    expect(discard).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Unstage" })).not.toBeInTheDocument();

    fireEvent.click(stage);
    expect(stageHunk).toHaveBeenCalledWith(hunk);

    fireEvent.click(discard);
    expect(discardHunk).toHaveBeenCalledWith(hunk);
  });

  it("shows an enabled Unstage for a staged hunk and calls the action", () => {
    const unstageHunk = vi.fn();
    useAppStore.setState({ unstageHunk });

    render(<HunkView hunk={hunk} staged={true} />);

    const unstage = screen.getByRole("button", { name: "Unstage" });
    expect(unstage).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Stage" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Discard" })).not.toBeInTheDocument();

    fireEvent.click(unstage);
    expect(unstageHunk).toHaveBeenCalledWith(hunk);
  });

  it("disables the buttons while busy", () => {
    useAppStore.setState({ busy: true });

    render(<HunkView hunk={hunk} staged={false} />);

    expect(screen.getByRole("button", { name: "Stage" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Discard" })).toBeDisabled();
  });
});
