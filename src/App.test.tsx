/**
 * App Tests
 *
 * The root component's two top-level states: the no-repo prompt versus the
 * populated staging view driven by the store's `status`.
 *
 * Key behaviors:
 * - getStatus rejection renders the "Open Repository" prompt, no file lists
 * - a loaded status renders the Unstaged/Staged lists and the repo name
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import App from "./App";
import { useAppStore } from "./store/useAppStore";
import { makeAppError, makeFileEntry, makeStatus } from "./test/factories";

vi.mock("./api/git", async () => (await import("./test/factories")).mockGitApi());

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ listen: vi.fn().mockResolvedValue(() => {}) }),
}));

vi.mock("@tauri-apps/plugin-store", () => ({
  load: vi.fn().mockResolvedValue({
    get: vi.fn().mockResolvedValue(undefined),
    set: vi.fn().mockResolvedValue(undefined),
    save: vi.fn().mockResolvedValue(undefined),
  }),
}));

// Imported after the mock so these are the mocked implementations.
import { getStatus } from "./api/git";

const mockGetStatus = vi.mocked(getStatus);

const sampleStatus = makeStatus({
  repoName: "rust-gitx",
  repoPath: "/repos/rust-gitx",
  unstaged: [makeFileEntry({ path: "src/app/main.rs", additions: 12, deletions: 3 })],
  staged: [makeFileEntry({ path: "README.md", status: "added", staged: true, additions: 5 })],
});

beforeEach(() => {
  useAppStore.setState({
    status: null,
    selection: null,
    loading: false,
    toasts: [],
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("App", () => {
  it("renders the no-repo state when getStatus rejects", async () => {
    mockGetStatus.mockRejectedValueOnce(makeAppError());
    render(<App />);

    expect(await screen.findByRole("button", { name: /open repository/i })).toBeInTheDocument();
    expect(screen.queryByText("Unstaged Changes")).not.toBeInTheDocument();
  });

  it("renders the file lists when the store has status", async () => {
    mockGetStatus.mockResolvedValueOnce(sampleStatus);
    render(<App />);

    expect(await screen.findByText("Unstaged Changes")).toBeInTheDocument();
    expect(screen.getByText("Staged Changes")).toBeInTheDocument();
    expect(screen.getByText("src/app/main.rs")).toBeInTheDocument();
    expect(screen.getByText("README.md")).toBeInTheDocument();
    expect(screen.getByText(/rust-gitx/i, { selector: ".header-bar__repo" })).toBeInTheDocument();
  });
});
