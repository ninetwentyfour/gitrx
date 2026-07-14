import { memo } from "react";
import type { Hunk } from "../types/ipc";
import type { DiffLineTokens } from "../highlight/shiki";
import { useAppStore } from "../store/useAppStore";
import { DiffLineRow } from "./DiffLineRow";

type HunkHeaderProps = {
  hunk: Hunk;
  /** Whether the parent diff is of the staged (index) version. */
  staged: boolean;
};

/** Shown on hunk actions disabled because the file is non-UTF-8 (lossy). */
const LOSSY_TITLE = "File contains non-UTF-8 text — use whole-file staging";

/**
 * A hunk's sticky header: the `@@` range text plus its action button group.
 *
 * An unstaged hunk offers [Discard][Stage]; a staged hunk offers [Unstage]. All
 * buttons are disabled while a mutation is in flight (`busy`) to avoid
 * overlapping index writes, AND when the current diff is `isLossy` (non-UTF-8):
 * per-hunk patches would corrupt such a file, so the user is steered to
 * whole-file staging via the button title. Extracted so both the plain and
 * virtualized diff renderers share one implementation.
 */
export function HunkHeader({ hunk, staged }: HunkHeaderProps) {
  const busy = useAppStore((s) => s.busy);
  const isLossy = useAppStore((s) => s.currentDiff?.isLossy ?? false);
  const stageHunk = useAppStore((s) => s.stageHunk);
  const unstageHunk = useAppStore((s) => s.unstageHunk);
  const discardHunk = useAppStore((s) => s.discardHunk);

  const disabled = busy || isLossy;
  const title = isLossy ? LOSSY_TITLE : undefined;

  return (
    <div className="hunk__header">
      <span className="hunk__range">{hunk.header}</span>
      <span className="hunk__actions">
        {staged ? (
          <button
            type="button"
            className="hunk__btn"
            disabled={disabled}
            title={title}
            onClick={() => void unstageHunk(hunk)}
          >
            Unstage
          </button>
        ) : (
          <>
            <button
              type="button"
              className="hunk__btn"
              disabled={disabled}
              title={title}
              onClick={() => void discardHunk(hunk)}
            >
              Discard
            </button>
            <button
              type="button"
              className="hunk__btn"
              disabled={disabled}
              title={title}
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

type HunkViewProps = {
  hunk: Hunk;
  /** Whether the parent diff is of the staged (index) version. */
  staged: boolean;
  /** Per-line syntax tokens for this hunk (index-aligned with `hunk.lines`). */
  tokens?: DiffLineTokens[] | undefined;
};

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
