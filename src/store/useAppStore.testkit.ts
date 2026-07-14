/**
 * Store Test Kit
 *
 * Helpers shared across the split `useAppStore.*.test.ts` files. These build the
 * store-specific shapes (a `FileSelection`, a status from bare path lists, the
 * canonical sample hunk) and the externally-resolvable `deferred` promise used to
 * pin response ordering. IPC-type builders live in `../test/factories`; this file
 * only adds the store-flavoured helpers on top.
 *
 * Not a `*.test.ts` file, so the vitest discovery glob ignores it.
 */
import type { FileSelection } from "./useAppStore";
import type { Hunk, RepoStatus } from "../types/ipc";
import { makeFileEntry, makeHunk, makeStatus } from "../test/factories";

/** A single-file (plain-click) selection for the given list. */
export function sel(path: string, staged = false): FileSelection {
  return { staged, paths: [path], anchorPath: path, focusedPath: path };
}

/** A status whose lists hold the given plain modified files, in order. */
export function statusWith(unstaged: string[], staged: string[] = []): RepoStatus {
  return makeStatus({
    unstaged: unstaged.map((p) => makeFileEntry({ path: p, staged: false })),
    staged: staged.map((p) => makeFileEntry({ path: p, staged: true })),
  });
}

/** The canonical 3-line context/del/add hunk (`a` / `b` -> `B`). */
export const sampleHunk: Hunk = makeHunk();

/** A promise whose resolution is controllable from the outside. */
export function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((_resolve) => {
    resolve = _resolve;
  });
  return { promise, resolve };
}
