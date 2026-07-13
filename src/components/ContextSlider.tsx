import { useAppStore } from "../store/useAppStore";

/**
 * Diff context-lines slider (0–8). Updates the store on change; the store
 * debounces the resulting diff refetch. The current value is shown numerically.
 */
export function ContextSlider() {
  const contextLines = useAppStore((s) => s.contextLines);
  const setContextLines = useAppStore((s) => s.setContextLines);

  return (
    <label className="context-slider">
      <span className="context-slider__label">Context: {contextLines}</span>
      <input
        type="range"
        min={0}
        max={8}
        value={contextLines}
        onChange={(e) => setContextLines(Number(e.target.value))}
        aria-label="Diff context lines"
      />
    </label>
  );
}
