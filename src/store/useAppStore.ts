import { create } from "zustand";
import { confirm } from "@tauri-apps/plugin-dialog";
import { load } from "@tauri-apps/plugin-store";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  commit as apiCommit,
  discardFile as apiDiscardFile,
  discardHunk as apiDiscardHunk,
  getDiff,
  getHeadCommitMessage,
  getStatus,
  openRepo,
  pickRepoFolder,
  stageFile as apiStageFile,
  stageHunk as apiStageHunk,
  unstageFile as apiUnstageFile,
  unstageHunk as apiUnstageHunk,
} from "../api/git";
import { toHunkPayload } from "../lib/hunkPayload";
import { deepEqual } from "../lib/deepEqual";
import { isNoRepoError, toAppError } from "../lib/errors";
import { logDebug, logWarn } from "../lib/log";
import type { FileDiff, Hunk, RepoStatus } from "../types/ipc";

/** Above this diff line count a getDiff response is a memory suspect (warned). */
const DIFF_LINES_WARN = 5000;

/** What triggered a status/diff refresh, logged so refresh loops are traceable. */
type RefreshSource = "action" | "watcher" | "stale-drop";

/**
 * A per-list multi-selection. All selected files live on the same side
 * (`staged`); `focusedPath` is the one whose diff is shown (or `null` for none),
 * and `anchorPath` is where a shift-range extends from.
 */
export type FileSelection = {
  staged: boolean;
  /** Selected paths, held in the list's display order. */
  paths: string[];
  anchorPath: string | null;
  focusedPath: string | null;
};

/** Modifier keys accompanying a row click, controlling the selection gesture. */
export type SelectMods = {
  meta?: boolean;
  shift?: boolean;
};

/** Color-scheme preference. `system` follows `prefers-color-scheme`. */
export type Theme = "system" | "light" | "dark";

/** A transient, dismissible error notification shown bottom-right. */
export type Toast = {
  id: number;
  message: string;
};

/** How long a toast lingers before auto-dismissing. */
const TOAST_TTL_MS = 6000;
/** Maximum simultaneously-visible toasts; oldest is dropped past this. */
const TOAST_MAX = 4;

type RefreshOpts = {
  /** When true, failures are logged instead of raising a toast (used by the
   * filesystem watcher, whose refreshes are background noise, not user actions). */
  silent?: boolean;
  /** What triggered this refresh, for diagnostic logging. Defaults to `action`. */
  source?: RefreshSource;
};

type AppState = {
  status: RepoStatus | null;
  selection: FileSelection | null;
  contextLines: number;
  currentDiff: FileDiff | null;
  diffLoading: boolean;
  loading: boolean;
  /** True while an index/working-tree-mutating action is in flight. */
  busy: boolean;
  /** Color-scheme preference (persisted). */
  theme: Theme;
  /** Active error notifications, newest last. */
  toasts: Toast[];

  /** Current commit-message editor contents. */
  commitMessage: string;
  /** Whether the next commit amends HEAD instead of creating a new commit. */
  amend: boolean;
  /** True while a commit/amend is in flight (kept separate from `busy` so the
   * Commit button and the staging buttons don't visually disable each other). */
  commitBusy: boolean;
  /** The user's non-amend draft, snapshotted when amend is toggled ON so it can
   * be restored when they toggle it back OFF. */
  commitDraft: string;
  /** The message last prefilled from HEAD, so we can tell an untouched prefill
   * from a real user edit when re-toggling amend. */
  lastPrefill: string | null;

  initialize: () => Promise<void>;
  /** Subscribe to backend filesystem-watch events. Idempotent AND serialized: a
   * re-entrant call while one is in flight awaits/reuses the same subscription. */
  initWatcher: () => Promise<void>;
  /** Tear down the active watcher subscriptions (invoked by App's effect cleanup). */
  disposeWatcher: () => void;
  openRepoViaPicker: () => Promise<void>;
  refreshStatus: (opts?: RefreshOpts) => Promise<void>;
  /** Apply a click gesture to the selection of `staged`'s list. */
  selectFile: (path: string, staged: boolean, mods?: SelectMods) => void;
  /** Cancel a pending deferred-collapse timer (see {@link COLLAPSE_DELAY_MS}). */
  cancelPendingCollapse: () => void;
  setContextLines: (n: number) => void;
  refreshDiff: (opts?: RefreshOpts) => Promise<void>;
  stageFile: (path: string) => Promise<void>;
  unstageFile: (path: string) => Promise<void>;
  /** Stage many files sequentially, then refresh once (busy-guarded). */
  stageFiles: (paths: string[]) => Promise<void>;
  /** Unstage many files sequentially, then refresh once (busy-guarded). */
  unstageFiles: (paths: string[]) => Promise<void>;
  discardFile: (path: string) => Promise<void>;
  stageHunk: (hunk: Hunk) => Promise<void>;
  unstageHunk: (hunk: Hunk) => Promise<void>;
  discardHunk: (hunk: Hunk) => Promise<void>;
  setCommitMessage: (message: string) => void;
  setAmend: (on: boolean) => Promise<void>;
  doCommit: () => Promise<void>;
  /** Change (and persist) the color-scheme preference. */
  setTheme: (theme: Theme) => void;
  /** Push an error notification; auto-dismisses after {@link TOAST_TTL_MS}. */
  pushToast: (message: string) => void;
  /** Remove a toast by id (also used by the click-to-dismiss handler). */
  dismissToast: (id: number) => void;
};

