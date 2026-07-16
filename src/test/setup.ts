import "@testing-library/jest-dom/vitest";
import { vi } from "vitest";

// `@tauri-apps/plugin-log` invokes the Tauri backend, which rejects in jsdom.
// The diagnostic-log wrapper (src/lib/log.ts) is fire-and-forget, but mocking the
// module keeps test output clean and mirrors the other Tauri plugin stubs below.
vi.mock("@tauri-apps/plugin-log", () => ({
  info: vi.fn().mockResolvedValue(undefined),
  warn: vi.fn().mockResolvedValue(undefined),
  debug: vi.fn().mockResolvedValue(undefined),
}));

// The Tauri window API has no backing window in jsdom. The store's
// theme-application path calls `getCurrentWindow().setTheme(...)` and its
// status-load path calls `getCurrentWindow().setTitle(...)`, so stub both
// globally to resolved no-ops. Individual suites may override this mock to
// assert on the calls (see useAppStore.test.ts).
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setTheme: vi.fn().mockResolvedValue(undefined),
    setTitle: vi.fn().mockResolvedValue(undefined),
  }),
}));

// react-resizable-panels relies on ResizeObserver and matchMedia, neither of
// which jsdom provides. Provide minimal stubs so components mount in tests.
if (!("ResizeObserver" in globalThis)) {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

if (!globalThis.matchMedia) {
  globalThis.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof globalThis.matchMedia;
}

// jsdom does not implement `scrollIntoView`; the window-level keyboard-nav hook
// (useFileListKeyboardNav) calls it to keep the newly focused row visible after an
// arrow-key navigation, so provide a no-op so that call does not throw in tests.
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

// jsdom does not implement layout, so element sizes are 0. Stub the boxes the
// panel library reads during resize calculations.
if (!Element.prototype.getBoundingClientRect.name.includes("stub")) {
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(
    () =>
      ({
        width: 800,
        height: 600,
        top: 0,
        left: 0,
        right: 800,
        bottom: 600,
        x: 0,
        y: 0,
        toJSON: () => {},
      }) as DOMRect,
  );
}
