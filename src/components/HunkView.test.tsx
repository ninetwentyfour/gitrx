/**
 * HunkView Tests
 *
 * The per-hunk action toolbar rendered inside the diff viewer. Verifies the
 * correct Stage/Discard vs Unstage buttons appear for the file's side and that
 * clicks dispatch the matching store action with the hunk payload.
 *
 * Key behaviors:
 * - Unstaged hunk: enabled Stage + Discard, no Unstage; clicks call stageHunk/discardHunk
 * - Staged hunk: enabled Unstage only; click calls unstageHunk
 * - `busy` disables every button
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { HunkView } from "./HunkView";
import { useAppStore } from "../store/useAppStore";
import { makeHunk } from "../test/factories";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: vi.fn(),
}));

const hunk = makeHunk({ header: "@@ -1,3 +1,3 @@ fn main()" });

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
