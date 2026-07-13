import { memo } from "react";
import type { Hunk } from "../types/ipc";
import type { DiffLineTokens } from "../highlight/shiki";
import { useAppStore } from "../store/useAppStore";
import { DiffLineRow } from "./DiffLineRow";

interface HunkHeaderProps {
  hunk: Hunk;
  /** Whether the parent diff is of the staged (index) version. */
  staged: boolean;
}

/**
 * A hunk's sticky header: the `@@` range text plus its action button group.
 *
 * An unstaged hunk offers [Discard][Stage]; a staged hunk offers [Unstage]. All
 * buttons are disabled while a mutation is in flight (`busy`) to avoid
 * overlapping index writes. Extracted so both the plain and virtualized diff
 * renderers share one implementation.
 */
export function HunkHeader({ hunk, staged }: HunkHeaderProps) {
  const busy = useAppStore((s) => s.busy);
  const stageHunk = useAppStore((s) => s.stageHunk);
  const unstageHunk = useAppStore((s) => s.unstageHunk);
  const discardHunk = useAppStore((s) => s.discardHunk);

  return (
    <div className="hunk__header">
      <span className="hunk__range">{hunk.header}</span>
      <span className="hunk__actions">
        {staged ? (
          <button
            type="button"
            className="hunk__btn"
            disabled={busy}
            onClick={() => void unstageHunk(hunk)}
          >
            Unstage
          </button>
        ) : (
          <>
            <button
              type="button"
              className="hunk__btn"
              disabled={busy}
              onClick={() => void discardHunk(hunk)}
            >
              Discard
            </button>
            <button
              type="button"
              className="hunk__btn"
              disabled={busy}
              onClick={() => void stageHunk(hunk)}
            >
              Stage
            </button>
          </>
        )}
      </span>
    </div>
  );
}

interface HunkViewProps {
  hunk: Hunk;
  /** Whether the parent diff is of the staged (index) version. */
  staged: boolean;
  /** Per-line syntax tokens for this hunk (index-aligned with `hunk.lines`). */
  tokens?: DiffLineTokens[];
}

/**
 * One diff hunk: a sticky header with its action buttons followed by the hunk's
 * line rows.
 *
 * Memoised because a diff can contain many hunks and their props are stable
 * between context-line refetches for unaffected hunks.
 */
function HunkViewImpl({ hunk, staged, tokens }: HunkViewProps) {
  return (
    <div className="hunk">
      <HunkHeader hunk={hunk} staged={staged} />
      {hunk.lines.map((line, i) => (
        <DiffLineRow key={i} line={line} tokens={tokens?.[i]} />
      ))}
    </div>
  );
}

export const HunkView = memo(HunkViewImpl);
