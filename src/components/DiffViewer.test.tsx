/**
 * DiffViewer Tests
 *
 * The right-hand diff pane: toolbar subtitle, hunk rendering, lossy/binary
 * fallbacks, inline image previews, and the virtualization threshold.
 *
 * Key behaviors:
 * - Renders hunk header, gutter numbers, +/- markers, and enabled hunk actions
 * - Lossy (non-UTF-8) diffs disable hunk actions with an explanatory title
 * - Binary files show a placeholder; image binaries fetch an inline preview that
 *   re-fetches on diff-identity change and falls back to the placeholder on error
 * - Diffs over the line threshold switch to the virtual list
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { DiffViewer } from "./DiffViewer";
import { useAppStore } from "../store/useAppStore";
import { makeDiff, makeDiffLine, makeHunk } from "../test/factories";
import type { FileDiff } from "../types/ipc";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

// Highlighting is async and pulls in shiki's wasm; stub it so these DOM-shape
// tests stay synchronous and deterministic (plain-text render path).
vi.mock("../highlight/useDiffHighlight", () => ({
  useDiffHighlight: () => null,
}));

import { readImage } from "../api/git";

const mockReadImage = vi.mocked(readImage);

/** A single-file (plain-click) selection. */
function selFor(path: string, staged = false) {
  return { staged, paths: [path], anchorPath: path, focusedPath: path };
}

const textDiff = makeDiff({
  path: "src/main.rs",
  language: "rust",
  hunks: [
    makeHunk({
      header: "@@ -1,3 +1,4 @@ fn main()",
      oldLines: 3,
      newLines: 4,
      lines: [
        makeDiffLine({ kind: "context", oldLineNo: 1, newLineNo: 1, content: "fn main() {" }),
        makeDiffLine({ kind: "del", oldLineNo: 2, newLineNo: null, content: "    old();" }),
        makeDiffLine({ kind: "add", oldLineNo: null, newLineNo: 2, content: "    new();" }),
        makeDiffLine({ kind: "add", oldLineNo: null, newLineNo: 3, content: "    more();" }),
        makeDiffLine({ kind: "context", oldLineNo: 3, newLineNo: 4, content: "}" }),
      ],
    }),
  ],
});

// A non-image binary keeps the plain placeholder path.
const binaryDiff = makeDiff({ path: "app.bin", isBinary: true });

// An image binary triggers the inline preview fetch.
const imageDiff = makeDiff({ path: "logo.png", isBinary: true });

