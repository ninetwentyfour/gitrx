import {
  debug as pluginDebug,
  info as pluginInfo,
  warn as pluginWarn,
} from "@tauri-apps/plugin-log";

/**
 * Thin wrappers around `@tauri-apps/plugin-log` so frontend diagnostics land in
 * the SAME rotated log file as the Rust backend
 * (`~/Library/Logs/com.travisberry.rust-gitx/gitrx.log`). This lets a single
 * timeline reconstruct what the app was doing as memory grew — watcher events,
 * refresh loops, oversized payloads — correlated against the Rust RSS watchdog.
 *
 * Every call is fire-and-forget: the underlying plugin invoke rejects when no
 * Tauri backend is present (notably jsdom under vitest, where the module is
 * mocked anyway), so the rejection is swallowed rather than surfaced. `console.*`
 * stays as-is for dev; these are the durable, file-backed complement.
 */

/** Log an informational, lifecycle-level message to the shared log file. */
export function logInfo(message: string): void {
  void pluginInfo(message).catch(() => {});
}

/** Log an anomaly (deferred refresh, oversized payload, guard hit) at warn. */
export function logWarn(message: string): void {
  void pluginWarn(message).catch(() => {});
}

/** Log a hot-path diagnostic (event received, refresh trigger) at debug. */
export function logDebug(message: string): void {
  void pluginDebug(message).catch(() => {});
}
