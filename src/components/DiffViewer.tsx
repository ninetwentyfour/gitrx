import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useAppStore } from "../store/useAppStore";
import { readImage } from "../api/git";
import type { FileDiff } from "../types/ipc";
import { useDiffHighlight } from "../highlight/useDiffHighlight";
import { HunkView } from "./HunkView";
import { VirtualDiff } from "./VirtualDiff";
import "../styles/diff.css";

// Delay before the loading affordance appears, so fast (<150ms) refetches — the
// common case when nudging the context slider — do not flash a spinner.
const LOADING_DELAY_MS = 150;

// Above this total line count the diff renders through a windowed list; below it
// the simple (and sticky-header-friendly) direct render is used.
const VIRTUALIZE_THRESHOLD = 2000;

/** Total number of diff lines across all hunks. */
function totalLines(diff: FileDiff): number {
  let n = 0;
  for (const hunk of diff.hunks) n += hunk.lines.length;
  return n;
}

/**
 * Center pane diff viewer. Reads the current selection and diff from the store
 * and renders the appropriate state: empty (no selection), a binary notice, an
 * empty-changes notice, or the unified diff hunks.
 */
export function DiffViewer() {
  const selection = useAppStore((s) => s.selection);
  const diffLoading = useAppStore((s) => s.diffLoading);
  const currentDiff = useAppStore((s) => s.currentDiff);
  const focusedPath = selection?.focusedPath ?? null;

  const [showLoading, setShowLoading] = useState(false);
  useEffect(() => {
    if (!diffLoading) {
      setShowLoading(false);
      return;
    }
    const t = setTimeout(() => setShowLoading(true), LOADING_DELAY_MS);
    return () => clearTimeout(t);
  }, [diffLoading]);

  const bodyRef = useRef<HTMLDivElement>(null);

  if (!selection || focusedPath == null) {
    return (
      <div className="diff-viewer diff-viewer--empty">
        <p className="diff-viewer__empty">Select a file to view its diff</p>
      </div>
    );
  }

  const subtitle = `${selection.staged ? "Staged" : "Unstaged"} changes for ${focusedPath}`;
  const renamedFrom = currentDiff?.oldPath;

  return (
    <div className="diff-viewer">
      <div className="diff-viewer__toolbar">
        <span className="diff-viewer__subtitle" title={focusedPath}>
          {subtitle}
        </span>
        {renamedFrom && (
          <span className="diff-viewer__rename" title={renamedFrom}>
            renamed from {renamedFrom}
          </span>
        )}
        {showLoading && (
          <span className="diff-viewer__loading" aria-live="polite">
            Loading…
          </span>
        )}
      </div>
      <div className="diff-viewer__body" ref={bodyRef}>
        <DiffBody scrollRef={bodyRef} />
      </div>
    </div>
  );
}

/** Renders the scrollable diff content based on the current diff state. */
function DiffBody({ scrollRef }: { scrollRef: React.RefObject<HTMLDivElement | null> }) {
  const currentDiff = useAppStore((s) => s.currentDiff);
  const diffLoading = useAppStore((s) => s.diffLoading);
  const staged = useAppStore((s) => s.selection?.staged ?? false);

  const tokens = useDiffHighlight(currentDiff);

  const virtualized = useMemo(
    () => (currentDiff ? totalLines(currentDiff) > VIRTUALIZE_THRESHOLD : false),
    [currentDiff],
  );

  if (!currentDiff) {
    // Nothing to show yet; the toolbar surfaces any (delayed) loading hint.
    return null;
  }

  if (currentDiff.isBinary) {
    return <BinaryNotice path={currentDiff.path} staged={staged} />;
  }

  if (currentDiff.hunks.length === 0) {
    return <p className="diff-viewer__notice">{diffLoading ? "" : "No changes"}</p>;
  }

  if (virtualized) {
    return <VirtualDiff diff={currentDiff} staged={staged} tokens={tokens} scrollRef={scrollRef} />;
  }

  return (
    <div className="diff-viewer__content">
      {currentDiff.hunks.map((hunk, i) => (
        <HunkView key={i} hunk={hunk} staged={staged} tokens={tokens?.[i]} />
      ))}
    </div>
  );
}

/** Image extensions we render as an inline preview rather than a placeholder. */
const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif"]);

/** Lowercased extension of `path` (without the dot), or "" when none. */
function extensionOf(path: string): string {
  const dot = path.lastIndexOf(".");
  const slash = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return dot > slash && dot !== -1 ? path.slice(dot + 1).toLowerCase() : "";
}

type ImageState = { status: "loading" } | { status: "loaded"; src: string } | { status: "error" };

/**
 * Binary files cannot be hunk-staged, but a whole-file Stage/Unstage still makes
 * sense, so we surface those actions alongside the notice. For recognised image
 * types we additionally fetch and render an inline preview (working-tree version
 * when unstaged, index version when staged); on any fetch failure we quietly
 * fall back to the plain binary/oversized placeholder.
 */
function BinaryNotice({ path, staged }: { path: string; staged: boolean }) {
  const busy = useAppStore((s) => s.busy);
  const currentDiff = useAppStore((s) => s.currentDiff);
  const stageFile = useAppStore((s) => s.stageFile);
  const unstageFile = useAppStore((s) => s.unstageFile);

  const isImage = IMAGE_EXTS.has(extensionOf(path));
  const [image, setImage] = useState<ImageState>({ status: "loading" });

  // Keyed on `currentDiff` identity (not just path/staged) so an on-disk edit that
  // produces a NEW diff object for the SAME path re-fetches the preview instead of
  // showing the stale image. The stale-guard below still discards a late result if
  // the diff changes again mid-fetch.
  useEffect(() => {
    if (!isImage) return;
    // Stale-guard: if the selection changes mid-fetch, ignore the late result.
    let active = true;
    setImage({ status: "loading" });
    readImage(path, staged)
      .then(({ mimeType, base64 }) => {
        if (active) setImage({ status: "loaded", src: `data:${mimeType};base64,${base64}` });
      })
      .catch(() => {
        if (active) setImage({ status: "error" });
      });
    return () => {
      active = false;
    };
  }, [path, staged, isImage, currentDiff]);

  const showPreview = isImage && image.status !== "error";

  // Flattened out of a nested JSX ternary (readability; behaviour unchanged):
  // no preview → binary/oversized notice; preview + loading → spinner text;
  // otherwise the fetched image.
  let preview: ReactNode;
  if (!showPreview) {
    preview = (
      <p className="diff-viewer__notice">
        Binary or oversized file — showing no text diff. Use whole-file stage/unstage.
      </p>
    );
  } else if (image.status === "loading") {
    preview = <p className="diff-viewer__notice">Loading preview…</p>;
  } else {
    preview = (
      <div className="diff-viewer__image">
        <img className="diff-viewer__image-el" src={image.src} alt={path} />
      </div>
    );
  }

  return (
    <div className="diff-viewer__binary">
      <div className="diff-viewer__binary-actions">
        {staged ? (
          <button
            type="button"
            className="hunk__btn"
            disabled={busy}
            onClick={() => void unstageFile(path)}
          >
            Unstage
          </button>
        ) : (
          <button
            type="button"
            className="hunk__btn"
            disabled={busy}
            onClick={() => void stageFile(path)}
          >
            Stage
          </button>
        )}
      </div>
      {preview}
    </div>
  );
}
