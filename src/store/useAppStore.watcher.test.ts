/**
 * useAppStore — Filesystem Watcher Tests
 *
 * The `initWatcher` subscription lifecycle and the `repo-changed` / `watch-error`
 * handling. The watcher refreshes status (and the diff when a file is focused),
 * but must defer while a mutation/commit is in flight and coalesce into a single
 * trailing refresh — never dropping a change nor stacking listeners.
 *
 * Key behaviors:
 * - initWatcher subscribes exactly once, even under a re-entrant (StrictMode) call
 * - repo-changed refreshes status (+ diff if selected); skips while busy/commitBusy
 * - a change during a busy mutation runs exactly ONE trailing refresh afterwards,
 *   and the in-flight flag releases so later events still refresh
 * - watch-error logs, never toasts
 *
 * See also:
 * - `useAppStore.test.ts` for the stale-response sequence guards the watcher relies on
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { makeDiff, makeStatus } from "../test/factories";
import { deferred, sel } from "./useAppStore.testkit";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn() }));

// Per-window event subscription: the store listens via
// getCurrentWebviewWindow().listen so each window only refreshes for its repo.
const webviewMocks = vi.hoisted(() => ({ listen: vi.fn() }));

vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ listen: webviewMocks.listen }),
}));

import { getDiff, getStatus, stageFile } from "../api/git";
import { useAppStore } from "./useAppStore";

const mockGetStatus = vi.mocked(getStatus);
const mockGetDiff = vi.mocked(getDiff);
const mockStageFile = vi.mocked(stageFile);
const mockListen = webviewMocks.listen;

/**
 * Override `document.visibilityState` (a read-only getter in jsdom) so the
 * visibility-gated watcher paths can be exercised. Restored to "visible" after
 * each test.
 */
function setVisibility(state: "visible" | "hidden"): void {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => state,
  });
}

/** Fire the `visibilitychange` event the store's listener is wired to. */
function fireVisibilityChange(): void {
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  useAppStore.setState({
    status: null,
    selection: null,
    contextLines: 3,
    currentDiff: null,
    diffLoading: false,
    loading: false,
    busy: false,
    commitBusy: false,
    toasts: [],
  });
  useAppStore.getState().disposeWatcher();
  setVisibility("visible");
});

afterEach(() => {
  useAppStore.getState().disposeWatcher();
  setVisibility("visible");
  vi.clearAllMocks();
});

/** Capture the store's event handlers keyed by event name. */
function wireListen() {
  const handlers: Record<string, (event: { payload: unknown }) => void> = {};
  mockListen.mockImplementation((event: string, handler: (e: never) => void) => {
    handlers[event] = handler as (e: { payload: unknown }) => void;
    return Promise.resolve(() => {});
  });
  return handlers;
}

