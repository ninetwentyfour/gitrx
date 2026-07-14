/**
 * CommitPanel Tests
 *
 * The commit message editor + Commit/Amend button. Verifies the enable/disable
 * rules (blank message, empty staged set, amend exception, in-flight commit,
 * unborn HEAD) and the Cmd+Enter shortcut gating.
 *
 * Key behaviors:
 * - Commit disabled unless there is a message AND staged files (or an amend)
 * - Amend allowed with an empty staged set; Amend checkbox disabled on unborn HEAD
 * - Cmd+Enter fires doCommit only when the button is enabled
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { CommitPanel } from "./CommitPanel";
import { useAppStore } from "../store/useAppStore";
import { makeFileEntry, makeStatus as baseStatus } from "../test/factories";
import type { RepoStatus } from "../types/ipc";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: vi.fn(),
}));

/** A status whose staged list holds a single modified file. */
function makeStatus(overrides: Partial<RepoStatus> = {}): RepoStatus {
  return baseStatus({ unstaged: [], staged: [makeFileEntry({ staged: true })], ...overrides });
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