/**
 * Normalise a thrown/rejected value into a displayable string. Delegates to
 * {@link toAppError} so a structured command rejection (`{ name, message }`)
 * surfaces its `message` rather than `String({...})` → `"[object Object]"`.
 */
export function toMessage(err: unknown): string {
  return toAppError(err).message;
}

/**
 * The paths of the given list, in display order. Exported so the window-level
 * keyboard-nav hook ({@link file://../hooks/useFileListKeyboardNav.ts}) derives a
 * list's arrow-navigation order from the exact same source the list renders from —
 * they cannot diverge.
 */
export function orderedPaths(status: RepoStatus | null, staged: boolean): string[] {
  if (!status) return [];
  return (staged ? status.staged : status.unstaged).map((f) => f.path);
}

/** The contiguous slice of `order` spanning `a`..`b` (inclusive, either way). */
function rangeBetween(order: string[], a: string, b: string): string[] {
  const i = order.indexOf(a);
  const j = order.indexOf(b);
  if (i === -1 || j === -1) return [b];
  const [lo, hi] = i <= j ? [i, j] : [j, i];
  return order.slice(lo, hi + 1);
}

/** A stable key for the focused file (list side + path), or null when none. */
function focusKey(selection: FileSelection | null): string | null {
  if (!selection || selection.focusedPath == null) return null;
  return `${selection.staged}:${selection.focusedPath}`;
}

/**
 * Reconcile a selection against a freshly-fetched status: drop paths that
 * vanished, repair a stale anchor, and clear the focus (signalling the diff must
 * go) when the focused file disappeared. An emptied selection collapses to null.
 */
function reconcileSelection(
  status: RepoStatus,
  selection: FileSelection | null,
): { next: FileSelection | null; focusCleared: boolean } {
  if (!selection) return { next: null, focusCleared: false };
  const present = new Set(orderedPaths(status, selection.staged));
  const paths = selection.paths.filter((p) => present.has(p));
  if (paths.length === 0) {
    return { next: null, focusCleared: selection.focusedPath != null };
  }
  const anchorPath =
    selection.anchorPath && present.has(selection.anchorPath) ? selection.anchorPath : null;
  let { focusedPath } = selection;
  let focusCleared = false;
  if (focusedPath != null && !present.has(focusedPath)) {
    focusedPath = null;
    focusCleared = true;
  }
  return { next: { staged: selection.staged, paths, anchorPath, focusedPath }, focusCleared };
}

// ---------------------------------------------------------------------------
// Theme persistence + application
// ---------------------------------------------------------------------------

const SETTINGS_STORE = "settings.json";
const THEME_KEY = "theme";

function isTheme(value: unknown): value is Theme {
  return value === "system" || value === "light" || value === "dark";
}

/**
 * Reflect the theme onto `<html>` AND the native window chrome. Explicit choices
 * set `data-theme`; `system` REMOVES the attribute so `prefers-color-scheme` (and
 * the diff-highlighter's MutationObserver fallback) take over. This attribute
 * contract is load-bearing.
 *
 * The same preference is mirrored onto the native window (title bar / macOS
 * traffic-light chrome) via `setTheme`: explicit choices map 1:1, while `system`
 * passes `null` so the OS drives the window theme. That call is fire-and-forget
 * and its rejection is swallowed with `console.warn` so it can never block the
 * UI or throw in the jsdom test environment (where no Tauri window exists).
 */
function applyThemeToDom(theme: Theme): void {
  const root = document.documentElement;
  if (theme === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", theme);
  }
  const nativeTheme = theme === "system" ? null : theme;
  void getCurrentWindow().setTheme(nativeTheme).catch(console.warn);
}

/**
 * Reflect the open repo onto the native window title: `"<repoName> — <branch>"`
 * when a repo is loaded, or the bare app name `"gitrx"` in the no-repo state.
 *
 * Mirrors {@link applyThemeToDom}'s native `setTheme` call: it is fire-and-forget
 * and its rejection is swallowed with `console.warn`, so it can never block the
 * UI or throw in the jsdom test environment (where no Tauri window exists). The
 * `core:window:allow-set-title` capability backs this call.
 */
