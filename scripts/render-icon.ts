// Renders the gitrx neon commit-graph source SVG to a 1024x1024 PNG.
// Rerun after editing src-tauri/icons/source/gitrx-icon.svg:
//   bun scripts/render-icon.ts
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { Resvg } from "@resvg/resvg-js";

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = resolve(here, "../src-tauri/icons/source");
const svgPath = resolve(srcDir, "gitrx-icon.svg");
const outPath = resolve(srcDir, "gitrx-icon-1024.png");

const svg = readFileSync(svgPath, "utf8");
const resvg = new Resvg(svg, {
  fitTo: { mode: "width", value: 1024 },
  background: "rgba(0,0,0,0)",
});
const png = resvg.render().asPng();
writeFileSync(outPath, png);
console.log(`Rendered ${outPath} (${png.length} bytes)`);
