/**
 * deepEqual Tests
 *
 * The recursive structural-equality helper used to skip no-op store writes on
 * watcher-driven refreshes (preserving object identity so React/shiki don't
 * re-run). Must treat the plain JSON-shaped IPC payloads correctly: nested
 * objects/arrays, `null` vs `undefined`, differing key sets, and shared refs.
 *
 * Key behaviors:
 * - primitives via Object.is (incl. NaN === NaN, +0 !== -0)
 * - deep nested objects/arrays compared structurally, first-difference short-circuit
 * - null vs undefined distinct; a key present-with-undefined differs from absent
 * - arrays vs objects, length/order mismatches
 */
import { describe, expect, it } from "vitest";
import { deepEqual } from "./deepEqual";

describe("deepEqual", () => {
  it("compares primitives via Object.is semantics", () => {
    expect(deepEqual(1, 1)).toBe(true);
    expect(deepEqual("a", "a")).toBe(true);
    expect(deepEqual(true, true)).toBe(true);
    expect(deepEqual(1, 2)).toBe(false);
    expect(deepEqual("a", "b")).toBe(false);
    expect(deepEqual(1, "1")).toBe(false);
    // Object.is: NaN equals NaN, and +0 differs from -0.
    expect(deepEqual(Number.NaN, Number.NaN)).toBe(true);
    expect(deepEqual(0, -0)).toBe(false);
  });

  it("treats null and undefined as distinct from each other and from objects", () => {
    expect(deepEqual(null, null)).toBe(true);
    expect(deepEqual(undefined, undefined)).toBe(true);
    expect(deepEqual(null, undefined)).toBe(false);
    expect(deepEqual(null, {})).toBe(false);
    expect(deepEqual({}, null)).toBe(false);
    expect(deepEqual(undefined, {})).toBe(false);
  });

  it("short-circuits on a shared reference", () => {
    const shared = { a: [1, 2, 3], b: { c: "x" } };
    expect(deepEqual(shared, shared)).toBe(true);
  });

  it("compares flat objects by value", () => {
    expect(deepEqual({ a: 1, b: 2 }, { a: 1, b: 2 })).toBe(true);
    // Key order does not matter.
    expect(deepEqual({ a: 1, b: 2 }, { b: 2, a: 1 })).toBe(true);
    expect(deepEqual({ a: 1, b: 2 }, { a: 1, b: 3 })).toBe(false);
  });

  it("distinguishes differing key sets", () => {
    expect(deepEqual({ a: 1 }, { b: 1 })).toBe(false);
    expect(deepEqual({ a: 1 }, { a: 1, b: 2 })).toBe(false);
    expect(deepEqual({ a: 1, b: 2 }, { a: 1 })).toBe(false);
  });

  it("treats a key present-with-undefined as distinct from an absent key", () => {
    expect(deepEqual({ a: undefined }, {})).toBe(false);
    expect(deepEqual({}, { a: undefined })).toBe(false);
    expect(deepEqual({ a: undefined }, { a: undefined })).toBe(true);
    // Same key count but different keys, one holding undefined.
    expect(deepEqual({ a: undefined, b: 1 }, { b: 1, c: 2 })).toBe(false);
  });

  it("compares arrays element-wise and by length", () => {
    expect(deepEqual([1, 2, 3], [1, 2, 3])).toBe(true);
    expect(deepEqual([], [])).toBe(true);
    expect(deepEqual([1, 2], [1, 2, 3])).toBe(false);
    expect(deepEqual([1, 2, 3], [1, 2])).toBe(false);
    // Order matters.
    expect(deepEqual([1, 2, 3], [3, 2, 1])).toBe(false);
  });

  it("does not conflate an array with a like-shaped object", () => {
    expect(deepEqual([1, 2], { 0: 1, 1: 2 })).toBe(false);
    expect(deepEqual({ 0: 1, 1: 2 }, [1, 2])).toBe(false);
    // An empty array and an empty object have the same key count (0) but differ.
    expect(deepEqual([], {})).toBe(false);
  });

  it("recurses through nested objects and arrays", () => {
    const a = {
      repoName: "repo",
      branch: "main",
      unstaged: [{ path: "a.txt", additions: 1, oldPath: undefined }],
      staged: [],
      headHasCommits: true,
    };
    const b = {
      repoName: "repo",
      branch: "main",
      unstaged: [{ path: "a.txt", additions: 1, oldPath: undefined }],
      staged: [],
      headHasCommits: true,
    };
    expect(deepEqual(a, b)).toBe(true);

    // A single deep leaf change is detected.
    const c = structuredClone(b);
    c.unstaged[0]!.additions = 2;
    expect(deepEqual(a, c)).toBe(false);

    // A change in branch (a shallow field) is detected.
    const d = structuredClone(b);
    d.branch = "dev";
    expect(deepEqual(a, d)).toBe(false);

    // A nested array-length change is detected.
    const e = { ...structuredClone(b), staged: [{ path: "b.txt", additions: 0 }] };
    expect(deepEqual(a, e)).toBe(false);
  });
});
