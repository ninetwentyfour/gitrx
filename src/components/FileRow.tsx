import type { MouseEvent } from "react";
import type { FileEntry, FileStatus } from "../types/ipc";

interface FileRowProps {
  entry: FileEntry;
  selected: boolean;
  /** Row clicked; the handler reads modifier keys off the event. */
  onSelect: (event: MouseEvent<HTMLButtonElement>) => void;
  /** Row double-clicked → stage/unstage the current selection. */
  onActivate: () => void;
  /** Row right-clicked → open the native context menu. */
  onContextMenu: (event: MouseEvent<HTMLButtonElement>) => void;
}

const STATUS_LABEL: Record<FileStatus, string> = {
  modified: "M",
  added: "A",
  deleted: "D",
  renamed: "R",
  untracked: "U",
  conflicted: "C",
  typechange: "T",
};

/**
 * A single file entry: status badge, ellipsised path (filename kept visible),
 * and an additions/deletions badge. Clicking selects (plain/cmd/shift);
 * double-clicking stages or unstages the selection; right-clicking opens the
 * native context menu.
 */
export function FileRow({ entry, selected, onSelect, onActivate, onContextMenu }: FileRowProps) {
  const fullPath =
    entry.status === "renamed" && entry.oldPath ? `${entry.oldPath} → ${entry.path}` : entry.path;

  return (
    <li className="file-row">
      <button
        type="button"
        className={`file-row__btn${selected ? " is-selected" : ""}`}
        aria-current={selected ? "true" : undefined}
        onClick={onSelect}
        onDoubleClick={onActivate}
        onContextMenu={onContextMenu}
        title={fullPath}
      >
        <span className={`status-badge status-badge--${entry.status}`} aria-hidden="true">
          {STATUS_LABEL[entry.status]}
        </span>
        {/* The container is `direction: rtl` so overflow ellipsises the START of
            long paths (keeping the filename visible); the inner element is
            `unicode-bidi: plaintext` so the path's own characters stay in
            logical order and a leading dot (".gitignore") is not reordered. */}
        <span className="file-row__path">
          <bdi className="file-row__path-text">{fullPath}</bdi>
        </span>
        {!entry.isBinary && (entry.additions > 0 || entry.deletions > 0) && (
          <span className="file-row__stat" aria-hidden="true">
            <span className="file-row__add">+{entry.additions}</span>{" "}
            <span className="file-row__del">−{entry.deletions}</span>
          </span>
        )}
        {entry.isBinary && <span className="file-row__stat file-row__stat--binary">bin</span>}
      </button>
    </li>
  );
}
