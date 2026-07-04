const fs = require("fs");
const vm = require("vm");

const source = fs.readFileSync(process.argv[2], "utf8");
const context = { console };
vm.createContext(context);
vm.runInContext(source, context, { filename: "app.mosaic-scroll.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const DEFAULT_THRESHOLD = 80;

// "At top" is scrollTop <= threshold; past it counts as scrolled (demo
// parity: window.scrollY > 80).
assert(
  context.mosaicPaneIsScrolled(0, DEFAULT_THRESHOLD) === false,
  "scrollTop 0 must read as at-top",
);
assert(
  context.mosaicPaneIsScrolled(DEFAULT_THRESHOLD, DEFAULT_THRESHOLD) === false,
  "scrollTop exactly at the threshold must still read as at-top",
);
assert(
  context.mosaicPaneIsScrolled(DEFAULT_THRESHOLD + 1, DEFAULT_THRESHOLD) === true,
  "scrollTop just past the threshold must read as scrolled",
);
assert(
  context.mosaicPaneIsScrolled(500, DEFAULT_THRESHOLD) === true,
  "scrollTop well past the threshold must read as scrolled",
);

// A missing/invalid threshold falls back to the default rather than
// treating every scroll position as scrolled or at-top.
assert(
  context.mosaicPaneIsScrolled(81, undefined) === true,
  "an omitted threshold must fall back to the 80px default",
);
assert(
  context.mosaicPaneIsScrolled(79, undefined) === false,
  "an omitted threshold must fall back to the 80px default",
);

console.log("mosaic scroll: all assertions passed");
