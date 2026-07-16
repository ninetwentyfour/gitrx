import { useEffect } from "react";
import { orderedPaths, useAppStore } from "../store/useAppStore";

/**
 * True when the event originated inside a control that owns its own arrow-key
 * behavior: any `<input>` (crucially the diff context slider, an
 * `input[type=range]` whose arrows step the value), a `<textarea>` (the commit
 * message editor), a `<select>`, or a contenteditable region. We must leave those
 * keystrokes untouched.
 */
function isTextEntryTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  return target.closest("input, textarea, select") != null;
}

/**
 * Move roving DOM focus onto (and scroll into view) the row for `path` on the
 * given list side. The hook is global while the rows live inside two `FileList`
 * instances, so the row is targeted by its `data-path` + `data-staged` attributes
 * rather than a per-list ref map (which the hook could not reach). Iterating and
 * matching on the dataset avoids escaping arbitrary paths into a CSS selector.
 *
 * `focus()` is deliberately kept even though it is no longer load-bearing for
 * navigation: unlike mouse-click focus, PROGRAMMATIC focus works in WKWebView, so
 * this restores roving keyboard focus (a11y: the active row is the focused
 * element) at no cost.
 */
function revealRow(path: string, staged: boolean): void {
  const wanted = String(staged);
  for (const row of document.querySelectorAll<HTMLElement>("[data-path]")) {
    if (row.dataset["path"] === path && row.dataset["staged"] === wanted) {
      row.focus();
      row.scrollIntoView({ block: "nearest" });
      return;
    }
  }
}

/**
 * Global ArrowUp/ArrowDown file-list navigation. Mounted EXACTLY ONCE (in App) so
 * the two `FileList` instances can never double-handle a single keystroke.
 *
 * WHY a window listener rather than an `onKeyDown` on the list `<ul>`: WKWebView —
 * the WebKit engine Tauri renders in on macOS — does NOT move focus onto a
 * `<button>` on mouse click; focus stays on `document.body`. The original handler
 * relied on the clicked row button holding focus so the keydown would bubble up
 * through the `<ul>`, so in the real app arrows were dead after a click — the
 * keydown fired on `body` and never reached the list. jsdom DOES focus buttons on
 * click, which is exactly why the old tests passed against behavior the webview
 * lacks. A window listener fires wherever focus idles, so navigation works
 * whenever a selection exists (also a UX win over the original).
 *
 * Semantics match the shipped feature: plain arrow collapses to the neighbor,
 * Shift extends the range from the anchor, boundaries do not wrap, and OS chords
 * (Cmd/Ctrl/Alt + arrow) are left to the platform. `preventDefault` is called only
 * once we've decided the key is ours (a selection exists whose focused row is in
 * that list) so the page never scrolls on a handled arrow — including at a
 * boundary — while an arrow we ignore is left fully intact.
 */
export function useFileListKeyboardNav(): void {
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (isTextEntryTarget(event.target)) return;

      const { selection, status, selectFile } = useAppStore.getState();
      if (!selection) return;

      const order = orderedPaths(status, selection.staged);
      if (order.length === 0) return;

      const down = event.key === "ArrowDown";
      let nextIndex: number;
      if (selection.focusedPath == null) {
        // Focus somehow absent: land on the natural edge for the direction.
        nextIndex = down ? 0 : order.length - 1;
      } else {
        const current = order.indexOf(selection.focusedPath);
        if (current === -1) return; // focus not in this list's order — not ours.
        nextIndex = down ? current + 1 : current - 1;
      }

      // The key belongs to us now; own it (no page scroll) even at a boundary.
      event.preventDefault();

      // Boundaries do not wrap; the presentational untracked divider is absent from
      // `order`, so it is naturally skipped.
      if (nextIndex < 0 || nextIndex >= order.length) return;
      const nextPath = order[nextIndex];
      if (nextPath == null) return; // unreachable after the bounds check; satisfies TS.

      selectFile(nextPath, selection.staged, event.shiftKey ? { shift: true } : undefined);
      revealRow(nextPath, selection.staged);
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}
