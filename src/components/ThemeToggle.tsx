import { useAppStore, type Theme } from "../store/useAppStore";

/** Cycle order and presentation for the three theme modes. */
const ORDER: Theme[] = ["system", "light", "dark"];
const LABEL: Record<Theme, string> = {
  system: "System",
  light: "Light",
  dark: "Dark",
};
// Non-emoji geometric glyphs so the control reads the same across platforms.
const GLYPH: Record<Theme, string> = {
  system: "◐",
  light: "○",
  dark: "●",
};

/**
 * Compact theme control: cycles system -> light -> dark on click. The title and
 * label announce the active mode and what the next click will switch to.
 */
export function ThemeToggle() {
  const theme = useAppStore((s) => s.theme);
  const setTheme = useAppStore((s) => s.setTheme);

  const next = ORDER[(ORDER.indexOf(theme) + 1) % ORDER.length];

  return (
    <button
      type="button"
      className="theme-toggle"
      title={`Theme: ${LABEL[theme]} (click for ${LABEL[next]})`}
      aria-label={`Theme: ${LABEL[theme]}. Activate to switch to ${LABEL[next]}.`}
      onClick={() => setTheme(next)}
    >
      <span className="theme-toggle__glyph" aria-hidden="true">
        {GLYPH[theme]}
      </span>
      <span className="theme-toggle__label">{LABEL[theme]}</span>
    </button>
  );
}
