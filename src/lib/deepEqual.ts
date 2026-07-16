/**
 * Recursive structural equality for the plain, JSON-shaped payloads the IPC layer
 * returns (`RepoStatus`, `FileDiff`, and their nested arrays/objects — see
 * `src/types/ipc.ts`). These contain only plain objects, arrays, and primitives:
 * no class instances, `Map`/`Set`, `Date`, or functions.
 *
 * Purpose: let watcher-driven (silent) refreshes skip a no-op store write when the
 * freshly-fetched payload is byte-for-byte the same as the one already held. On a
 * skip the store keeps the EXISTING object reference, so React sees no change and
 * `useDiffHighlight` (which re-tokenizes whenever the `diff` identity changes) does
 * not re-run the shiki pipeline. With multiple background windows churning on every
 * external git operation, that is the whole memory-leak fix.
 *
 * Deliberately NOT `JSON.stringify` equality: diffs can be multiple megabytes, and
 * serializing both sides would allocate two huge strings on every refresh — exactly
 * the allocation churn we are trying to avoid. This walks the structure and
 * short-circuits on the first difference.
 *
 * Semantics: `Object.is` for primitives (so `NaN` equals `NaN`, and a shared
 * reference short-circuits immediately); arrays compared by length then
 * element-wise; objects compared by own enumerable keys, where a key present with
 * value `undefined` is DISTINCT from an absent key, and `null` is distinct from
 * `undefined`.
 */
export function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || a === null || typeof b !== "object" || b === null) {
    return false;
  }

  const aArray = Array.isArray(a);
  const bArray = Array.isArray(b);
  if (aArray !== bArray) return false;

  if (aArray) {
    const arrA = a as unknown[];
    const arrB = b as unknown[];
    if (arrA.length !== arrB.length) return false;
    return arrA.every((item, i) => deepEqual(item, arrB[i]));
  }

  const objA = a as Record<string, unknown>;
  const objB = b as Record<string, unknown>;
  const keysA = Object.keys(objA);
  // A differing key count catches both extra AND missing keys (including a key
  // present with an `undefined` value on only one side).
  if (keysA.length !== Object.keys(objB).length) return false;
  return keysA.every((key) => Object.hasOwn(objB, key) && deepEqual(objA[key], objB[key]));
}
