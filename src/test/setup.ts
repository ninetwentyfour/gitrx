import "@testing-library/jest-dom/vitest";
import { vi } from "vitest";

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
