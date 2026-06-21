const fs = require("fs");
const vm = require("vm");

const composerPath = process.argv[2];

class FakeStyle {
  constructor() {
    this.values = {};
  }

  setProperty(name, value) {
    this.values[name] = String(value);
  }
}

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName.toLowerCase();
    this.children = [];
    this.parentElement = null;
    this.dataset = {};
    this.attributes = {};
    this.className = "";
    this.title = "";
    this.style = new FakeStyle();
  }

  append(...nodes) {
    for (const node of nodes) this.attach(node, this.children.length);
  }

  attach(node, index) {
    if (node.parentElement) node.remove();
    this.children.splice(index, 0, node);
    node.parentElement = this;
  }

  replaceWith(node) {
    if (!this.parentElement) throw new Error("replaceWith requires a parent");
    const parent = this.parentElement;
    const index = parent.children.indexOf(this);
    if (index < 0) throw new Error("replaceWith could not find current node");
    if (node.parentElement) node.remove();
    parent.children[index] = node;
    node.parentElement = parent;
    this.parentElement = null;
  }

  remove() {
    if (!this.parentElement) return;
    const parent = this.parentElement;
    const index = parent.children.indexOf(this);
    if (index >= 0) parent.children.splice(index, 1);
    this.parentElement = null;
  }

  setAttribute(name, value) {
    this.attributes[name] = String(value);
  }

  getAttribute(name) {
    return Object.prototype.hasOwnProperty.call(this.attributes, name)
      ? this.attributes[name]
      : null;
  }

  querySelector(selector) {
    if (selector === "[data-composer-driver-icon]") {
      return this.find((node) =>
        Object.prototype.hasOwnProperty.call(node.dataset || {}, "composerDriverIcon"),
      );
    }
    throw new Error("unsupported selector: " + selector);
  }

  find(predicate) {
    for (const child of this.children) {
      if (predicate(child)) return child;
      const found = child.find ? child.find(predicate) : null;
      if (found) return found;
    }
    return null;
  }
}

const context = {
  console,
  document: {
    createElement(tagName) {
      return new FakeElement(tagName);
    },
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(composerPath, "utf8"), context, {
  filename: "app.composer.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function member(overrides = {}) {
  return {
    driverIconName: "codex",
    driverName: "codex",
    driverModel: "gpt-5",
    driverEffort: "high",
    targetThreadId: "thread-a",
    ...overrides,
  };
}

const band = context.document.createElement("div");

context.syncComposerDriverIcon(band, member());
const firstIcon = band.querySelector("[data-composer-driver-icon]");
assert(firstIcon, "initial composer driver icon rendered");
firstIcon.hoverProbe = { active: true };

context.syncComposerDriverIcon(
  band,
  member({ driverModel: "gpt-5.1", targetThreadId: "thread-b" }),
);
const secondIcon = band.querySelector("[data-composer-driver-icon]");
assert(
  secondIcon === firstIcon,
  "unchanged composer driver rerender keeps the same icon element",
);
assert(secondIcon.hoverProbe.active, "hover state probe stays attached to icon");
assert(
  secondIcon.title.includes("model: gpt-5.1"),
  "rerender refreshes the icon tooltip on the stable element",
);
assert(
  secondIcon.title.includes("thread: thread-b"),
  "rerender refreshes the icon thread detail on the stable element",
);
assert(
  secondIcon.getAttribute("aria-label") === secondIcon.title,
  "rerender keeps icon aria-label aligned with tooltip",
);
assert(
  secondIcon.style.values["--composer-driver-icon-url"] ===
    'url("/static/icons/openai.svg")',
  "rerender keeps the OpenAI emblem asset on the stable element",
);
