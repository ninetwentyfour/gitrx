import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { FileRow } from "./FileRow";
import type { FileEntry } from "../types/ipc";

function makeEntry(overrides: Partial<FileEntry> = {}): FileEntry {
  return {
    path: "a.txt",
    status: "modified",
    staged: false,
    isBinary: false,
    additions: 1,
    deletions: 0,
    ...overrides,
  };
}

function renderRow(entry: FileEntry, handlers: Partial<Parameters<typeof FileRow>[0]> = {}) {
  return render(
    <FileRow
      entry={entry}
      selected={false}
      onSelect={() => {}}
      onActivate={() => {}}
      onContextMenu={() => {}}
      {...handlers}
    />,
  );
}

afterEach(() => {
  vi.clearAllMocks();
});

describe("FileRow", () => {
  it("no longer renders inline Stage/Unstage/Discard action buttons", () => {
    renderRow(makeEntry());
    expect(screen.queryByRole("button", { name: "Stage" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Discard" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Unstage" })).not.toBeInTheDocument();
  });

  it("marks the row button as selected", () => {
    renderRow(makeEntry(), {}); // not selected
    expect(screen.getByRole("button")).not.toHaveClass("is-selected");

    render(
      <FileRow
        entry={makeEntry({ path: "b.txt" })}
        selected
        onSelect={() => {}}
        onActivate={() => {}}
        onContextMenu={() => {}}
      />,
    );
    const selected = screen.getByRole("button", { name: /b\.txt/ });
    expect(selected).toHaveClass("is-selected");
    expect(selected).toHaveAttribute("aria-current", "true");
  });

  it("forwards click / double-click / context-menu to the handlers", () => {
    const onSelect = vi.fn();
    const onActivate = vi.fn();
    const onContextMenu = vi.fn();
    renderRow(makeEntry({ path: "x.txt" }), { onSelect, onActivate, onContextMenu });

    const btn = screen.getByRole("button", { name: /x\.txt/ });
    fireEvent.click(btn);
    fireEvent.doubleClick(btn);
    fireEvent.contextMenu(btn);

    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onActivate).toHaveBeenCalledTimes(1);
    expect(onContextMenu).toHaveBeenCalledTimes(1);
  });

  // The RTL/plaintext truncation must not reorder a leading dot to the end.
  it.each([".gitignore", ".oxlintrc.json", ".claude/scheduled_tasks.lock", "src/a.txt"])(
    "keeps the leading characters of %s in logical order",
    (path) => {
      const { container } = renderRow(makeEntry({ path }));
      const text = container.querySelector(".file-row__path-text");
      expect(text?.textContent).toBe(path);
    },
  );

  it("renders a renamed path as old → new", () => {
    const { container } = renderRow(
      makeEntry({ status: "renamed", oldPath: "old/name.txt", path: "new/name.txt" }),
    );
    const text = container.querySelector(".file-row__path-text");
    expect(text?.textContent).toBe("old/name.txt → new/name.txt");
  });
});
