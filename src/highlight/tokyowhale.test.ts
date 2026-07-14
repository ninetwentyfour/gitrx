/**
 * TokyoWhale Theme Tests
 *
 * Pins the one theme rule the real-shiki smoke test in `shiki.test.ts` does NOT
 * observe: comments must render italic. The identity fields and the keyword/string
 * colour literals were pruned — `shiki.test.ts` exercises those colours through a
 * real highlight, which supersedes asserting the raw theme table.
 *
 * Key behaviors:
 * - The `comment` scope is slate #708090 AND italic (fontStyle contract)
 *
 * See also:
 * - `shiki.test.ts` for real-shiki colour coverage (keyword mauve, string green)
 */
import { describe, expect, it } from "vitest";
import { tokyowhale } from "./tokyowhale";
import type { RawThemeSetting } from "shiki";

/** Does any tokenColor rule cover `scope` (either as its sole scope or in a list)? */
function findByScope(scope: string): RawThemeSetting | undefined {
  return tokyowhale.tokenColors?.find((rule) => {
    const s = rule.scope;
    if (typeof s === "string") return s === scope || s.split(",").includes(scope);
    return Array.isArray(s) && s.includes(scope);
  });
}

describe("tokyowhale theme", () => {
  it("renders comments in italic slate #708090", () => {
    const rule = findByScope("comment");
    expect(rule?.settings.foreground).toBe("#708090");
    expect(rule?.settings.fontStyle).toBe("italic");
  });
});
