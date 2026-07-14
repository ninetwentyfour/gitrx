/**
 * DiffLineRow Tests
 *
 * A single rendered diff line. Verifies the display-time CRLF strip (the trailing
 * `\r` is dropped for rendering only) and the two render modes: plain text versus
 * pre-highlighted coloured token spans with inline colour/weight.
 *
 * Key behaviors:
 * - Plain mode strips the trailing `\r` and emits no span children
 * - Token mode renders one span per token with inline colour and fontWeight
 */
import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { DiffLineRow } from "./DiffLineRow";
import { makeDiffLine } from "../test/factories";

const addLine = makeDiffLine({
  kind: "add",
  oldLineNo: null,
  newLineNo: 2,
  content: "const n = 42;\r",
});

describe("DiffLineRow", () => {
  it("renders plain content with a trailing CRLF stripped when no tokens are given", () => {
    const { container } = render(<DiffLineRow line={addLine} />);
    const code = container.querySelector(".diff-line__code");
    // The `\r` is dropped for display; no coloured spans in plain mode.
    expect(code?.textContent).toBe("const n = 42;");
    expect(code?.querySelectorAll("span").length).toBe(0);
  });

  it("renders coloured token spans with inline colour and style when tokens are provided", () => {
    const { container } = render(
      <DiffLineRow
        line={addLine}
        tokens={[
          { content: "const", color: "#C792EA", fontStyle: "bold" },
          { content: " n = 42;", color: "#a9b1d6" },
        ]}
      />,
    );
    const spans = container.querySelectorAll(".diff-line__code span");
    expect(spans.length).toBe(2);
    expect((spans[0] as HTMLElement).style.color).toBe("rgb(199, 146, 234)");
    expect((spans[0] as HTMLElement).style.fontWeight).toBe("bold");
    expect((spans[0] as HTMLElement).textContent).toBe("const");
  });
});
