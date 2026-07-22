"use strict";

const fs = require("fs");
const path = require("path");

const sourcePath = process.argv[2];
const outputPath = process.argv[3];
const modulesPath = process.argv[4];
if (!sourcePath || !outputPath || !modulesPath) {
  throw new Error("usage: handout.js PAYLOAD OUTPUT_DIRECTORY NODE_MODULES");
}

const { chromium } = require(path.join(modulesPath, "playwright"));
const mermaidBundle = path.join(modulesPath, "mermaid", "dist", "mermaid.min.js");
const payload = JSON.parse(fs.readFileSync(sourcePath, "utf8"));
const diagramPath = path.join(outputPath, "diagrams");
const BAND_RATIO = 1.45;
const BAND_HEIGHT_IN = 5.25;
const PAGE_CHART_WIDTH_IN = 9.75;
const SIDE_HEIGHT_IN = 6.15;
const SIDE_WIDTH_IN = 5.55;
const RASTER_WIDTH = 1600;
const RASTER_HEIGHT = 1400;
const RASTER_MARGIN = 48;
const RASTER_MIN_WIDTH = 400;
const RASTER_MIN_HEIGHT = 300;
const BYTES_PER_KIBIBYTE = 1024;

const FAMILY = {
  magnitude: ["Magnitude", "#eb6834"],
  flow: ["Flow", "#2a78d6"],
  topology: ["Topology", "#1baf7a"],
  time: ["Time", "#e87ba4"],
};

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function pageFooter(label, pageNumber) {
  return `<footer><span>${escapeHtml(label)}</span><span>${pageNumber}</span></footer>`;
}

function coverPage(pageNumber) {
  const facts = payload.facts;
  const stats = [
    [facts.tasks, "live tasks"],
    [facts.completed, "completed"],
    [facts.archived, "archived filings"],
    [facts.diagrams, "diagrams"],
  ]
    .map(
      ([value, label]) =>
        `<div class="stat"><b>${value}</b><span>${escapeHtml(label)}</span></div>`,
    )
    .join("");
  const ceiling = payload.ceiling
    ? `Snapshot ceiling ${escapeHtml(payload.ceiling)}`
    : "Current live snapshot";
  return `<section class="page cover">
    <div class="mast"><span class="brand">spice</span><i></i><span>Board field guide</span></div>
    <div class="cover-copy">
      <h1>Thirty-seven ways<br>to look at work.</h1>
      <p>${facts.lanes} lanes, ${facts.days} days, one repository. Every diagram is
      rebuilt from one TaskChampion export, and every caption is composed from the
      same snapshot as the picture beside it.</p>
    </div>
    <div class="cover-bottom"><div class="stats">${stats}</div>
      <div class="snapshot"><code>${escapeHtml(payload.command)}</code><span>${ceiling}</span></div>
    </div>
    ${pageFooter("spice board handout", pageNumber)}
  </section>`;
}

function indexPage(pageNumber) {
  const entries = payload.diagrams
    .map((diagram) => {
      const [family, hue] = FAMILY[diagram.family];
      const number = diagram.rank ? String(diagram.rank).padStart(2, "0") : "--";
      return `<div class="index-item" style="--hue:${hue}"><b>${number}</b><div>
        <span>${escapeHtml(diagram.title)}</span>
        <small>${family} / ${escapeHtml(diagram.name)}</small>
      </div></div>`;
    })
    .join("");
  return `<section class="page index-page">
    <header><div class="eyebrow">Complete registry</div><h2>One snapshot, four grammars.</h2>
    <p>The first fifteen cuts retain the handout ranking. Every remaining cut follows in registry order.</p></header>
    <div class="index-grid">${entries}</div>
    ${pageFooter("Contents", pageNumber)}
  </section>`;
}

function chartPage(diagram, pageNumber) {
  const [family, hue] = FAMILY[diagram.family];
  const number = diagram.rank
    ? `No. ${String(diagram.rank).padStart(2, "0")}`
    : "Registry cut";
  const layout = diagram.ratio >= BAND_RATIO ? "band" : "side";
  let visualStyle = "";
  if (layout === "band") {
    visualStyle = `height:${Math.min(BAND_HEIGHT_IN, PAGE_CHART_WIDTH_IN / diagram.ratio).toFixed(2)}in`;
  } else {
    const height = Math.min(SIDE_HEIGHT_IN, SIDE_WIDTH_IN / diagram.ratio);
    visualStyle = `width:${Math.min(SIDE_WIDTH_IN, height * diagram.ratio).toFixed(2)}in;height:${height.toFixed(2)}in`;
  }
  const archive = diagram.includeArchived
    ? "This filing census includes archived rows."
    : "Archived rows are excluded from this live-board cut.";
  return `<section class="page chart-page ${layout}">
    <header><div class="eyebrow"><i style="background:${hue}"></i>${number} / ${family}</div>
      <h2>${escapeHtml(diagram.title)}</h2></header>
    <div class="chart-content">
      <figure class="visual" style="${visualStyle}">${diagram.svg}</figure>
      <div class="caption"><p class="lead">${escapeHtml(diagram.note)}</p>
        <p>${escapeHtml(diagram.caption)} ${archive}</p>
        <code>spice task graph ${escapeHtml(diagram.name)}${payload.ceiling ? ` --ceiling ${escapeHtml(payload.ceiling)}` : ""}</code>
      </div>
    </div>
    ${pageFooter(diagram.name, pageNumber)}
  </section>`;
}

