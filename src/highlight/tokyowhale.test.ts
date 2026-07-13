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
  it("declares the expected identity and editor colors", () => {
    expect(tokyowhale.name).toBe("tokyowhale");
    expect(tokyowhale.type).toBe("dark");
    expect(tokyowhale.colors?.["editor.background"]).toBe("#0a0a0d");
    expect(tokyowhale.colors?.["editor.foreground"]).toBe("#a9b1d6");
  });

  it("maps keyword scopes to the mauve #C792EA", () => {
    const rule = findByScope("keyword");
    expect(rule?.settings.foreground).toBe("#C792EA");
  });

  it("maps string to the green #9ce88d", () => {
    const rule = findByScope("string");
    expect(rule?.settings.foreground).toBe("#9ce88d");
  });

  it("renders comments in italic slate #708090", () => {
    const rule = findByScope("comment");
    expect(rule?.settings.foreground).toBe("#708090");
    expect(rule?.settings.fontStyle).toBe("italic");
  });
});
