import { useEffect, useState } from "react";
import type { FileDiff } from "../types/ipc";
import { type DiffTheme, type DiffTokens, highlightDiff } from "./shiki";

/**
 * Decide which diff theme is active RIGHT NOW. A manual `data-theme` attribute on
 * `<html>` (set by the app's theme toggle) wins; otherwise we fall back to the
 * OS `prefers-color-scheme` media query, mirroring how App.css chooses colours.
 */
function currentTheme(): DiffTheme {
  if (typeof document !== "undefined") {
    const attr = document.documentElement.getAttribute("data-theme");
    if (attr === "dark") return "tokyowhale";
    if (attr === "light") return "one-light";
  }
  const dark =
    typeof window !== "undefined" && window.matchMedia?.("(prefers-color-scheme: dark)").matches;
  return dark ? "tokyowhale" : "one-light";
}

/**
 * Highlight `diff` asynchronously and return its per-hunk/per-line token map, or
 * `null` while pending or when highlighting is unavailable (so the caller paints
 * plain text first and upgrades to colour when ready).
 *
 * Re-highlights when the diff changes or the active theme flips — the latter is
 * watched via a `matchMedia` listener (OS scheme) and a `MutationObserver` on the
 * `<html data-theme>` attribute (manual toggle). A stale-diff guard prevents a
 * slow highlight from overwriting a newer selection's tokens.
 */
export function useDiffHighlight(diff: FileDiff | null): DiffTokens | null {
  const [theme, setTheme] = useState<DiffTheme>(currentTheme);
  const [tokens, setTokens] = useState<DiffTokens | null>(null);

  useEffect(() => {
    const update = () => setTheme(currentTheme());
    const mql =
      typeof window === "undefined"
        ? undefined
        : window.matchMedia?.("(prefers-color-scheme: dark)");
    mql?.addEventListener("change", update);

    let observer: MutationObserver | undefined;
    if (typeof MutationObserver !== "undefined" && typeof document !== "undefined") {
      observer = new MutationObserver(update);
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });
    }
    return () => {
      mql?.removeEventListener("change", update);
      observer?.disconnect();
    };
  }, []);

  useEffect(() => {
    if (!diff) {
      setTokens(null);
      return;
    }
    let cancelled = false;
    // Paint plain first: clear any previous tokens so the diff renders
    // immediately, then upgrade once highlighting resolves.
    setTokens(null);
    highlightDiff(diff, theme)
      .then((result) => {
        if (!cancelled) setTokens(result);
      })
      .catch(() => {
        if (!cancelled) setTokens(null);
      });
    return () => {
      cancelled = true;
    };
  }, [diff, theme]);

  return tokens;
}
