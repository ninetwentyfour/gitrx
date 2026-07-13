import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { CommitPanel } from "./CommitPanel";
import { useAppStore } from "../store/useAppStore";
import type { FileEntry, RepoStatus } from "../types/ipc";

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

function stagedEntry(path = "a.txt"): FileEntry {
  return {
    path,
    status: "modified",
    staged: true,
    isBinary: false,
    additions: 1,
    deletions: 0,
  };
}

function makeStatus(overrides: Partial<RepoStatus> = {}): RepoStatus {
  return {
    repoName: "repo",
    repoPath: "/repos/repo",
    branch: "main",
    headHasCommits: true,
    unstaged: [],
    staged: [stagedEntry()],
    ...overrides,
  };
}

/** Seed the store with a known baseline before each render. */
function seed(overrides: Partial<ReturnType<typeof useAppStore.getState>> = {}) {
  useAppStore.setState({
    status: makeStatus(),
    commitMessage: "",
    amend: false,
    commitBusy: false,
    commitDraft: "",
    lastPrefill: null,
    ...overrides,
  });
}

beforeEach(() => {
  seed();
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("CommitPanel", () => {
  it("disables Commit when the message is blank", () => {
    seed({ commitMessage: "   " });
    render(<CommitPanel />);
    expect(screen.getByRole("button", { name: "Commit" })).toBeDisabled();
  });

  it("disables Commit when nothing is staged and not amending", () => {
    seed({ commitMessage: "hello", status: makeStatus({ staged: [] }) });
    render(<CommitPanel />);
    expect(screen.getByRole("button", { name: "Commit" })).toBeDisabled();
  });

  it("enables Commit with a message and staged files", () => {
    seed({ commitMessage: "hello" });
    render(<CommitPanel />);
    expect(screen.getByRole("button", { name: "Commit" })).toBeEnabled();
  });

  it("allows an amend with an empty staged set (message-only edit)", () => {
    seed({ commitMessage: "reword", amend: true, status: makeStatus({ staged: [] }) });
    render(<CommitPanel />);
    const button = screen.getByRole("button", { name: "Amend Commit" });
    expect(button).toBeEnabled();
  });

  it("disables Commit while a commit is in flight", () => {
    seed({ commitMessage: "hello", commitBusy: true });
    render(<CommitPanel />);
    expect(screen.getByRole("button", { name: "Commit" })).toBeDisabled();
  });

  it("disables the Amend checkbox when HEAD has no commits", () => {
    seed({ status: makeStatus({ headHasCommits: false }) });
    render(<CommitPanel />);
    expect(screen.getByRole("checkbox")).toBeDisabled();
  });

  it("Cmd+Enter fires the commit when it is enabled", () => {
    const doCommit = vi.fn();
    seed({ commitMessage: "hello", doCommit });
    render(<CommitPanel />);

    fireEvent.keyDown(screen.getByLabelText("Commit message"), {
      key: "Enter",
      metaKey: true,
    });

    expect(doCommit).toHaveBeenCalledTimes(1);
  });

  it("Cmd+Enter does nothing when the commit is disabled", () => {
    const doCommit = vi.fn();
    seed({ commitMessage: "", doCommit });
    render(<CommitPanel />);

    fireEvent.keyDown(screen.getByLabelText("Commit message"), {
      key: "Enter",
      metaKey: true,
    });

    expect(doCommit).not.toHaveBeenCalled();
  });
});