beforeEach(() => {
  useAppStore.setState({
    status: null,
    selection: null,
    contextLines: 3,
    currentDiff: null,
    diffLoading: false,
    loading: false,
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("DiffViewer", () => {
  it("shows the empty state when nothing is selected", () => {
    render(<DiffViewer />);
    expect(screen.getByText(/select a file to view its diff/i)).toBeInTheDocument();
  });

  it("renders hunk header, gutter numbers, markers and enabled Stage/Discard buttons", () => {
    useAppStore.setState({
      selection: selFor("src/main.rs"),
      currentDiff: textDiff,
    });
    const { container } = render(<DiffViewer />);

    // Subtitle for an unstaged selection.
    expect(screen.getByText(/Unstaged changes for src\/main\.rs/)).toBeInTheDocument();

    // Sticky hunk header text.
    expect(screen.getByText("@@ -1,3 +1,4 @@ fn main()")).toBeInTheDocument();

    // Markers for add/del rows.
    expect(screen.getAllByText("+").length).toBe(2);
    expect(screen.getByText("−")).toBeInTheDocument();

    // The deleted row shows only the old line number (#2) in its gutter and
    // leaves the new gutter empty; the added row is the inverse.
    const delRow = container.querySelector('.diff-line[data-kind="del"]');
    const delGutters = delRow?.querySelectorAll(".diff-line__gutter");
    expect(delGutters?.[0]?.textContent).toBe("2");
    expect(delGutters?.[1]?.textContent).toBe("");

    const addRow = container.querySelector('.diff-line[data-kind="add"]');
    const addGutters = addRow?.querySelectorAll(".diff-line__gutter");
    expect(addGutters?.[0]?.textContent).toBe("");
    expect(addGutters?.[1]?.textContent).toBe("2");

    // Enabled Stage / Discard buttons for an unstaged hunk.
    expect(screen.getByRole("button", { name: "Stage" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Discard" })).toBeEnabled();
  });

  it("disables the hunk Stage/Discard buttons with the lossy title for a non-UTF-8 diff", () => {
    useAppStore.setState({
      selection: selFor("src/main.rs"),
      currentDiff: { ...textDiff, isLossy: true },
    });
    render(<DiffViewer />);

    const stage = screen.getByRole("button", { name: "Stage" });
    const discard = screen.getByRole("button", { name: "Discard" });
    expect(stage).toBeDisabled();
    expect(discard).toBeDisabled();
    const lossyTitle = "File contains non-UTF-8 text — use whole-file staging";
    expect(stage).toHaveAttribute("title", lossyTitle);
    expect(discard).toHaveAttribute("title", lossyTitle);
  });

  it("disables the Unstage hunk button with the lossy title for a lossy staged diff", () => {
    useAppStore.setState({
      selection: selFor("src/main.rs", true),
      currentDiff: { ...textDiff, isLossy: true },
    });
    render(<DiffViewer />);

    const unstage = screen.getByRole("button", { name: "Unstage" });
    expect(unstage).toBeDisabled();
    expect(unstage).toHaveAttribute(
      "title",
      "File contains non-UTF-8 text — use whole-file staging",
    );
  });

  it("renders an Unstage button for staged selections", () => {
    useAppStore.setState({
      selection: selFor("src/main.rs", true),
      currentDiff: textDiff,
    });
    render(<DiffViewer />);

    expect(screen.getByText(/Staged changes for src\/main\.rs/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Unstage" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Stage" })).not.toBeInTheDocument();
  });

  it("shows a binary notice and no diff rows for non-image binary files", () => {
    useAppStore.setState({
      selection: selFor("app.bin"),
      currentDiff: binaryDiff,
    });
    const { container } = render(<DiffViewer />);

    expect(screen.getByText(/binary or oversized file/i)).toBeInTheDocument();
    expect(container.querySelectorAll(".diff-line").length).toBe(0);
  });

  it("offers a whole-file Stage button for an unstaged binary file", () => {
    useAppStore.setState({
      selection: selFor("app.bin"),
      currentDiff: binaryDiff,
    });
    render(<DiffViewer />);

    expect(screen.getByRole("button", { name: "Stage" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Unstage" })).not.toBeInTheDocument();
  });

  it("offers a whole-file Unstage button for a staged binary file", () => {
    useAppStore.setState({
      selection: selFor("app.bin", true),
      currentDiff: { ...binaryDiff },
    });
    render(<DiffViewer />);

    expect(screen.getByRole("button", { name: "Unstage" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Stage" })).not.toBeInTheDocument();
  });

  it("renders an inline image preview for a binary image file", async () => {
    mockReadImage.mockResolvedValueOnce({ mimeType: "image/png", base64: "AAAA" });
    useAppStore.setState({
      selection: selFor("logo.png"),
      currentDiff: imageDiff,
    });
    const { container } = render(<DiffViewer />);

    const img = await screen.findByRole("img");
    expect(img).toHaveAttribute("src", "data:image/png;base64,AAAA");
    expect(mockReadImage).toHaveBeenCalledWith("logo.png", false);
    // The whole-file Stage button is kept above the preview.
    expect(screen.getByRole("button", { name: "Stage" })).toBeInTheDocument();
    expect(container.querySelector(".diff-viewer__image")).toBeInTheDocument();
  });

  it("re-fetches the image preview when the diff object changes for the same path", async () => {
    // A file edited on disk yields a NEW currentDiff object with the SAME path and
    // staged flag; the preview effect keys on diff identity so it re-fetches
    // instead of showing the stale image.
    mockReadImage.mockResolvedValue({ mimeType: "image/png", base64: "AAAA" });
    useAppStore.setState({ selection: selFor("logo.png"), currentDiff: imageDiff });
    render(<DiffViewer />);

    await screen.findByRole("img");
    expect(mockReadImage).toHaveBeenCalledTimes(1);

    // Same path + staged, but a fresh diff object (content changed on disk).
    useAppStore.setState({ currentDiff: { ...imageDiff } });

    await waitFor(() => expect(mockReadImage).toHaveBeenCalledTimes(2));
  });

  it("fetches the index version for a staged image", async () => {
    mockReadImage.mockResolvedValueOnce({ mimeType: "image/gif", base64: "ZZZZ" });
    useAppStore.setState({
      selection: selFor("logo.png", true),
      currentDiff: imageDiff,
    });
    render(<DiffViewer />);

    await screen.findByRole("img");
    expect(mockReadImage).toHaveBeenCalledWith("logo.png", true);
  });

  it("falls back to the binary placeholder when the image fetch fails", async () => {
    mockReadImage.mockRejectedValueOnce("read failed");
    useAppStore.setState({
      selection: selFor("logo.png"),
      currentDiff: imageDiff,
    });
    render(<DiffViewer />);

    await waitFor(() => expect(screen.getByText(/binary or oversized file/i)).toBeInTheDocument());
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it("shows a 'renamed from' note in the toolbar for renamed files", () => {
    useAppStore.setState({
      selection: selFor("src/new.rs"),
      currentDiff: { ...textDiff, path: "src/new.rs", oldPath: "src/old.rs" },
    });
    render(<DiffViewer />);

    expect(screen.getByText(/renamed from src\/old\.rs/)).toBeInTheDocument();
  });

  it("renders diffs at or below the virtualization threshold without a virtual list", () => {
    useAppStore.setState({
      selection: selFor("src/main.rs"),
      currentDiff: textDiff, // 5 lines
    });
    const { container } = render(<DiffViewer />);

    expect(container.querySelector('[data-virtualized="true"]')).toBeNull();
    expect(container.querySelector(".diff-viewer__content")).toBeInTheDocument();
  });

  it("virtualizes diffs whose total line count exceeds the threshold", () => {
    const bigDiff: FileDiff = makeDiff({
      path: "src/big.rs",
      language: "rust",
      hunks: [
        makeHunk({
          header: "@@ -1,2001 +1,2001 @@",
          oldLines: 2001,
          newLines: 2001,
          lines: Array.from({ length: 2001 }, (_, i) =>
            makeDiffLine({ oldLineNo: i + 1, newLineNo: i + 1, content: `line ${i}` }),
          ),
        }),
      ],
    });
    useAppStore.setState({
      selection: selFor("src/big.rs"),
      currentDiff: bigDiff,
    });
    const { container } = render(<DiffViewer />);

    expect(container.querySelector('[data-virtualized="true"]')).toBeInTheDocument();
    expect(container.querySelector(".diff-viewer__content--virtual")).toBeInTheDocument();
  });
});
