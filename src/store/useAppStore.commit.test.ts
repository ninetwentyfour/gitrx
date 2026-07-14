/**
 * useAppStore — Commit / Amend Tests
 *
 * The commit-message lifecycle: `setAmend` (draft snapshot/restore, HEAD prefill
 * with race-safe guards) and `doCommit` (success reset + refresh, error retention,
 * re-entrancy). Prefill must never clobber a real draft or keystrokes typed during
 * the in-flight HEAD fetch, and a mid-flight amend toggle-off must discard it.
 *
 * Key behaviors:
 * - setAmend(true) prefills from HEAD only when the box is empty/untouched; keeps a
 *   real draft and restores it on toggle-off
 * - prefill is discarded if the user types OR toggles amend OFF during the fetch
 * - doCommit success clears+resets+refreshes; failure retains the message; a second
 *   dispatch while committing is a no-op (commitBusy short-circuit)
 *
 * See also:
 * - `useAppStore.test.ts` for the shared toast plumbing errors surface through
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { makeStatus } from "../test/factories";
import { deferred } from "./useAppStore.testkit";

vi.mock("../api/git", async () => (await import("../test/factories")).mockGitApi());

vi.mock("@tauri-apps/plugin-dialog", () => ({ confirm: vi.fn() }));

import { commit, getHeadCommitMessage, getStatus } from "../api/git";
import { useAppStore } from "./useAppStore";

const mockCommit = vi.mocked(commit);
const mockGetHeadCommitMessage = vi.mocked(getHeadCommitMessage);
const mockGetStatus = vi.mocked(getStatus);

beforeEach(() => {
  useAppStore.setState({
    status: null,
    selection: null,
    currentDiff: null,
    busy: false,
    toasts: [],
    commitMessage: "",
    amend: false,
    commitBusy: false,
    commitDraft: "",
    lastPrefill: null,
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("setAmend", () => {
  it("setAmend prefill keeps keystrokes typed during the in-flight HEAD fetch", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "" });
    const gate = deferred<string>();
    mockGetHeadCommitMessage.mockReturnValueOnce(gate.promise);

    // Toggling ON with an empty box starts a HEAD prefill fetch.
    const p = useAppStore.getState().setAmend(true);
    // The user types into the box WHILE the fetch is in flight.
    useAppStore.getState().setCommitMessage("typed while loading");
    gate.resolve("HEAD subject\n\nbody\n");
    await p;

    // The resolved prefill must not clobber the user's keystrokes.
    expect(useAppStore.getState().commitMessage).toBe("typed while loading");
  });

  it("discards the prefill when amend is toggled OFF mid-flight", async () => {
    // Toggling amend OFF while the HEAD fetch is in flight must abandon the
    // prefill: the resolved message lands on a no-longer-amending box, so the
    // restored draft (empty here) wins and lastPrefill is not set.
    useAppStore.setState({ status: makeStatus(), commitMessage: "" });
    const gate = deferred<string>();
    mockGetHeadCommitMessage.mockReturnValueOnce(gate.promise);

    const p = useAppStore.getState().setAmend(true);
    // User cancels amend before HEAD resolves.
    await useAppStore.getState().setAmend(false);
    gate.resolve("HEAD subject\n\nbody\n");
    await p;

    expect(useAppStore.getState().amend).toBe(false);
    expect(useAppStore.getState().commitMessage).toBe("");
    expect(useAppStore.getState().lastPrefill).toBeNull();
  });

  it("setAmend(true) prefills the message from HEAD when the box is empty", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "" });
    mockGetHeadCommitMessage.mockResolvedValueOnce("previous subject\n\nbody\n");

    await useAppStore.getState().setAmend(true);

    expect(mockGetHeadCommitMessage).toHaveBeenCalled();
    expect(useAppStore.getState().amend).toBe(true);
    expect(useAppStore.getState().commitMessage).toBe("previous subject\n\nbody\n");
  });

  it("setAmend keeps a real draft, then restores it when toggled back off", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "my work in progress" });

    // Turning ON must NOT clobber a real draft (and must not fetch HEAD).
    await useAppStore.getState().setAmend(true);
    expect(mockGetHeadCommitMessage).not.toHaveBeenCalled();
    expect(useAppStore.getState().commitMessage).toBe("my work in progress");

    // Turning OFF restores the pre-amend draft.
    await useAppStore.getState().setAmend(false);
    expect(useAppStore.getState().amend).toBe(false);
    expect(useAppStore.getState().commitMessage).toBe("my work in progress");
  });

  it("setAmend(false) restores the pre-amend draft after a prefill", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "draft text" });
    mockGetHeadCommitMessage.mockResolvedValueOnce("HEAD message");

    // Empty? No — draft is real, so no prefill. Force the empty-box branch:
    useAppStore.setState({ commitMessage: "" });
    await useAppStore.getState().setAmend(true);
    expect(useAppStore.getState().commitMessage).toBe("HEAD message");

    await useAppStore.getState().setAmend(false);
    // Restores the (empty) draft that existed when amend was toggled on.
    expect(useAppStore.getState().commitMessage).toBe("");
  });
});

describe("doCommit", () => {
  it("doCommit() success clears the message, resets amend, and refreshes", async () => {
    useAppStore.setState({
      status: makeStatus(),
      commitMessage: "ship it",
      amend: false,
    });
    mockCommit.mockResolvedValueOnce({ oid: "abc123" });
    mockGetStatus.mockResolvedValueOnce(makeStatus({ unstaged: [], staged: [] }));

    await useAppStore.getState().doCommit();

    expect(mockCommit).toHaveBeenCalledWith("ship it", false);
    expect(useAppStore.getState().commitMessage).toBe("");
    expect(useAppStore.getState().amend).toBe(false);
    expect(useAppStore.getState().commitBusy).toBe(false);
    expect(mockGetStatus).toHaveBeenCalled();
  });

  it("doCommit() toasts the error and does not clear the message on failure", async () => {
    useAppStore.setState({ status: makeStatus(), commitMessage: "oops", amend: false });
    mockCommit.mockRejectedValueOnce("No staged changes to commit");

    await useAppStore.getState().doCommit();

    expect(useAppStore.getState().toasts.map((t) => t.message)).toContain(
      "No staged changes to commit",
    );
    expect(useAppStore.getState().commitMessage).toBe("oops");
    expect(useAppStore.getState().commitBusy).toBe(false);
  });

  it("a second doCommit while one is in flight is a no-op (commitBusy short-circuit)", async () => {
    // A double-click on Commit must not fire two commits. The first holds
    // commitBusy across an unresolved commit call; the second must short-circuit.
    useAppStore.setState({ status: makeStatus(), commitMessage: "ship it", amend: false });
    const gate = deferred<{ oid: string }>();
    mockCommit.mockReturnValueOnce(gate.promise);
    mockGetStatus.mockResolvedValue(makeStatus({ unstaged: [], staged: [] }));

    const first = useAppStore.getState().doCommit();
    const second = useAppStore.getState().doCommit(); // should short-circuit

    expect(mockCommit).toHaveBeenCalledTimes(1);

    gate.resolve({ oid: "abc123" });
    await Promise.all([first, second]);

    expect(mockCommit).toHaveBeenCalledTimes(1);
    expect(useAppStore.getState().commitBusy).toBe(false);
  });
});
