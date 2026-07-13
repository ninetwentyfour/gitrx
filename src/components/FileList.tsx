import { useEffect } from "react";
import type { MouseEvent } from "react";
import type { FileEntry } from "../types/ipc";
import { useAppStore } from "../store/useAppStore";
import { showFileContextMenu } from "../api/git";
import { FileRow } from "./FileRow";

interface FileListProps {
  title: string;
  files: FileEntry[];
  staged: boolean;
}

/**
 * A titled, scrollable list of file changes with an empty-state fallback.
 * Owns the click/double-click/right-click gestures for its list, delegating the
 * resulting selection and staging intent to the store.
 */
export function FileList({ title, files, staged }: FileListProps) {
  const selection = useAppStore((s) => s.selection);
  const selectFile = useAppStore((s) => s.selectFile);
  const cancelPendingCollapse = useAppStore((s) => s.cancelPendingCollapse);
  const stageFiles = useAppStore((s) => s.stageFiles);
  const unstageFiles = useAppStore((s) => s.unstageFiles);
  const pushToast = useAppStore((s) => s.pushToast);

  // Only this list's selection paints; the other list's is elsewhere.
  const selectedPaths = selection && selection.staged === staged ? selection.paths : [];

  // Unmounting drops the DOM a pending deferred collapse would have acted on, so
  // cancel it (also covers a list switch that swaps this list out).
  useEffect(() => cancelPendingCollapse, [cancelPendingCollapse]);

  const handleSelect = (path: string) => (event: MouseEvent<HTMLButtonElement>) => {
    // A double-click fires onClick twice before onDblClick; ignore the extra
    // clicks so a range/toggle is not recomputed mid-gesture.
    if (event.detail > 1) return;
    selectFile(path, staged, { meta: event.metaKey, shift: event.shiftKey });
  };

  const handleActivate = (path: string) => () => {
    // A plain click on a member of a multi-selection defers (does not flash) the
    // collapse; cancel that pending collapse so the double-click acts on the
    // still-intact full selection. Act on the whole selection when the row is
    // part of it, otherwise just the (already-collapsed) clicked row.
    cancelPendingCollapse();
    const paths = selectedPaths.includes(path) ? selectedPaths : [path];
    if (staged) void unstageFiles(paths);
    else void stageFiles(paths);
  };

  const handleContextMenu = (path: string) => (event: MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    // Right-clicking outside the selection first selects the row (plain click).
    let paths = selectedPaths;
    if (!paths.includes(path)) {
      selectFile(path, staged);
      paths = [path];
    }
    showFileContextMenu(paths, staged).catch((err) => pushToast(String(err)));
  };

  return (
    <section className="file-list">
      <h2 className="file-list__title">
        {title}
        <span className="file-list__count">{files.length}</span>
      </h2>
      {files.length === 0 ? (
        <p className="file-list__empty">No changes</p>
      ) : (
        <ul className="file-list__items">
          {files.map((entry) => (
            <FileRow
              key={entry.path}
              entry={entry}
              selected={selectedPaths.includes(entry.path)}
              onSelect={handleSelect(entry.path)}
              onActivate={handleActivate(entry.path)}
              onContextMenu={handleContextMenu(entry.path)}
            />
          ))}
        </ul>
      )}
    </section>
  );
}
