// Renders the spice menu version footer with a mocked DOM and asserts it
// reflects the injected runtime version (spiceServeBranding.version).
const fs = require("fs");
const vm = require("vm");

const menuPath = process.argv[2];

function makeElement() {
  return {
    className: "",
    textContent: "",
    append() {},
    replaceChildren() {},
  };
}

const context = {
  console,
  document: { createElement: makeElement },
  spiceServeBranding: { name: "spice", version: "9.9.9" },
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(menuPath, "utf8"), context, {
  filename: "app.menu.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const withVersion = context.renderSpiceMenuVersion();
assert(
  withVersion.className === "spice-menu-version",
  "version footer carries the spice-menu-version class",
);
assert(
  withVersion.textContent === "v9.9.9",
  "version footer renders v<version>, got: " + withVersion.textContent,
);

context.spiceServeBranding = { name: "spice" };
const withoutVersion = context.renderSpiceMenuVersion();
assert(
  withoutVersion.textContent === "",
  "missing version renders an empty footer, got: " + withoutVersion.textContent,
);

console.log("ok");