function applyWindowTitle(status: RepoStatus | null): void {
  const title = status ? `${status.repoName} — ${status.branch}` : "gitrx";
  void getCurrentWindow().setTitle(title).catch(console.warn);
}

/**
 * Whether this window is currently occluded/minimized. On macOS WKWebView
 * `document.visibilityState` reflects NSWindow occlusion, so a side-by-side but
 * unfocused window still reports `"visible"` (and keeps refreshing — correct),
 * while a fully hidden or minimized window reports `"hidden"`. The watcher uses
 * this to defer refreshes for hidden windows, which is where the wasted
 * status/diff/shiki cycles pile up. Guarded for the jsdom/non-DOM environment.
 */
function isDocumentHidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

/** Read the persisted theme, or null if unset/unavailable. Never throws. */
async function loadPersistedTheme(): Promise<Theme | null> {
  try {
    const store = await load(SETTINGS_STORE);
    const value = await store.get(THEME_KEY);
    return isTheme(value) ? value : null;
  } catch (error) {
    console.warn("Failed to load persisted theme:", error);
    return null;
  }
}

/** Persist the theme preference. Failures are non-fatal (logged, not surfaced). */
async function persistTheme(theme: Theme): Promise<void> {
  try {
    const store = await load(SETTINGS_STORE);
    await store.set(THEME_KEY, theme);
    await store.save();
  } catch (error) {
    console.warn("Failed to persist theme:", error);
  }
}

// Open-repo persistence and reopen-on-launch now live entirely in the Rust
// backend (see `src-tauri/src/windows.rs`): it owns the per-window repo bindings,
// writes the `openRepos` set to `settings.json` on every window-lifecycle change,
// and restores all windows at startup. The frontend no longer writes
// `lastRepoPath` — a single writer avoids racing the backend for the same file.

// Debounce for context-line slider changes.
const CONTEXT_DEBOUNCE_MS = 120;
// A plain click on a member of a multi-selection does not collapse the selection
// immediately (that visibly flashes between the two clicks of a double-click); it
// schedules the collapse and lets a following dblclick — or any other interaction
// — cancel it.
const COLLAPSE_DELAY_MS = 250;

