/**
 * toHunkPayload Tests
 *
 * The pure mapping from a rendered `FileDiff` + `Hunk` to the `HunkPatchPayload`
 * the backend patch builder consumes. The critical invariant: `content` is
 * carried BYTE-VERBATIM (trailing `\r` preserved) and line numbers are dropped.
 *
 * Key behaviors:
 * - Maps path/oldPath/isUntracked/header/lines; drops oldLineNo/newLineNo
 * - staged flag and contextLines pass through verbatim
 * - Trailing `\r` preserved (CRLF byte-exactness); noNewline markers pass through
 */
import { describe, expect, it } from "vitest";
import type { FileDiff, Hunk } from "../types/ipc";
import { makeDiff as baseDiff, makeDiffLine, makeHunk as baseHunk } from "../test/factories";
import { toHunkPayload } from "./hunkPayload";

/** The mapping fixture: a rust file with a 4-line context/del/add/context hunk. */
function makeHunk(overrides: Partial<Hunk> = {}): Hunk {
  return baseHunk({
    header: "@@ -1,3 +1,3 @@ fn main()",
    lines: [
      makeDiffLine({ kind: "context", oldLineNo: 1, newLineNo: 1, content: "a" }),
      makeDiffLine({ kind: "del", oldLineNo: 2, newLineNo: null, content: "b" }),
      makeDiffLine({ kind: "add", oldLineNo: null, newLineNo: 2, content: "B" }),
      makeDiffLine({ kind: "context", oldLineNo: 3, newLineNo: 3, content: "c" }),
    ],
    ...overrides,
  });
}

function makeDiff(overrides: Partial<FileDiff> = {}): FileDiff {
  return baseDiff({ path: "src/main.rs", language: "rust", ...overrides });
}

describe("toHunkPayload", () => {
  it("maps path/header/line kind+content, drops line numbers, passes staged+contextLines through", () => {
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

    // staged flag and contextLines are carried through verbatim (folded from the
    // former one-line pass-through tests).
    expect(toHunkPayload(diff, hunk, true, 3).staged).toBe(true);
    expect(toHunkPayload(diff, hunk, false, 0).contextLines).toBe(0);
    expect(toHunkPayload(diff, hunk, false, 8).contextLines).toBe(8);
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
        makeDiffLine({ kind: "context", oldLineNo: 1, newLineNo: 1, content: "a\r" }),
        makeDiffLine({ kind: "del", oldLineNo: 2, newLineNo: null, content: "b\r" }),
        makeDiffLine({ kind: "add", oldLineNo: null, newLineNo: 2, content: "B\r" }),
      ],
    });

    const payload = toHunkPayload(makeDiff(), hunk, false, 3);

    expect(payload.lines.map((l) => l.content)).toEqual(["a\r", "b\r", "B\r"]);
  });

  it("carries noNewline marker lines through unchanged", () => {
    const hunk = makeHunk({
      lines: [
        makeDiffLine({ kind: "context", oldLineNo: 1, newLineNo: 1, content: "x" }),
        makeDiffLine({ kind: "add", oldLineNo: null, newLineNo: 2, content: "yEDIT" }),
        makeDiffLine({
          kind: "noNewline",
          oldLineNo: null,
          newLineNo: null,
          content: "\\ No newline at end of file",
        }),
      ],
    });

    const payload = toHunkPayload(makeDiff(), hunk, false, 3);

    expect(payload.lines).toContainEqual({
      kind: "noNewline",
      content: "\\ No newline at end of file",
    });
  });
});
