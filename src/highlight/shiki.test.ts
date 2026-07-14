/**
 * Shiki Diff Highlighting Tests
 *
 * The diff-to-highlight bridge: `reconstructBlobs` (rebuild old/new text + per-line
 * attribution from a hunk list, stripping CRLF), the size guards that bail to
 * plain text, and a real-shiki smoke test proving the custom TokyoWhale theme
 * registers (keyword mauve + string green resolve).
 *
 * Key behaviors:
 * - reconstructBlobs splits context/del/add across old/new blobs and strips `\r`
 * - highlightDiff returns null over the line/char/unknown-language guards
 * - real shiki resolves the registered theme's keyword + string colours
 */
import { describe, expect, it } from "vitest";
import { highlightDiff, reconstructBlobs } from "./shiki";
import { makeDiff, makeHunk } from "../test/factories";
import type { DiffLine, FileDiff, Hunk } from "../types/ipc";

function hunk(header: string, lines: DiffLine[]): Hunk {
  return makeHunk({ header, oldLines: 0, newLines: 0, lines });
}

function diff(hunks: Hunk[], language: string | null = "typescript"): FileDiff {
  return makeDiff({ path: "f.ts", language, hunks });
}

describe("reconstructBlobs", () => {
  it("rebuilds old/new text and per-line refs across two hunks, stripping CRLF", () => {
    const d = diff([
      hunk("@@ -1,2 +1,2 @@", [
        { kind: "context", oldLineNo: 1, newLineNo: 1, content: "a" },
        { kind: "del", oldLineNo: 2, newLineNo: null, content: "b" },
        { kind: "add", oldLineNo: null, newLineNo: 2, content: "B" },
      ]),
      hunk("@@ -3,1 +3,2 @@", [
        { kind: "context", oldLineNo: 3, newLineNo: 3, content: "c\r" }, // CRLF artifact
        { kind: "add", oldLineNo: null, newLineNo: 4, content: "d" },
        {
          kind: "noNewline",
          oldLineNo: null,
          newLineNo: null,
          content: "\\ No newline at end of file",
        },
      ]),
    ]);

    const { oldText, newText, refs } = reconstructBlobs(d);

    // OLD = context + del lines; NEW = context + add lines. The `\r` is stripped.
    expect(oldText).toBe("a\nb\nc");
    expect(newText).toBe("a\nB\nc\nd");

    // Hunk 1 attribution.
    expect(refs[0]?.[0]).toEqual({ old: 0, new: 0 }); // context
    expect(refs[0]?.[1]).toEqual({ old: 1, new: -1 }); // del -> old only
    expect(refs[0]?.[2]).toEqual({ old: -1, new: 1 }); // add -> new only

    // Hunk 2 attribution: the CRLF context line lands at old[2]/new[2].
    expect(refs[1]?.[0]).toEqual({ old: 2, new: 2 });
    expect(refs[1]?.[1]).toEqual({ old: -1, new: 3 }); // add "d"
    expect(refs[1]?.[2]).toEqual({ old: -1, new: -1 }); // noNewline marker: no blob line
  });
});

describe("highlightDiff size guards", () => {
  it("returns null when the total line count exceeds 5000", async () => {
    const lines: DiffLine[] = Array.from({ length: 5001 }, (_, i) => ({
      kind: "context" as const,
      oldLineNo: i + 1,
      newLineNo: i + 1,
      content: "x",
    }));
    const result = await highlightDiff(diff([hunk("@@", lines)]), "tokyowhale");
    expect(result).toBeNull();
  });

  it("returns null when a single line is longer than 1000 chars", async () => {
    const long: DiffLine = {
      kind: "add",
      oldLineNo: null,
      newLineNo: 1,
      content: "y".repeat(1001),
    };
    const result = await highlightDiff(diff([hunk("@@", [long])]), "tokyowhale");
    expect(result).toBeNull();
  });

  it("returns null for a language shiki does not know", async () => {
    const d = diff(
      [hunk("@@", [{ kind: "add", oldLineNo: null, newLineNo: 1, content: "hi" }])],
      "this-is-not-a-language",
    );
    expect(await highlightDiff(d, "tokyowhale")).toBeNull();
  });

  it("returns null when language is null", async () => {
    const d = diff(
      [hunk("@@", [{ kind: "add", oldLineNo: null, newLineNo: 1, content: "hi" }])],
      null,
    );
    expect(await highlightDiff(d, "tokyowhale")).toBeNull();
  });
});

describe("highlightDiff (real shiki, proves theme registration)", () => {
  it("highlights a small TS fixture with TokyoWhale colours", async () => {
    const d = diff([
      hunk("@@ -1,1 +1,2 @@", [
        { kind: "context", oldLineNo: 1, newLineNo: 1, content: 'const s = "hi";' },
        { kind: "add", oldLineNo: null, newLineNo: 2, content: "const n = 42;" },
      ]),
    ]);

    const result = await highlightDiff(d, "tokyowhale");
    expect(result).not.toBeNull();

    // Flatten every token across every hunk/line.
    const colors = new Set<string>();
    for (const hunkTokens of result!) {
      for (const lineTokens of hunkTokens) {
        for (const token of lineTokens ?? []) colors.add(token.color.toUpperCase());
      }
    }

    // `const` must resolve to the keyword mauve, and the string to the green —
    // which only happens if the custom theme registration was accepted by shiki.
    expect(colors).toContain("#C792EA");
    expect(colors).toContain("#9CE88D");
  }, 20_000);
});