const CSS = `
@page { size: 11in 8.5in; margin: 0; }
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; background: #e9e7e1; color: #0b0b0b; }
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; print-color-adjust: exact; }
.page { width: 11in; height: 8.5in; padding: .55in .62in .42in; background: #f2f1ed; break-after: page; overflow: hidden; display: flex; flex-direction: column; }
.page:last-child { break-after: auto; }
h1, h2 { font-family: Georgia, "Times New Roman", serif; font-weight: 400; margin: 0; letter-spacing: -.025em; }
h1 { font-size: 54pt; line-height: 1.01; }
h2 { font-size: 25pt; line-height: 1.08; }
p { color: #52514e; line-height: 1.5; }
code { font-family: ui-monospace, Menlo, monospace; font-size: 7.5pt; color: #52514e; }
footer { margin-top: auto; border-top: 1px solid rgba(11,11,11,.1); padding-top: 7px; display: flex; justify-content: space-between; font-size: 7pt; text-transform: uppercase; letter-spacing: .1em; color: #898781; }
.cover { padding: .75in .82in .42in; justify-content: space-between; }
.mast { display: flex; align-items: center; gap: 12px; color: #898781; font-size: 8pt; letter-spacing: .18em; text-transform: uppercase; }
.mast .brand { font-family: Georgia, serif; color: #0b0b0b; font-size: 15pt; letter-spacing: 0; text-transform: none; }
.mast i { width: 32px; height: 1px; background: rgba(11,11,11,.18); }
.cover-copy { margin-top: .15in; }
.cover-copy p { width: 5.6in; font-size: 11pt; margin-top: .25in; }
.cover-bottom { display: flex; align-items: end; justify-content: space-between; gap: .4in; }
.stats { display: flex; gap: .48in; border-top: 1px solid rgba(11,11,11,.14); padding-top: .16in; }
.stat { display: flex; flex-direction: column; gap: 4px; }
.stat b { font: 25pt Georgia, serif; }
.stat span { font-size: 6.8pt; text-transform: uppercase; color: #898781; letter-spacing: .1em; white-space: nowrap; }
.snapshot { text-align: right; display: flex; flex-direction: column; gap: 8px; color: #898781; font-size: 7.5pt; }
.snapshot code { border: 1px solid rgba(11,11,11,.12); padding: 6px 9px; border-radius: 4px; color: #0b0b0b; }
.index-page header p { margin: 7px 0 0; font-size: 9pt; }
.eyebrow { display: flex; align-items: center; gap: 7px; color: #898781; font-size: 7.3pt; text-transform: uppercase; letter-spacing: .12em; margin-bottom: 8px; }
.eyebrow i { width: 7px; height: 7px; border-radius: 50%; }
.index-grid { margin-top: .22in; display: grid; grid-template-columns: repeat(3, 1fr); grid-auto-flow: column; grid-template-rows: repeat(13, 1fr); column-gap: .28in; flex: 1; min-height: 0; }
.index-item { display: grid; grid-template-columns: 25px 1fr; gap: 7px; align-items: start; border-left: 3px solid var(--hue); padding-left: 7px; min-height: 0; }
.index-item b { font: 11pt Georgia, serif; color: #52514e; }
.index-item span { display: block; font: 8.2pt Georgia, serif; line-height: 1.12; }
.index-item small { display: block; margin-top: 2px; color: #898781; font-size: 5.8pt; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.chart-page header { flex: none; }
.chart-content { flex: 1; min-height: 0; margin-top: .18in; }
.visual { margin: 0; background: #fcfcfb; border-radius: 5px; box-shadow: 0 8px 24px -12px rgba(11,11,11,.25), inset 0 0 0 1px rgba(11,11,11,.06); overflow: hidden; display: flex; align-items: center; justify-content: center; }
.visual svg { display: block; width: 100%; height: 100%; max-width: 100%; max-height: 100%; }
.caption .lead { color: #0b0b0b; font: 12pt Georgia, serif; line-height: 1.35; margin: 0; }
.caption p:not(.lead) { font-size: 8.5pt; margin: 10px 0; }
.caption code { display: block; overflow-wrap: anywhere; }
.band .chart-content { display: flex; flex-direction: column; gap: .18in; }
.band .visual { width: 100%; }
.band .caption { display: grid; grid-template-columns: 1.25fr 1fr 1fr; gap: .25in; align-items: start; }
.band .caption p { margin: 0; }
.side .chart-content { display: grid; grid-template-columns: 5.7in 1fr; gap: .35in; align-items: start; }
.side .visual { justify-self: center; }
.side .caption { border-top: 1px solid rgba(11,11,11,.12); padding-top: .18in; }
`;