export const useAppStore = create<AppState>((set, get) => {
  // ---------------------------------------------------------------------------
  // Per-store internal state (relocated from module scope for testability: every
  // `create()` gets its own fresh copy, so store re-creation in tests starts
  // clean without manual resets of module globals).
  // ---------------------------------------------------------------------------

  // Monotonic sequences guarding against out-of-order responses: only the most
  // recently issued request is allowed to write its result to the store.
  let diffSeq = 0;
  let statusSeq = 0;
  // Pending debounce timer for context-line changes.
  let contextTimer: ReturnType<typeof setTimeout> | null = null;
  // Pending deferred-collapse timer (see COLLAPSE_DELAY_MS).
  let collapseTimer: ReturnType<typeof setTimeout> | null = null;
  // Monotonic toast id source.
  let toastSeq = 0;
  // Teardown callbacks for the active watcher subscriptions.
  let watcherUnlisteners: (() => void)[] = [];
  // The in-progress initWatcher() call, so a re-entrant call reuses it rather
  // than stacking a second subscription (React StrictMode double-invoke).
  let initWatcherPromise: Promise<void> | null = null;
  // Guard: a watcher-driven refresh is currently running.
  let repoChangedInFlight = false;
  // A `repo-changed` event arrived while a blocking condition (busy / commitBusy /
  // in-flight refresh) held; when that condition clears we run ONE trailing
  // refresh so a change during our own mutation is never silently dropped.
  let pendingRefresh = false;

  /** Cancel any pending deferred-collapse timer. Safe when none is armed. */
  function clearCollapseTimer(): void {
    if (collapseTimer) {
      clearTimeout(collapseTimer);
      collapseTimer = null;
    }
  }

  /**
   * Run the single trailing refresh a deferred `repo-changed` event asked for, but
   * only once no blocking condition holds. Clears `pendingRefresh` before running
   * so a fresh event during the refresh re-arms it rather than double-running.
   *
   * A hidden (occluded/minimized) window is also a blocking condition: the pending
   * refresh stays armed and the `visibilitychange` listener re-invokes this once the
   * window becomes visible again, so an occluded window catches up exactly once.
   */
  async function flushPendingRefresh(): Promise<void> {
    if (!pendingRefresh) return;
    if (get().busy || get().commitBusy || repoChangedInFlight) return;
    if (isDocumentHidden()) return;
    pendingRefresh = false;
    logDebug("watcher: running deferred trailing refresh");
    await get().refreshStatus({ silent: true, source: "watcher" });
    if (get().selection?.focusedPath != null) {
      await get().refreshDiff({ silent: true, source: "watcher" });
    }
  }

  /**
   * Handle a debounced `repo-changed` event from the backend: refresh status, and
   * the diff when a file is selected. The backend already debounces and de-dupes.
   *
   * If a blocking condition holds (our own mutation is `busy`, a commit is in
   * flight, a prior watcher refresh is still running, or this window is hidden) the
   * event is NOT dropped: it sets `pendingRefresh`, and the site that clears the
   * condition (a mutation finishing, or `visibilitychange` -> visible) runs one
   * trailing refresh. These refreshes are `silent`: a background failure logs, not toasts.
   */
  async function handleRepoChanged(reason: string): Promise<void> {
    const { busy, commitBusy } = get();
    const hidden = isDocumentHidden();
    if (busy || commitBusy || repoChangedInFlight || hidden) {
      // Not dropped — deferred to a single trailing refresh once we're idle and
      // visible again. A hidden window is the common waste case: it stops
      // refreshing entirely until the user brings it back to the foreground.
      logDebug(
        `watcher: repo-changed reason=${reason} deferred (busy=${busy} commitBusy=${commitBusy} inFlight=${repoChangedInFlight} hidden=${hidden})`,
      );
      pendingRefresh = true;
      return;
    }
    logDebug(`watcher: repo-changed reason=${reason} processing`);
    repoChangedInFlight = true;
    try {
      await get().refreshStatus({ silent: true, source: "watcher" });
      // Re-read: refreshStatus may have dropped a now-vanished focused file.
      if (get().selection?.focusedPath != null) {
        await get().refreshDiff({ silent: true, source: "watcher" });
      }
    } finally {
      repoChangedInFlight = false;
    }
    // An event that landed while we were in-flight left `pendingRefresh` set.
    await flushPendingRefresh();
  }

  /**
   * Stage/unstage a batch of files sequentially (the backend serialises anyway),
   * then refresh once. If the focused file is among the acted-on paths, its
   * selection follows it to the other list (same path, `staged` flipped) so the
   * diff pane tracks it; the remaining acted-on paths simply drop out via the
   * status reconcile.
   *
   * `fromStaged` is the side the files lived on before the mutation. Per-file
   * failures do not abort the batch: the first error message is captured and
   * toasted after the resync, and every file is still attempted.
   */
  async function mutateFilesAndFollow(
    paths: string[],
    fromStaged: boolean,
    mutate: (path: string) => Promise<void>,
  ): Promise<void> {
    if (paths.length === 0) return;
    // Re-entrancy guard: never let two whole-file mutations overlap.
    if (get().busy) return;

    const { selection } = get();
    const followed =
      selection != null &&
      selection.staged === fromStaged &&
      selection.focusedPath != null &&
      paths.includes(selection.focusedPath)
        ? selection.focusedPath
        : null;

    set({ busy: true });
    let errorMsg: string | null = null;
    for (const path of paths) {
      try {
        await mutate(path);
      } catch (error) {
        errorMsg ??= toMessage(error);
      }
    }

    // Move the focused file's selection to the other list before the reconcile so
    // it is preserved (it now lives there) and the diff refetch keys off it.
    if (followed != null) {
      set({
        selection: {
          staged: !fromStaged,
          paths: [followed],
          anchorPath: followed,
          focusedPath: followed,
        },
      });
    }
    await get().refreshStatus({ silent: true });
    if (followed != null) await get().refreshDiff({ silent: true });
    set({ busy: false });
    if (errorMsg) get().pushToast(errorMsg);
    // A watcher event during the mutation was deferred; run its one trailing refresh.
    await flushPendingRefresh();
  }

  /**
   * Run a single-hunk mutation against the current diff/selection, then resync.
   *
   * The payload is mapped from the *current* diff + `hunk` (unstaged -> `staged`
   * false, staged -> true). Whatever the outcome, both status and diff are
   * refreshed so the remaining hunks re-render and a stale click (the underlying
   * file having changed since it was rendered) self-heals. A captured mutation
   * failure is toasted AFTER the resync ("fail loudly, resync").
   *
   * `preBusied` is set by callers (discardHunk) that already flipped `busy` before
   * their own confirm dialog; those skip the re-entrancy guard and the busy-set
   * here so the single guard is not doubled.
   */
  async function applyHunk(
    hunk: Hunk,
    staged: boolean,
    mutate: (payload: ReturnType<typeof toHunkPayload>) => Promise<void>,
    preBusied = false,
  ): Promise<void> {
    // Re-entrancy guard: a second hunk action while one is in flight is a no-op,
    // so a double-click cannot apply the same hunk twice (the backend freshness
    // check would reject the second anyway, but this avoids the round-trip).
    if (!preBusied && get().busy) return;

    const { currentDiff, selection, contextLines } = get();
    if (!currentDiff || selection?.focusedPath == null) {
      // The selection vanished during a caller's confirm await; release the busy
      // flag the caller set so the UI does not wedge.
      if (preBusied) set({ busy: false });
      return;
    }

    const payload = toHunkPayload(currentDiff, hunk, staged, contextLines);
    if (!preBusied) set({ busy: true });

    let errorMsg: string | null = null;
    try {
      await mutate(payload);
    } catch (error) {
      errorMsg = toMessage(error);
    }

    // Resync unconditionally (success updates the remaining hunks; failure
    // recovers from a stale click). Silent so a resync hiccup does not bury the
    // captured mutation error, which is toasted last.
    await get().refreshStatus({ silent: true });
    await get().refreshDiff({ silent: true });
    set({ busy: false });
    if (errorMsg) get().pushToast(errorMsg);
    // A watcher event during the mutation was deferred; run its one trailing refresh.
    await flushPendingRefresh();
  }

  return {
    status: null,
    selection: null,
    contextLines: 3,
    currentDiff: null,
    diffLoading: false,
    loading: false,
    busy: false,
    theme: "system",
    toasts: [],
    commitMessage: "",
    amend: false,
    commitBusy: false,
    commitDraft: "",
    lastPrefill: null,

    initialize: async () => {
      // Apply the persisted theme first so the correct palette is in place before
      // (or with only a brief flash after) the first meaningful paint.
      const saved = await loadPersistedTheme();
      const theme = saved ?? "system";
      applyThemeToDom(theme);
      set({ theme });

      // The backend restores every window (binding each to its repo) before the
      // frontend boots, so `get_status` either returns this window's repo or
      // reports none. Reopen-on-launch is no longer the frontend's job.
      const seq = ++statusSeq;
      try {
        const status = await getStatus();
        if (seq !== statusSeq) return; // a newer status write superseded this one
        set({ status });
        applyWindowTitle(status);
      } catch (error) {
        if (seq !== statusSeq) return;
        // Always fall to the no-repo screen. A `noRepoOpen` rejection is the
        // expected empty state (plain launch, nothing to restore) and stays silent;
        // any OTHER failure ALSO shows no-repo but surfaces a toast so it is not lost.
        set({ status: null });
        applyWindowTitle(null);
        if (!isNoRepoError(error)) get().pushToast(toMessage(error));
      }
    },

    initWatcher: async () => {
      // Serialized: a re-entrant call while one subscription is being wired reuses
      // the in-flight promise instead of stacking a second listener pair.
      if (initWatcherPromise) return initWatcherPromise;

      initWatcherPromise = (async () => {
        // Idempotent: tear down any prior subscriptions before re-subscribing.
        for (const unlisten of watcherUnlisteners) unlisten();
        watcherUnlisteners = [];

        // Listen on THIS window's target only: each window's watcher emits
        // `repo-changed`/`watch-error` to its own label, so a change in one repo
        // refreshes only its window.
        const webview = getCurrentWebviewWindow();
        const unlistenChanged = await webview.listen<{ reason?: string }>(
          "repo-changed",
          (event) => {
            void handleRepoChanged(event.payload?.reason ?? "unknown");
          },
        );
        const unlistenError = await webview.listen<string>("watch-error", (event) => {
          // Watcher errors are non-fatal noise; keep them out of the user-facing
          // toasts and just log them.
          console.warn("File watcher error:", event.payload);
          logWarn(`watch-error: ${event.payload}`);
        });

        // When this window becomes visible again, run the single trailing refresh a
        // `repo-changed` event deferred while it was occluded/minimized. Wired
        // through `watcherUnlisteners` so it shares the watcher's idempotent
        // teardown (re-subscribing removes it first; StrictMode double-invoke and
        // disposeWatcher both clean it up).
        const onVisibilityChange = (): void => {
          if (document.visibilityState === "visible") void flushPendingRefresh();
        };
        const unlistenVisibility = (): void => {
          document.removeEventListener("visibilitychange", onVisibilityChange);
        };
        document.addEventListener("visibilitychange", onVisibilityChange);

        watcherUnlisteners = [unlistenChanged, unlistenError, unlistenVisibility];
      })();

      try {
        await initWatcherPromise;
      } finally {
        initWatcherPromise = null;
      }
    },

    disposeWatcher: () => {
      for (const unlisten of watcherUnlisteners) unlisten();
      watcherUnlisteners = [];
    },

    openRepoViaPicker: async () => {
      let path: string | null;
      try {
        path = await pickRepoFolder();
      } catch (error) {
        get().pushToast(toMessage(error));
        return;
      }
      if (path === null) return; // user cancelled

      set({ loading: true });
      const seq = ++statusSeq;
      try {
        const status = await openRepo(path);
        if (seq !== statusSeq) return; // a newer status write superseded this one
        // Opening a new repo invalidates any prior diff/selection.
        diffSeq++;
        clearCollapseTimer();
        set({
          status,
          selection: null,
          currentDiff: null,
          diffLoading: false,
          loading: false,
        });
        applyWindowTitle(status);
      } catch (error) {
        if (seq !== statusSeq) return;
        set({ loading: false });
        get().pushToast(toMessage(error));
      }
    },

    refreshStatus: async (opts) => {
      logDebug(`refreshStatus trigger source=${opts?.source ?? "action"}`);
      // A refresh may reshuffle/reconcile the selection out from under a pending
      // deferred collapse, so cancel it here.
      clearCollapseTimer();
      // Monotonic guard mirroring diffSeq: only the most recently issued status
      // request may write. Without this, a watcher `getStatus` started before a
      // user mutation could resolve LAST and overwrite the post-mutation status,
      // and reconcileSelection would then wipe the followed selection.
      const seq = ++statusSeq;
      set({ loading: true });
      try {
        const status = await getStatus();
        if (seq !== statusSeq) return; // a newer request superseded this one
        // Silent (watcher-driven) refresh returning an identical payload: keep the
        // existing `status` reference so nothing re-renders. When the status is
        // deep-equal, both side effects are provable no-ops — the selection was
        // already reconciled against this exact status (so `reconcileSelection`
        // would reproduce the same object and never clear focus) and the window
        // title derives purely from `repoName`/`branch` (unchanged). So a full
        // early-return is safe; we only release the `loading` flag we set above.
        if (opts?.silent && deepEqual(status, get().status)) {
          set({ loading: false });
          return;
        }
        const { next, focusCleared } = reconcileSelection(status, get().selection);
        // If the focused file disappeared, drop its stale diff too and cancel any
        // in-flight fetch for it.
        if (focusCleared) diffSeq++;
        set({
          status,
          selection: next,
          currentDiff: focusCleared ? null : get().currentDiff,
          loading: false,
        });
        // Branch changes arrive via watcher-driven refreshStatus, so the title
        // tracks the current branch naturally.
        applyWindowTitle(status);
      } catch (error) {
        if (seq !== statusSeq) return; // stale failure — the newer request owns loading
        set({ loading: false });
        if (opts?.silent) {
          console.warn("Background status refresh failed:", error);
        } else {
          get().pushToast(toMessage(error));
        }
      }
    },

    selectFile: (path, staged, mods) => {
      const { selection, status } = get();
      // Any explicit click gesture supersedes a pending deferred collapse.
      clearCollapseTimer();
      const sameList = selection != null && selection.staged === staged;
      const prevKey = focusKey(selection);

      // Deferred collapse: a plain click landing on a row that is already part of a
      // multi-selection on THIS list does NOT collapse immediately — that would
      // visibly flash the selection down to one row between the two clicks of a
      // double-click. Keep the full selection painted and schedule the collapse; a
      // following dblclick (which cancels via `cancelPendingCollapse`), or any
      // other click/refresh/list-switch, cancels it before it fires.
      if (
        !mods?.shift &&
        !mods?.meta &&
        sameList &&
        selection.paths.length > 1 &&
        selection.paths.includes(path)
      ) {
        collapseTimer = setTimeout(() => {
          collapseTimer = null;
          // The status may have changed while the timer was armed (a watcher refresh
          // could have dropped this row). If the clicked path no longer exists on
          // this list, cancel the collapse silently rather than pinning focus onto a
          // vanished path.
          if (!orderedPaths(get().status, staged).includes(path)) return;
          // Collapse to just the clicked row now. Re-read focus at fire time so the
          // diff only refetches when the focused file actually changes.
          const keyBefore = focusKey(get().selection);
          const collapsed: FileSelection = {
            staged,
            paths: [path],
            anchorPath: path,
            focusedPath: path,
          };
          set({ selection: collapsed });
          if (focusKey(collapsed) !== keyBefore) {
            set({ currentDiff: null });
            void get().refreshDiff();
          }
        }, COLLAPSE_DELAY_MS);
        return;
      }

      let next: FileSelection | null;
      if (mods?.shift && sameList && selection.anchorPath != null) {
        // Contiguous range from the anchor to the clicked row; anchor preserved.
        const range = rangeBetween(orderedPaths(status, staged), selection.anchorPath, path);
        next = { staged, paths: range, anchorPath: selection.anchorPath, focusedPath: path };
      } else if (mods?.meta && sameList) {
        // Toggle this row within the current selection.
        if (selection.paths.includes(path)) {
          const paths = selection.paths.filter((p) => p !== path);
          if (paths.length === 0) {
            next = null;
          } else {
            // Toggling off the focused row moves focus to the last remaining one.
            // `paths` is non-empty here (the length===0 case returned above), so
            // `lastPath` is always defined; `?? null` is unreachable and only
            // satisfies noUncheckedIndexedAccess.
            const lastPath = paths.at(-1) ?? null;
            const focusedPath = selection.focusedPath === path ? lastPath : selection.focusedPath;
            const anchorPath = selection.anchorPath === path ? focusedPath : selection.anchorPath;
            next = { staged, paths, anchorPath, focusedPath };
          }
        } else {
          // Focus follows the toggled-on file.
          next = { staged, paths: [...selection.paths, path], anchorPath: path, focusedPath: path };
        }
      } else {
        // Plain click (also the fallback for a modified click into the other list,
        // which clears that other list's selection): select only this file. A plain
        // click on a NON-member row collapses instantly — the selection visibly
        // moving to where you clicked is correct, not jank.
        next = { staged, paths: [path], anchorPath: path, focusedPath: path };
      }

      set({ selection: next });

      // Only refetch when the focused file actually changed. Clear the previous
      // diff first so the viewer does not flash stale content while loading.
      if (focusKey(next) !== prevKey) {
        if (next?.focusedPath == null) {
          diffSeq++;
          set({ currentDiff: null, diffLoading: false });
        } else {
          set({ currentDiff: null });
          void get().refreshDiff();
        }
      }
    },

    cancelPendingCollapse: () => {
      clearCollapseTimer();
    },

    setContextLines: (n) => {
      set({ contextLines: n });
      if (contextTimer) clearTimeout(contextTimer);
      contextTimer = setTimeout(() => {
        contextTimer = null;
        void get().refreshDiff();
      }, CONTEXT_DEBOUNCE_MS);
    },

    refreshDiff: async (opts) => {
      const { selection, contextLines } = get();
      const focusedPath = selection?.focusedPath ?? null;
      if (!selection || focusedPath == null) {
        diffSeq++;
        set({ currentDiff: null, diffLoading: false });
        return;
      }
      logDebug(`refreshDiff trigger source=${opts?.source ?? "action"} path=${focusedPath}`);
      const seq = ++diffSeq;
      set({ diffLoading: true });
      try {
        const diff = await getDiff(focusedPath, selection.staged, contextLines);
        if (seq !== diffSeq) return; // a newer request superseded this one
        // Silent (watcher-driven) refresh returning an identical diff: keep the
        // existing `currentDiff` reference so `useDiffHighlight` does NOT re-run the
        // shiki tokenizer (it re-tokenizes on `diff` identity change). The diffSeq
        // stale-guard above already ran, so this only skips the redundant write.
        if (opts?.silent && deepEqual(diff, get().currentDiff)) {
          set({ diffLoading: false });
          return;
        }
        const totalLines = diff.hunks.reduce((n, h) => n + h.lines.length, 0);
        if (totalLines > DIFF_LINES_WARN) {
          logWarn(
            `refreshDiff LARGE path=${focusedPath} lines=${totalLines} hunks=${diff.hunks.length} binary=${diff.isBinary}`,
          );
        }
        set({ currentDiff: diff, diffLoading: false });
      } catch (error) {
        if (seq !== diffSeq) return; // stale failure — ignore
        set({ diffLoading: false });
        if (opts?.silent) {
          console.warn("Background diff refresh failed:", error);
        } else {
          get().pushToast(toMessage(error));
        }
      }
    },

    stageFile: async (path) => {
      await mutateFilesAndFollow([path], false, apiStageFile);
    },

    unstageFile: async (path) => {
      await mutateFilesAndFollow([path], true, apiUnstageFile);
    },

    stageFiles: async (paths) => {
      await mutateFilesAndFollow(paths, false, apiStageFile);
    },

    unstageFiles: async (paths) => {
      await mutateFilesAndFollow(paths, true, apiUnstageFile);
    },

    discardFile: async (path) => {
      // Re-entrancy guard: a double-click must not raise two confirm dialogs.
      // `busy` is set BEFORE the confirm await (and cleared on cancel) so the
      // second click short-circuits here instead of opening a second prompt.
      if (get().busy) return;

      const { status } = get();
      const entry = status?.unstaged.find((f) => f.path === path);
      const isUntracked = entry?.status === "untracked";
      const message = isUntracked
        ? `Delete untracked file ${path}?`
        : `Discard all changes to ${path}? This cannot be undone.`;

      set({ busy: true });

      let confirmed: boolean;
      try {
        confirmed = await confirm(message);
      } catch (error) {
        set({ busy: false });
        get().pushToast(toMessage(error));
        return;
      }
      if (!confirmed) {
        set({ busy: false });
        return;
      }

      let errorMsg: string | null = null;
      try {
        await apiDiscardFile(path);
        await get().refreshStatus({ silent: true });
      } catch (error) {
        errorMsg = toMessage(error);
      } finally {
        set({ busy: false });
      }
      if (errorMsg) get().pushToast(errorMsg);
      // A watcher event during the discard was deferred; run its one trailing refresh.
      await flushPendingRefresh();
    },

    stageHunk: async (hunk) => {
      const { currentDiff } = get();
      // Untracked files must NOT go through the synthesized-patch path: a
      // hardcoded `new file mode 100644` patch drops exec bits and turns symlinks
      // into text blobs. An untracked diff is always a single all-add hunk, so
      // delegating to the whole-file stage is behavior-identical AND mode-safe
      // (mirrors how `discardHunk` delegates to `discardFile`).
      if (currentDiff?.isUntracked) {
        await get().stageFile(currentDiff.path);
        return;
      }
      await applyHunk(hunk, false, apiStageHunk);
    },

    unstageHunk: async (hunk) => {
      await applyHunk(hunk, true, apiUnstageHunk);
    },

    discardHunk: async (hunk) => {
      const { currentDiff, selection } = get();
      if (!currentDiff || selection?.focusedPath == null) return;

      // Untracked files have no per-hunk discard (no old blob to revert to), so
      // delegate to the whole-file delete. `discardFile` shows its own — single —
      // untracked confirmation, so we deliberately do NOT prompt here first. This
      // runs BEFORE we flip `busy` because discardFile has its own busy guard.
      if (currentDiff.isUntracked) {
        await get().discardFile(currentDiff.path);
        return;
      }

      // Re-entrancy guard mirroring discardFile: `busy` is flipped BEFORE the confirm
      // await so a rapid double-dispatch short-circuits here instead of opening a
      // second dialog. `applyHunk` is told (`preBusied`) not to re-check/re-set busy.
      if (get().busy) return;
      set({ busy: true });

      let confirmed: boolean;
      try {
        confirmed = await confirm(
          `Discard this hunk of ${currentDiff.path}? This cannot be undone.`,
        );
      } catch (error) {
        set({ busy: false });
        get().pushToast(toMessage(error));
        return;
      }
      if (!confirmed) {
        set({ busy: false });
        await flushPendingRefresh();
        return;
      }

      await applyHunk(hunk, false, apiDiscardHunk, true);
    },

    setCommitMessage: (message) => {
      set({ commitMessage: message });
    },

    setAmend: async (on) => {
      const { amend, commitMessage, lastPrefill } = get();
      if (on === amend) return;

      if (!on) {
        // Toggling OFF: drop back to the pre-amend draft, forget the prefill.
        set({ amend: false, commitMessage: get().commitDraft, lastPrefill: null });
        return;
      }

      // Toggling ON: remember the current draft so OFF can restore it.
      set({ amend: true, commitDraft: commitMessage });

      // Prefill from HEAD only when the box is empty or still shows an untouched
      // prior prefill — never clobber a real draft the user has typed.
      const untouched = commitMessage.trim() === "" || commitMessage === lastPrefill;
      if (!untouched) return;

      // Capture the box contents at fetch time so we can tell whether the user
      // typed during the (awaited) HEAD fetch.
      const captured = commitMessage;
      try {
        const headMessage = await getHeadCommitMessage();
        // The user may have toggled back off while this was in flight.
        if (!get().amend) return;
        // ...or typed into the box during the fetch — never clobber those keystrokes.
        if (get().commitMessage !== captured) return;
        set({ commitMessage: headMessage, lastPrefill: headMessage });
      } catch (error) {
        get().pushToast(toMessage(error));
      }
    },

    doCommit: async () => {
      // Guard against a double-click firing two commits/amends.
      if (get().commitBusy) return;
      const { commitMessage, amend } = get();
      set({ commitBusy: true });
      let errorMsg: string | null = null;
      try {
        await apiCommit(commitMessage, amend);
        set({ commitMessage: "", amend: false, commitDraft: "", lastPrefill: null });
        await get().refreshStatus({ silent: true });
      } catch (error) {
        errorMsg = toMessage(error);
      } finally {
        set({ commitBusy: false });
      }
      if (errorMsg) get().pushToast(errorMsg);
      // A watcher event during the commit was deferred; run its one trailing refresh.
      await flushPendingRefresh();
    },

    setTheme: (theme) => {
      set({ theme });
      applyThemeToDom(theme);
      void persistTheme(theme);
    },

    pushToast: (message) => {
      const id = ++toastSeq;
      // Append, then cap to the newest TOAST_MAX (dropping the oldest).
      set((s) => ({ toasts: [...s.toasts, { id, message }].slice(-TOAST_MAX) }));
      setTimeout(() => get().dismissToast(id), TOAST_TTL_MS);
    },

    dismissToast: (id) => {
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
    },
  };
});
