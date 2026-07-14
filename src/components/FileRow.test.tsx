/**
 * FileRow Tests
 *
 * A single file row's presentation: no inline action buttons, the selected
 * styling contract, RTL-safe path truncation (leading dot preserved), and
 * renamed-path rendering. Click/activate/context-menu forwarding is covered
 * end-to-end by the FileList suite.
 *
 * Key behaviors:
 * - Renders no Stage/Unstage/Discard buttons (actions moved to menu/dblclick)
 * - `selected` sets the is-selected class and aria-current
 * - Path text keeps logical order (leading `.` not reordered); renames show old → new
 */
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { FileRow } from "./FileRow";
import { makeFileEntry as makeEntry } from "../test/factories";
import type { FileEntry } from "../types/ipc";

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
