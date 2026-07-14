import { useMemo, type RefObject } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { FileDiff } from "../types/ipc";
import type { DiffTokens } from "../highlight/shiki";
import { DiffLineRow } from "./DiffLineRow";
import { HunkHeader } from "./HunkView";

type VirtualDiffProps = {
  diff: FileDiff;
  staged: boolean;
  tokens: DiffTokens | null;
  /** The scrolling ancestor (`.diff-viewer__body`) the virtualizer measures. */
  scrollRef: RefObject<HTMLDivElement | null>;
};

/** One flattened row of the virtual list: a hunk header or a single diff line. */
type Row =
  | { kind: "header"; hunkIndex: number }
  | { kind: "line"; hunkIndex: number; lineIndex: number };

// Rough row heights (px) for the initial layout; real heights are measured once
// each row mounts, so these only need to be in the right ballpark.
const HEADER_ESTIMATE = 24;
const LINE_ESTIMATE = 18;

/**
 * Virtualized diff renderer for very large diffs (>2000 lines). Hunk headers and
 * line rows are flattened into a single windowed list so only on-screen rows
 * exist in the DOM. Per-hunk Stage/Discard/Unstage buttons live on the header
 * rows and keep working. Headers are NOT sticky here (sticky fights an absolutely
 * positioned virtual list under WKWebView); this is the accepted trade-off for
 * huge diffs.
 */
export function VirtualDiff({ diff, staged, tokens, scrollRef }: VirtualDiffProps) {
  const rows = useMemo<Row[]>(() => {
    const flat: Row[] = [];
    diff.hunks.forEach((hunk, hunkIndex) => {
      flat.push({ kind: "header", hunkIndex });
      hunk.lines.forEach((_, lineIndex) => {
        flat.push({ kind: "line", hunkIndex, lineIndex });
      });
    });
    return flat;
  }, [diff]);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => (rows[i]?.kind === "header" ? HEADER_ESTIMATE : LINE_ESTIMATE),
    overscan: 24,
  });

  return (
    <div
      className="diff-viewer__content diff-viewer__content--virtual"
      style={{ height: virtualizer.getTotalSize(), position: "relative" }}
      data-virtualized="true"
    >
      {virtualizer.getVirtualItems().map((item) => {
        // `rows` is derived from `diff.hunks`, so every virtual index maps to a
        // real row/hunk; the guards narrow the `| undefined` from
        // noUncheckedIndexedAccess and are not expected to fire.
        const row = rows[item.index];
        if (!row) return null;
        const hunk = diff.hunks[row.hunkIndex];
        if (!hunk) return null;

        let content: React.ReactNode;
        if (row.kind === "header") {
          content = <HunkHeader hunk={hunk} staged={staged} />;
        } else {
          const line = hunk.lines[row.lineIndex];
          if (!line) return null;
          content = <DiffLineRow line={line} tokens={tokens?.[row.hunkIndex]?.[row.lineIndex]} />;
        }

        return (
          <div
            key={item.key}
            data-index={item.index}
            ref={virtualizer.measureElement}
            className="diff-viewer__vrow"
            style={{ transform: `translateY(${item.start}px)` }}
          >
            {content}
          </div>
        );
      })}
    </div>
  );
}
