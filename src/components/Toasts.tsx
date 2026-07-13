import { useAppStore } from "../store/useAppStore";

/**
 * Bottom-right stack of error notifications. Each toast auto-dismisses after a
 * few seconds (handled in the store) and can be clicked to dismiss immediately.
 * Rendered once at the app root so it overlays both the repo and no-repo views.
 */
export function Toasts() {
  const toasts = useAppStore((s) => s.toasts);
  const dismissToast = useAppStore((s) => s.dismissToast);

  if (toasts.length === 0) return null;

  return (
    <div className="toasts" aria-live="polite" aria-label="Notifications">
      {toasts.map((toast) => (
        <button
          key={toast.id}
          type="button"
          className="toast"
          title="Dismiss"
          onClick={() => dismissToast(toast.id)}
        >
          <span className="toast__message" role="alert">
            {toast.message}
          </span>
        </button>
      ))}
    </div>
  );
}
