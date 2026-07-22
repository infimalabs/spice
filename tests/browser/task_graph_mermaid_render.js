"use strict";

// Render every diagram `spice task graph` emits through real mermaid in a real
// browser, and report the SVG each one produced.
//
// Parsing is not rendering: a diagram can parse and still fail in layout (an
// illegal node id, a quoted subgraph title, a label mermaid reads as markup).
// Only a full `mermaid.render` proves the emitted source renders unmodified,
// so this drives the repo-local mermaid bundle inside repo-local Playwright
// rather than asserting on the text.
//
// Usage: node task_graph_render_smoke.js <diagrams.json>
//   <diagrams.json> maps a view name to its mermaid source.
//   Prints one JSON object: { view: { ok, svgLength, error } }.

const fs = require("fs");
const path = require("path");
const { chromium } = require("playwright");

const repoRoot = path.resolve(__dirname, "..", "..");
const mermaidBundle = path.join(
  repoRoot,
  "node_modules",
  "mermaid",
  "dist",
  "mermaid.min.js",
);

async function main() {
  const source = process.argv[2];
  if (!source) {
    throw new Error("usage: node task_graph_render_smoke.js <diagrams.json>");
  }
  if (!fs.existsSync(mermaidBundle)) {
    throw new Error(`missing repo-local mermaid bundle at ${mermaidBundle}`);
  }
  const diagrams = JSON.parse(fs.readFileSync(source, "utf8"));

  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // A real document: mermaid measures text against live layout, so an empty
    // context would let a diagram "render" without ever being laid out.
    await page.setContent("<!doctype html><html><body></body></html>");
    await page.addScriptTag({ path: mermaidBundle });
    await page.evaluate(() => {
      window.mermaid.initialize({ startOnLoad: false });
    });

    const results = {};
    for (const [view, text] of Object.entries(diagrams)) {
      results[view] = await page.evaluate(async ([id, definition]) => {
        try {
          const { svg } = await window.mermaid.render(`g_${id}`, definition);
          const viewBox = svg.match(/viewBox="([^"]+)"/);
          const dimensions = viewBox
            ? viewBox[1].split(/\s+/).map((value) => Number(value))
            : [];
          return {
            ok: svg.includes("<svg") && Boolean(viewBox),
            svgLength: svg.length,
            width: dimensions[2] || 0,
            height: dimensions[3] || 0,
            error: viewBox ? "" : "rendered SVG has no measurable viewBox",
          };
        } catch (err) {
          return {
            ok: false,
            svgLength: 0,
            width: 0,
            height: 0,
            error: String(err && err.message),
          };
        }
      }, [view, text]);
    }
    process.stdout.write(JSON.stringify(results));
  } finally {
    await browser.close();
  }
}

main().catch((err) => {
  process.stderr.write(String((err && err.stack) || err));
  process.exit(1);
});