async function renderDiagram(renderPage, shotPage, diagram, index) {
  const result = await renderPage.evaluate(
    async ({ id, source }) => {
      const rendered = await window.mermaid.render(`handout_${id}`, source);
      const match = rendered.svg.match(/viewBox="([^"]+)"/);
      const values = match ? match[1].split(/\s+/).map(Number) : [];
      return { svg: rendered.svg, width: values[2] || 0, height: values[3] || 0 };
    },
    { id: index, source: diagram.source },
  );
  const ratio = result.width / result.height;
  if (!result.width || !result.height || ratio < payload.aspect.minimum || ratio > payload.aspect.maximum) {
    throw new Error(`${diagram.name} rendered at ${ratio.toFixed(2)}:1 outside ${payload.aspect.minimum}:1..${payload.aspect.maximum}:1`);
  }
  fs.writeFileSync(path.join(diagramPath, `${diagram.name}.svg`), result.svg);
  const width = Math.round(Math.min(RASTER_WIDTH, RASTER_HEIGHT * ratio));
  const height = Math.round(width / ratio);
  await shotPage.setViewportSize({
    width: Math.max(RASTER_MIN_WIDTH, width + RASTER_MARGIN),
    height: Math.max(RASTER_MIN_HEIGHT, height + RASTER_MARGIN),
  });
  await shotPage.setContent(`<style>html,body{margin:0;background:#fcfcfb}.shot{width:${width}px;height:${height}px;padding:16px}.shot svg{width:100%;height:100%;display:block}</style><div class="shot">${result.svg}</div>`);
  await shotPage.locator(".shot").screenshot({ path: path.join(diagramPath, `${diagram.name}.png`) });
  return { ...diagram, ...result, ratio };
}

async function main() {
  fs.mkdirSync(outputPath, { recursive: true });
  fs.mkdirSync(diagramPath, { recursive: true });
  const browser = await chromium.launch();
  try {
    const renderPage = await browser.newPage();
    const shotPage = await browser.newPage({ deviceScaleFactor: 2 });
    await renderPage.setContent("<!doctype html><html><body></body></html>");
    await renderPage.addScriptTag({ path: mermaidBundle });
    await renderPage.evaluate(() => window.mermaid.initialize({ startOnLoad: false }));
    const rendered = [];
    for (const [index, diagram] of payload.diagrams.entries()) {
      rendered.push(await renderDiagram(renderPage, shotPage, diagram, index));
    }
    const pages = [coverPage(1), indexPage(2)];
    rendered.forEach((diagram, index) => pages.push(chartPage(diagram, index + 3)));
    const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>spice board handout</title><style>${CSS}</style></head><body>${pages.join("")}</body></html>`;
    fs.writeFileSync(path.join(outputPath, "handout.html"), html);
    const pdfPage = await browser.newPage();
    await pdfPage.setContent(html, { waitUntil: "load" });
    await pdfPage.emulateMedia({ media: "print" });
    await pdfPage.pdf({
      path: path.join(outputPath, "handout.pdf"),
      width: "11in",
      height: "8.5in",
      printBackground: true,
      preferCSSPageSize: true,
    });
    const manifest = { ...payload, diagrams: rendered.map(({ source, svg, ...entry }) => entry) };
    fs.writeFileSync(path.join(outputPath, "manifest.json"), JSON.stringify(manifest, null, 2));
    const pdfBytes = fs.statSync(path.join(outputPath, "handout.pdf")).size;
    process.stdout.write(`handout=${path.join(outputPath, "handout.pdf")} pages=${pages.length} diagrams=${rendered.length} pdf_kb=${Math.round(pdfBytes / BYTES_PER_KIBIBYTE)}`);
  } finally {
    await browser.close();
  }
}

main().catch((error) => {
  process.stderr.write(String(error && error.stack ? error.stack : error));
  process.exit(1);
});