describe("initWatcher", () => {
  it("subscribes to repo-changed and watch-error", async () => {
    wireListen();
    await useAppStore.getState().initWatcher();

    expect(mockListen).toHaveBeenCalledWith("repo-changed", expect.any(Function));
    expect(mockListen).toHaveBeenCalledWith("watch-error", expect.any(Function));
  });

  it("concurrent initWatcher calls subscribe exactly once", async () => {
    wireListen();
    // Two overlapping calls (e.g. StrictMode double-invoke) reuse one in-flight
    // subscription rather than stacking a second listener pair.
    const a = useAppStore.getState().initWatcher();
    const b = useAppStore.getState().initWatcher();
    await Promise.all([a, b]);

    // One subscription = repo-changed + watch-error (2 calls), not 4.
    expect(mockListen).toHaveBeenCalledTimes(2);
  });

  it("a repo-changed event during a busy mutation runs exactly one trailing refresh after busy clears", async () => {
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    useAppStore.setState({ status: makeStatus(), selection: sel("a.txt") });

    // Hold the stage call open so `busy` stays true across the watcher event.
    const stageGate = deferred<void>();
    mockStageFile.mockReturnValueOnce(stageGate.promise);
    mockGetStatus.mockResolvedValue(makeStatus());
    mockGetDiff.mockResolvedValue(makeDiff({ path: "a.txt" }));

    const mutation = useAppStore.getState().stageFile("a.txt");
    // busy is true now; the event must be deferred, not run immediately.
    handlers["repo-changed"]?.({ payload: { reason: "fs" } });
    await Promise.resolve();
    expect(mockGetStatus).not.toHaveBeenCalled();

    // The mutation's own refresh (1) + exactly one trailing refresh (2).
    stageGate.resolve();
    await mutation;
    expect(mockGetStatus).toHaveBeenCalledTimes(2);

    // No further refreshes pile up.
    await Promise.resolve();
    expect(mockGetStatus).toHaveBeenCalledTimes(2);
  });

  it("consecutive repo-changed events each refresh (the in-flight flag releases)", async () => {
    // After a handleRepoChanged cycle completes, repoChangedInFlight must reset so
    // a later, independent event is NOT silently swallowed as still-in-flight.
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValue(makeStatus());

    handlers["repo-changed"]?.({ payload: { reason: "fs" } });
    await vi.waitFor(() => expect(mockGetStatus).toHaveBeenCalledTimes(1));

    handlers["repo-changed"]?.({ payload: { reason: "fs" } });
    await vi.waitFor(() => expect(mockGetStatus).toHaveBeenCalledTimes(2));
  });

  it("a repo-changed event refreshes status (no diff when nothing is selected)", async () => {
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValueOnce(makeStatus());

    handlers["repo-changed"]?.({ payload: { reason: "fs" } });

    await vi.waitFor(() => expect(mockGetStatus).toHaveBeenCalledTimes(1));
    expect(mockGetDiff).not.toHaveBeenCalled();
  });

  it("a repo-changed event also refreshes the diff when a file is selected", async () => {
    const handlers = wireListen();
    useAppStore.setState({
      status: makeStatus(),
      selection: sel("a.txt"),
    });
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt" }));

    handlers["repo-changed"]?.({ payload: { reason: "index" } });

    await vi.waitFor(() => expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 3));
    expect(mockGetStatus).toHaveBeenCalledTimes(1);
  });

  it("skips refresh while a mutation is busy", async () => {
    const handlers = wireListen();
    useAppStore.setState({ status: makeStatus(), busy: true });
    await useAppStore.getState().initWatcher();

    handlers["repo-changed"]?.({ payload: { reason: "index" } });
    await Promise.resolve();

    expect(mockGetStatus).not.toHaveBeenCalled();
  });

  it("skips refresh while a commit is in flight", async () => {
    const handlers = wireListen();
    useAppStore.setState({ status: makeStatus(), commitBusy: true });
    await useAppStore.getState().initWatcher();

    handlers["repo-changed"]?.({ payload: { reason: "head" } });
    await Promise.resolve();

    expect(mockGetStatus).not.toHaveBeenCalled();
  });

  it("defers a repo-changed event while the window is hidden, then refreshes once on visible", async () => {
    // A fully occluded/minimized window (visibilityState === "hidden") is the waste
    // case: it must NOT refresh on every backend event. The change is deferred and
    // a single trailing refresh runs when the window becomes visible again.
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValue(makeStatus());

    setVisibility("hidden");
    handlers["repo-changed"]?.({ payload: { reason: "fs" } });
    await Promise.resolve();
    await Promise.resolve();
    expect(mockGetStatus).not.toHaveBeenCalled();

    // Bringing the window back to the foreground runs exactly one catch-up refresh.
    setVisibility("visible");
    fireVisibilityChange();
    await vi.waitFor(() => expect(mockGetStatus).toHaveBeenCalledTimes(1));

    // No second refresh piles up from the single deferred event.
    await Promise.resolve();
    expect(mockGetStatus).toHaveBeenCalledTimes(1);
  });

  it("does not refresh on visibilitychange when nothing was deferred", async () => {
    // Becoming visible with no pending change must be a no-op — not a spurious
    // refresh every time the user tabs back to an already-current window.
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValue(makeStatus());

    setVisibility("visible");
    fireVisibilityChange();
    await Promise.resolve();

    expect(mockGetStatus).not.toHaveBeenCalled();
    expect(handlers["repo-changed"]).toBeTypeOf("function");
  });

  it("coalesces multiple hidden-window events into a single trailing refresh", async () => {
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValue(makeStatus());

    setVisibility("hidden");
    handlers["repo-changed"]?.({ payload: { reason: "fs" } });
    handlers["repo-changed"]?.({ payload: { reason: "index" } });
    handlers["repo-changed"]?.({ payload: { reason: "head" } });
    await Promise.resolve();
    expect(mockGetStatus).not.toHaveBeenCalled();

    setVisibility("visible");
    fireVisibilityChange();
    await vi.waitFor(() => expect(mockGetStatus).toHaveBeenCalledTimes(1));
    await Promise.resolve();
    expect(mockGetStatus).toHaveBeenCalledTimes(1);
  });

  it("refreshes immediately for a repo-changed event while the window is visible (regression)", async () => {
    // The visibility gate must not regress the normal foreground path: a visible
    // window still refreshes on the event itself, no visibilitychange required.
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValue(makeStatus());

    setVisibility("visible");
    handlers["repo-changed"]?.({ payload: { reason: "fs" } });

    await vi.waitFor(() => expect(mockGetStatus).toHaveBeenCalledTimes(1));
  });

  it("disposeWatcher unwires the visibilitychange listener", async () => {
    // The visibility listener shares the watcher's teardown; after disposal a
    // visible transition must not run a deferred refresh.
    const handlers = wireListen();
    await useAppStore.getState().initWatcher();
    mockGetStatus.mockResolvedValue(makeStatus());

    setVisibility("hidden");
    handlers["repo-changed"]?.({ payload: { reason: "fs" } });
    await Promise.resolve();

    useAppStore.getState().disposeWatcher();
    setVisibility("visible");
    fireVisibilityChange();
    await Promise.resolve();

    expect(mockGetStatus).not.toHaveBeenCalled();
  });

  it("watch-error is logged, not surfaced in the error banner", async () => {
    const handlers = wireListen();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    await useAppStore.getState().initWatcher();

    handlers["watch-error"]?.({ payload: "FSEvents blew up" });

    expect(warn).toHaveBeenCalled();
    expect(useAppStore.getState().toasts).toEqual([]);
    warn.mockRestore();
  });
});
