/**
 * useAppStore — Core / Theme / Toasts Tests
 *
 * The store's lifecycle and cross-cutting concerns: `initialize` (theme + status
 * boot, no-repo handling), `openRepoViaPicker`, the monotonic stale-response
 * guards for status and diff, context-line debounce, theme persistence/mirroring,
 * the native window title, and the toast stack.
 *
 * Key behaviors:
 * - initialize reflects the backend-bound repo (or silent no-repo) and applies the
 *   persisted theme + window title; a non-"no repo" failure still toasts
 * - out-of-order status/diff responses are discarded by sequence number
 * - theme mirrors to <html> + native window and persists; toasts cap at 4 and TTL out
 *
 * See also:
 * - `useAppStore.selection.test.ts` for selection reconcile + staging/hunk mutations
 * - `useAppStore.commit.test.ts` for amend/commit
 * - `useAppStore.watcher.test.ts` for the filesystem-watch subscription
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FileDiff, RepoStatus } from "../types/ipc";
import { makeAppError, makeDiff, makeStatus } from "../test/factories";
import { deferred, sel, statusWith } from "./useAppStore.testkit";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn() }));

// Hoisted stable spies so we can assert the native window theme is mirrored and
// the native window title tracks the open repo.
const windowMocks = vi.hoisted(() => ({ setTheme: vi.fn(), setTitle: vi.fn() }));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTheme: windowMocks.setTheme, setTitle: windowMocks.setTitle }),
}));

// Hoisted so the vi.mock factory (itself hoisted) can reference the spies.
const storeMocks = vi.hoisted(() => ({
  get: vi.fn(),
  set: vi.fn(),
  save: vi.fn(),
  delete: vi.fn(),
  load: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-store", () => ({
  load: storeMocks.load,
}));

import { getDiff, getStatus, openRepo, pickRepoFolder } from "../api/git";
import { useAppStore } from "./useAppStore";

const mockGetStatus = vi.mocked(getStatus);
const mockGetDiff = vi.mocked(getDiff);
const mockOpenRepo = vi.mocked(openRepo);
const mockPickRepoFolder = vi.mocked(pickRepoFolder);

beforeEach(() => {
  useAppStore.setState({
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
  });

  // Native window theme + title mirroring resolve by default.
  windowMocks.setTheme.mockResolvedValue(undefined);
  windowMocks.setTitle.mockResolvedValue(undefined);

  // Default plugin-store behaviour: an empty settings store that accepts writes.
  storeMocks.get.mockResolvedValue(undefined);
  storeMocks.set.mockResolvedValue(undefined);
  storeMocks.save.mockResolvedValue(undefined);
  storeMocks.delete.mockResolvedValue(true);
  storeMocks.load.mockResolvedValue({
    get: storeMocks.get,
    set: storeMocks.set,
    save: storeMocks.save,
    delete: storeMocks.delete,
  });
});

afterEach(() => {
  vi.clearAllMocks();
  document.documentElement.removeAttribute("data-theme");
});

describe("useAppStore core", () => {
  it("initialize() shows no-repo state (no toast) when getStatus rejects", async () => {
    mockGetStatus.mockRejectedValueOnce(makeAppError());
    await useAppStore.getState().initialize();

    const state = useAppStore.getState();
    expect(state.status).toBeNull();
    expect(state.toasts).toEqual([]);
  });

  it("initialize() stores status on success", async () => {
    mockGetStatus.mockResolvedValueOnce(makeStatus());
    await useAppStore.getState().initialize();

    expect(useAppStore.getState().status?.repoName).toBe("repo");
  });

  // Reopen-on-launch and repo-path persistence moved entirely to the Rust
  // backend (per-window restore + `openRepos`), so `initialize()` no longer
  // writes `lastRepoPath` or tries to reopen a remembered repo: it just reflects
  // whatever repo the backend bound to this window.
  it("initialize() does NOT reopen a repo or write lastRepoPath (backend owns restore)", async () => {
    mockGetStatus.mockRejectedValueOnce(makeAppError());
    await useAppStore.getState().initialize();

    expect(mockOpenRepo).not.toHaveBeenCalled();
    expect(storeMocks.set).not.toHaveBeenCalledWith("lastRepoPath", expect.anything());
    expect(useAppStore.getState().status).toBeNull();
    expect(useAppStore.getState().toasts).toEqual([]);
  });

  it("initialize() does not persist on a successful status load", async () => {
    mockGetStatus.mockResolvedValueOnce(makeStatus({ repoPath: "/repos/live" }));
    await useAppStore.getState().initialize();

    expect(useAppStore.getState().status?.repoName).toBe("repo");
    expect(storeMocks.set).not.toHaveBeenCalledWith("lastRepoPath", expect.anything());
  });

  it("initialize() shows no-repo AND toasts a non-no-repo failure", async () => {
    // A "No repository open" rejection is the silent empty state; any OTHER error
    // still shows no-repo but must not be swallowed — it raises a toast.
    mockGetStatus.mockRejectedValueOnce("index corrupt");
    await useAppStore.getState().initialize();

    const state = useAppStore.getState();
    expect(state.status).toBeNull();
    expect(state.toasts.map((t) => t.message)).toContain("index corrupt");
  });

  it("openRepoViaPicker() is a no-op when the picker is cancelled", async () => {
    mockPickRepoFolder.mockResolvedValueOnce(null);
    await useAppStore.getState().openRepoViaPicker();

    expect(mockOpenRepo).not.toHaveBeenCalled();
    expect(useAppStore.getState().status).toBeNull();
  });

  it("openRepoViaPicker() surfaces backend errors as a toast", async () => {
    mockPickRepoFolder.mockResolvedValueOnce("/some/path");
    mockOpenRepo.mockRejectedValueOnce("boom");
    await useAppStore.getState().openRepoViaPicker();

    const { toasts, loading } = useAppStore.getState();
    expect(toasts.map((t) => t.message)).toContain("boom");
    expect(loading).toBe(false);
  });

  it("openRepoViaPicker() opens the repo without writing lastRepoPath (backend persists)", async () => {
    mockPickRepoFolder.mockResolvedValueOnce("/some/path");
    mockOpenRepo.mockResolvedValueOnce(makeStatus({ repoPath: "/repos/opened" }));

    await useAppStore.getState().openRepoViaPicker();

    expect(mockOpenRepo).toHaveBeenCalledWith("/some/path");
    expect(useAppStore.getState().status?.repoName).toBe("repo");
    // The Rust `open_repo` command binds + persists; the frontend no longer does.
    expect(storeMocks.set).not.toHaveBeenCalledWith("lastRepoPath", expect.anything());
  });

  it("openRepoViaPicker() surfaces a picker rejection as a toast and does not open", async () => {
    // The folder picker itself rejecting (dialog plugin error) must be caught: no
    // openRepo attempt, no loading flip, just a toast of the normalized message.
    mockPickRepoFolder.mockRejectedValueOnce(new Error("dialog unavailable"));

    await useAppStore.getState().openRepoViaPicker();

    expect(mockOpenRepo).not.toHaveBeenCalled();
    expect(useAppStore.getState().loading).toBe(false);
    expect(useAppStore.getState().toasts.map((t) => t.message)).toContain("dialog unavailable");
  });

  it("openRepoViaPicker() cancels a pending deferred collapse before opening", async () => {
    // A live deferred-collapse timer from the previous repo must not fire against
    // the freshly opened repo. The opened repo deliberately STILL contains b.txt,
    // so a surviving stale timer would wrongly collapse the selection onto it —
    // proving openRepoViaPicker's clearCollapseTimer, not an incidental path drop.
    vi.useFakeTimers();
    try {
      useAppStore.setState({ status: statusWith(["a.txt", "b.txt"]) });
      mockGetDiff.mockResolvedValue(makeDiff());
      const s = useAppStore.getState();
      s.selectFile("a.txt", false);
      s.selectFile("b.txt", false, { meta: true });
      s.selectFile("b.txt", false); // arms the deferred collapse

      mockPickRepoFolder.mockResolvedValueOnce("/some/path");
      mockOpenRepo.mockResolvedValueOnce(statusWith(["a.txt", "b.txt"]));
      await useAppStore.getState().openRepoViaPicker();

      // Opening reset the selection to null; the stale collapse must not revive it.
      expect(useAppStore.getState().selection).toBeNull();
      vi.runAllTimers();
      expect(useAppStore.getState().selection).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("setContextLines() debounces and refetches with the new value", async () => {
    vi.useFakeTimers();
    try {
      useAppStore.setState({ selection: sel("a.txt") });
      mockGetDiff.mockResolvedValue(makeDiff({ path: "a.txt" }));

      useAppStore.getState().setContextLines(6);
      useAppStore.getState().setContextLines(7); // supersedes the first

      expect(useAppStore.getState().contextLines).toBe(7);
      expect(mockGetDiff).not.toHaveBeenCalled(); // still debouncing

      await vi.advanceTimersByTimeAsync(120);

      expect(mockGetDiff).toHaveBeenCalledTimes(1);
      expect(mockGetDiff).toHaveBeenCalledWith("a.txt", false, 7);
    } finally {
      vi.useRealTimers();
    }
  });

  it("ignores a stale diff response that resolves after a newer one", async () => {
    useAppStore.setState({ selection: sel("a.txt") });

    const first = deferred<FileDiff>();
    const second = deferred<FileDiff>();
    mockGetDiff.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const p1 = useAppStore.getState().refreshDiff();
    const p2 = useAppStore.getState().refreshDiff();

    const newer = makeDiff({ path: "a.txt", language: "newer" });
    const older = makeDiff({ path: "a.txt", language: "older" });

    // Resolve the newer request first, then the stale older one.
    second.resolve(newer);
    await p2;
    first.resolve(older);
    await p1;

    expect(useAppStore.getState().currentDiff?.language).toBe("newer");
  });

  it("ignores a stale status response that resolves after a newer one", async () => {
    // Mirrors the diff-seq guard: a watcher getStatus started before a user
    // mutation could resolve LAST and clobber the post-mutation status (and
    // reconcileSelection would then wipe the followed selection). statusSeq must
    // discard the stale writer.
    const first = deferred<RepoStatus>();
    const second = deferred<RepoStatus>();
    mockGetStatus.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const p1 = useAppStore.getState().refreshStatus({ silent: true });
    const p2 = useAppStore.getState().refreshStatus({ silent: true });

    // The newer (second-issued) request resolves first and writes; the older one
    // resolves last and must be discarded.
    second.resolve(makeStatus({ branch: "newer" }));
    await p2;
    first.resolve(makeStatus({ branch: "older" }));
    await p1;

    expect(useAppStore.getState().status?.branch).toBe("newer");
  });
});

describe("silent refresh identity preservation", () => {
  // Watcher-driven (silent) refreshes that fetch an IDENTICAL payload must keep the
  // existing store object reference: a new-but-deep-equal object would change
  // identity and re-trigger React renders + the shiki re-highlight pipeline on
  // every background git event across every open window (the memory-leak fix).

  it("keeps the status reference when a silent refresh returns deep-equal data", async () => {
    const original = makeStatus();
    useAppStore.setState({ status: original, selection: null });
    // A fresh, structurally-identical object from the backend.
    mockGetStatus.mockResolvedValueOnce(makeStatus());

    await useAppStore.getState().refreshStatus({ silent: true });

    expect(useAppStore.getState().status).toBe(original);
    expect(useAppStore.getState().loading).toBe(false);
  });

  it("replaces the status reference when a silent refresh returns changed data", async () => {
    const original = makeStatus({ branch: "main" });
    useAppStore.setState({ status: original, selection: null });
    mockGetStatus.mockResolvedValueOnce(makeStatus({ branch: "feature" }));

    await useAppStore.getState().refreshStatus({ silent: true });

    expect(useAppStore.getState().status).not.toBe(original);
    expect(useAppStore.getState().status?.branch).toBe("feature");
  });

  it("keeps the currentDiff reference when a silent refresh returns a deep-equal diff", async () => {
    const original = makeDiff({ path: "a.txt", hunks: [] });
    useAppStore.setState({ selection: sel("a.txt"), currentDiff: original });
    mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt", hunks: [] }));

    await useAppStore.getState().refreshDiff({ silent: true });

    expect(useAppStore.getState().currentDiff).toBe(original);
    expect(useAppStore.getState().diffLoading).toBe(false);
  });

  it("replaces the currentDiff reference when a silent refresh returns a changed diff", async () => {
    const original = makeDiff({ path: "a.txt", language: null });
    useAppStore.setState({ selection: sel("a.txt"), currentDiff: original });
    mockGetDiff.mockResolvedValueOnce(makeDiff({ path: "a.txt", language: "rust" }));

    await useAppStore.getState().refreshDiff({ silent: true });

    expect(useAppStore.getState().currentDiff).not.toBe(original);
    expect(useAppStore.getState().currentDiff?.language).toBe("rust");
  });

  it("a non-silent status refresh always writes a fresh object even on equal data", async () => {
    // The identity-preserving skip is scoped to silent (watcher) refreshes; a
    // user-initiated refresh keeps its unconditional write path.
    const original = makeStatus();
    useAppStore.setState({ status: original, selection: null });
    mockGetStatus.mockResolvedValueOnce(makeStatus());

    await useAppStore.getState().refreshStatus();

    expect(useAppStore.getState().status).not.toBe(original);
  });
});

describe("theme", () => {
  it("setTheme('dark') sets data-theme and persists the choice", async () => {
    useAppStore.getState().setTheme("dark");

    expect(useAppStore.getState().theme).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    await vi.waitFor(() => expect(storeMocks.set).toHaveBeenCalledWith("theme", "dark"));
    expect(storeMocks.save).toHaveBeenCalled();
  });

  it("setTheme('system') removes the data-theme attribute", () => {
    document.documentElement.setAttribute("data-theme", "dark");

    useAppStore.getState().setTheme("system");

    expect(useAppStore.getState().theme).toBe("system");
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
  });

  it("mirrors the theme onto the native window: 'dark' -> 'dark', 'system' -> null", () => {
    useAppStore.getState().setTheme("dark");
    expect(windowMocks.setTheme).toHaveBeenCalledWith("dark");

    useAppStore.getState().setTheme("system");
    expect(windowMocks.setTheme).toHaveBeenCalledWith(null);
  });

  it("persistence failures never throw and do not block the UI", async () => {
    storeMocks.save.mockRejectedValueOnce("disk full");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    expect(() => useAppStore.getState().setTheme("light")).not.toThrow();
    expect(useAppStore.getState().theme).toBe("light");
    await vi.waitFor(() => expect(warn).toHaveBeenCalled());
    warn.mockRestore();
  });

  it("initialize() applies the persisted theme on startup", async () => {
    storeMocks.get.mockResolvedValueOnce("dark");
    mockGetStatus.mockRejectedValueOnce(makeAppError());

    await useAppStore.getState().initialize();

    expect(useAppStore.getState().theme).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  it("initialize() falls back to system when nothing is persisted", async () => {
    storeMocks.get.mockResolvedValueOnce(undefined);
    mockGetStatus.mockRejectedValueOnce(makeAppError());

    await useAppStore.getState().initialize();

    expect(useAppStore.getState().theme).toBe("system");
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
  });
});

describe("window title", () => {
  it("initialize() sets the title to '<repoName> — <branch>' on a status load", async () => {
    mockGetStatus.mockResolvedValueOnce(makeStatus({ repoName: "gitrx", branch: "main" }));

    await useAppStore.getState().initialize();

    expect(windowMocks.setTitle).toHaveBeenCalledWith("gitrx — main");
  });

  it("initialize() sets the title to 'gitrx' in the no-repo state", async () => {
    mockGetStatus.mockRejectedValueOnce(makeAppError());

    await useAppStore.getState().initialize();

    expect(windowMocks.setTitle).toHaveBeenCalledWith("gitrx");
  });

  it("refreshStatus() updates the title so a branch change is reflected", async () => {
    mockGetStatus.mockResolvedValueOnce(makeStatus({ repoName: "gitrx", branch: "feature/x" }));

    await useAppStore.getState().refreshStatus();

    expect(windowMocks.setTitle).toHaveBeenCalledWith("gitrx — feature/x");
  });

  it("openRepoViaPicker() sets the title for the freshly opened repo", async () => {
    mockPickRepoFolder.mockResolvedValueOnce("/some/path");
    mockOpenRepo.mockResolvedValueOnce(makeStatus({ repoName: "opened", branch: "dev" }));

    await useAppStore.getState().openRepoViaPicker();

    expect(windowMocks.setTitle).toHaveBeenCalledWith("opened — dev");
  });

  it("a title set failure never throws (swallowed like the theme mirror)", async () => {
    windowMocks.setTitle.mockRejectedValueOnce("no window");
    mockGetStatus.mockResolvedValueOnce(makeStatus());

    await expect(useAppStore.getState().initialize()).resolves.toBeUndefined();
  });
});

describe("toasts", () => {
  it("pushToast appends and caps the stack at 4, dropping the oldest", () => {
    const { pushToast } = useAppStore.getState();
    for (let i = 1; i <= 5; i++) pushToast(`msg ${i}`);

    const { toasts } = useAppStore.getState();
    expect(toasts).toHaveLength(4);
    expect(toasts.map((t) => t.message)).toEqual(["msg 2", "msg 3", "msg 4", "msg 5"]);
  });

  it("a toast auto-dismisses after 6s", () => {
    vi.useFakeTimers();
    try {
      useAppStore.getState().pushToast("boom");
      expect(useAppStore.getState().toasts).toHaveLength(1);

      vi.advanceTimersByTime(6000);

      expect(useAppStore.getState().toasts).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("dismissToast removes only the targeted toast", () => {
    const { pushToast } = useAppStore.getState();
    pushToast("first");
    pushToast("second");
    const [first] = useAppStore.getState().toasts;
    if (!first) throw new Error("expected at least one toast");

    useAppStore.getState().dismissToast(first.id);

    const messages = useAppStore.getState().toasts.map((t) => t.message);
    expect(messages).toEqual(["second"]);
  });
});
