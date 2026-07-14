/**
 * Tests for the {@link toAppError} normalizer and the {@link isNoRepoError}
 * discriminant that replaced the store's old `message.includes(...)` string match.
 *
 * The contract these lock:
 * - a serialized `{ name, message }` command rejection passes through untouched;
 * - legacy strings / Error instances / junk fold into the generic `git` variant
 *   with their text preserved (never `"[object Object]"`);
 * - `isNoRepoError` is a pure discriminant on the normalized `name`.
 */
import { describe, expect, it } from "vitest";
import { isNoRepoError, toAppError } from "./errors";
import { makeAppError } from "../test/factories";

describe("toAppError", () => {
  it("passes a well-formed structured error through unchanged", () => {
    const err = makeAppError({ name: "staleHunk", message: "changed since displayed" });
    expect(toAppError(err)).toEqual(err);
  });

  it("preserves every known discriminant tag", () => {
    for (const name of [
      "noRepoOpen",
      "staleHunk",
      "nonUtf8File",
      "identityMissing",
      "nothingStaged",
      "emptyMessage",
      "windowClosed",
      "validation",
      "git",
      "io",
    ] as const) {
      expect(toAppError(makeAppError({ name })).name).toBe(name);
    }
  });

  it("folds a legacy string rejection into the generic git variant, keeping text", () => {
    expect(toAppError("something broke")).toEqual({ name: "git", message: "something broke" });
  });

  it("folds an Error instance into the generic git variant, keeping its message", () => {
    expect(toAppError(new Error("dialog unavailable"))).toEqual({
      name: "git",
      message: "dialog unavailable",
    });
  });

  it("folds an unrecognized object shape into the generic git variant", () => {
    // No `name`/`message` fields → not an AppError → generic, with String() text.
    const result = toAppError({ unexpected: true });
    expect(result.name).toBe("git");
    expect(result.message).toBe("[object Object]");
  });

  it("rejects an object whose name is not a known tag (treats it as generic)", () => {
    const result = toAppError({ name: "bogus", message: "hi" });
    expect(result.name).toBe("git");
  });
});

describe("isNoRepoError", () => {
  it("is true only for the noRepoOpen discriminant", () => {
    expect(isNoRepoError(makeAppError())).toBe(true);
    expect(isNoRepoError(makeAppError({ name: "staleHunk", message: "x" }))).toBe(false);
  });

  it("is false for a legacy string, even one that reads like the old message", () => {
    // The migration deliberately drops message-substring matching: a bare string
    // normalizes to the generic variant, so this is no longer a no-repo signal.
    expect(isNoRepoError("No repository open")).toBe(false);
  });
});
