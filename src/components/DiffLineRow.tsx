import { memo } from "react";
import type { DiffLine } from "../types/ipc";
import type { DiffLineTokens } from "../highlight/shiki";

interface DiffLineRowProps {
  line: DiffLine;
  /** Syntax-highlight tokens for this line, or `null`/undefined for plain text. */
  tokens?: DiffLineTokens;
}

const MARKER: Record<DiffLine["kind"], string> = {
  context: "",
  add: "+",
  del: "−", // minus sign
  noNewline: "",
};

/**
 * A single unified-diff row laid out as a CSS grid:
 * [old gutter][new gutter][marker][code]. Gutters are muted, right-aligned and
 * non-selectable; the code cell is monospaced `white-space: pre`. Horizontal
 * overflow is handled by the enclosing scroll container so all rows move as one.
 *
 * When `tokens` are supplied the code cell renders coloured `<span>`s (inline
 * colour so the add/del row background overlays read through); otherwise it
 * renders the plain content — minus a single trailing `\r`, which the diff layer
 * keeps for byte-exact round-tripping but must not render as a stray glyph.
 */
function DiffLineRowImpl({ line, tokens }: DiffLineRowProps) {
  const showOld = line.kind === "context" || line.kind === "del";
  const showNew = line.kind === "context" || line.kind === "add";

  const displayContent = line.content.endsWith("\r") ? line.content.slice(0, -1) : line.content;

  return (
    <div className="diff-line" data-kind={line.kind}>
      <span className="diff-line__gutter" aria-hidden="true">
        {showOld ? line.oldLineNo : ""}
      </span>
      <span className="diff-line__gutter" aria-hidden="true">
        {showNew ? line.newLineNo : ""}
      </span>
      <span className="diff-line__marker" aria-hidden="true">
        {MARKER[line.kind]}
      </span>
      <span className="diff-line__code">
        {tokens
          ? tokens.map((t, i) => (
              <span
                key={i}
                style={{
                  color: t.color,
                  fontStyle: t.fontStyle === "italic" ? "italic" : undefined,
                  fontWeight: t.fontStyle === "bold" ? "bold" : undefined,
                }}
              >
                {t.content}
              </span>
            ))
          : displayContent}
      </span>
    </div>
  );
}

export const DiffLineRow = memo(DiffLineRowImpl);
