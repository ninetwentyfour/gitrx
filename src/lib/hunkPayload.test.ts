import { describe, expect, it } from "vitest";
import type { FileDiff, Hunk } from "../types/ipc";
import { toHunkPayload } from "./hunkPayload";

function makeHunk(overrides: Partial<Hunk> = {}): Hunk {
  return {
    header: "@@ -1,3 +1,3 @@ fn main()",
    oldStart: 1,
    oldLines: 3,
    newStart: 1,
    newLines: 3,
    lines: [
      { kind: "context", oldLineNo: 1, newLineNo: 1, content: "a" },
      { kind: "del", oldLineNo: 2, newLineNo: null, content: "b" },
      { kind: "add", oldLineNo: null, newLineNo: 2, content: "B" },
      { kind: "context", oldLineNo: 3, newLineNo: 3, content: "c" },
    ],
    ...overrides,
  };
}

function makeDiff(overrides: Partial<FileDiff> = {}): FileDiff {
  return {
    path: "src/main.rs",
    language: "rust",
    isBinary: false,
    isUntracked: false,
    hunks: [],
    ...overrides,
  };
}

describe("toHunkPayload", () => {
  it("maps path, isUntracked, header and line kind/content, dropping line numbers", () => {
    const diff = makeDiff();
    const hunk = makeHunk();

    const payload = toHunkPayload(diff, hunk, false, 3);

    expect(payload.path).toBe("src/main.rs");
    expect(payload.staged).toBe(false);
    expect(payload.isUntracked).toBe(false);
    expect(payload.contextLines).toBe(3);
    expect(payload.header).toBe("@@ -1,3 +1,3 @@ fn main()");
    expect(payload.lines).toEqual([
      { kind: "context", content: "a" },
      { kind: "del", content: "b" },
      { kind: "add", content: "B" },
      { kind: "context", content: "c" },
    ]);
    // Line numbers must not leak into the payload.
    expect(payload.lines.every((l) => !("oldLineNo" in l) && !("newLineNo" in l))).toBe(true);
  });

  it("carries the staged flag through verbatim", () => {
    const payload = toHunkPayload(makeDiff(), makeHunk(), true, 3);
    expect(payload.staged).toBe(true);
  });

  it("includes the contextLines the diff was rendered with", () => {
    expect(toHunkPayload(makeDiff(), makeHunk(), false, 0).contextLines).toBe(0);
    expect(toHunkPayload(makeDiff(), makeHunk(), false, 8).contextLines).toBe(8);
  });

  it("carries oldPath for renames and isUntracked for untracked files", () => {
    const renamed = toHunkPayload(makeDiff({ oldPath: "src/old.rs" }), makeHunk(), false, 3);
    expect(renamed.oldPath).toBe("src/old.rs");

    const untracked = toHunkPayload(makeDiff({ isUntracked: true }), makeHunk(), false, 3);
    expect(untracked.isUntracked).toBe(true);
  });

  it("preserves a trailing \\r in content (CRLF byte-exactness)", () => {
    const hunk = makeHunk({
      lines: [
        { kind: "context", oldLineNo: 1, newLineNo: 1, content: "a\r" },
        { kind: "del", oldLineNo: 2, newLineNo: null, content: "b\r" },
        { kind: "add", oldLineNo: null, newLineNo: 2, content: "B\r" },
      ],
    });

    const payload = toHunkPayload(makeDiff(), hunk, false, 3);

    expect(payload.lines.map((l) => l.content)).toEqual(["a\r", "b\r", "B\r"]);
  });

  it("carries noNewline marker lines through unchanged", () => {
    const hunk = makeHunk({
      lines: [
        { kind: "context", oldLineNo: 1, newLineNo: 1, content: "x" },
        { kind: "add", oldLineNo: null, newLineNo: 2, content: "yEDIT" },
        {
          kind: "noNewline",
          oldLineNo: null,
          newLineNo: null,
          content: "\\ No newline at end of file",
        },
      ],
    });

    const payload = toHunkPayload(makeDiff(), hunk, false, 3);

    expect(payload.lines).toContainEqual({
      kind: "noNewline",
      content: "\\ No newline at end of file",
    });
  });
});
