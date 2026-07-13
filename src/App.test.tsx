import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import App from "./App";
import { useAppStore } from "./store/useAppStore";
import type { RepoStatus } from "./types/ipc";

vi.mock("./api/git", () => ({
  getStatus: vi.fn(),
  getDiff: vi.fn(),
  openRepo: vi.fn(),
  pickRepoFolder: vi.fn(),
}));

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

const sampleStatus: RepoStatus = {
  repoName: "rust-gitx",
  repoPath: "/repos/rust-gitx",
  branch: "main",
  headHasCommits: true,
  unstaged: [
    {
      path: "src/app/main.rs",
      status: "modified",
      staged: false,
      isBinary: false,
      additions: 12,
      deletions: 3,
    },
  ],
  staged: [
    {
      path: "README.md",
      status: "added",
      staged: true,
      isBinary: false,
      additions: 5,
      deletions: 0,
    },
  ],
};

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
    mockGetStatus.mockRejectedValueOnce("No repository open");
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
