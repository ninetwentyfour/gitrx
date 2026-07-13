import type { KeyboardEvent } from "react";
import { useAppStore } from "../store/useAppStore";

/**
 * Commit panel: message editor, amend toggle, and the Commit button.
 *
 * A commit is allowed when there is a non-blank message and either something is
 * staged or we are amending (amend permits a message-only edit). Cmd+Enter in
 * the textarea fires the commit when it is enabled — a GitX muscle-memory habit.
 */
export function CommitPanel() {
  const status = useAppStore((s) => s.status);
  const commitMessage = useAppStore((s) => s.commitMessage);
  const amend = useAppStore((s) => s.amend);
  const commitBusy = useAppStore((s) => s.commitBusy);
  const setCommitMessage = useAppStore((s) => s.setCommitMessage);
  const setAmend = useAppStore((s) => s.setAmend);
  const doCommit = useAppStore((s) => s.doCommit);

  const headHasCommits = status?.headHasCommits ?? false;
  const stagedEmpty = (status?.staged.length ?? 0) === 0;
  const messageBlank = commitMessage.trim() === "";
  const canCommit = !commitBusy && !messageBlank && (!stagedEmpty || amend);

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.metaKey && e.key === "Enter" && canCommit) {
      e.preventDefault();
      void doCommit();
    }
  };

  return (
    <section className="commit-panel">
      <h2 className="commit-panel__title">Commit</h2>
      <textarea
        className="commit-panel__message"
        placeholder="Commit message"
        aria-label="Commit message"
        value={commitMessage}
        onChange={(e) => setCommitMessage(e.target.value)}
        onKeyDown={onKeyDown}
      />
      <div className="commit-panel__actions">
        <label
          className="commit-panel__amend"
          title={
            headHasCommits
              ? "Replace the last commit with this one"
              : "Nothing to amend: this branch has no commits yet"
          }
        >
          <input
            type="checkbox"
            checked={amend}
            disabled={!headHasCommits}
            onChange={(e) => void setAmend(e.target.checked)}
          />{" "}
          Amend
        </label>
        <button
          type="button"
          className="commit-panel__commit"
          disabled={!canCommit}
          onClick={() => void doCommit()}
        >
          {amend ? "Amend Commit" : "Commit"}
        </button>
      </div>
    </section>
  );
}
