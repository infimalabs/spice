const fs = require("fs");
const path = require("path");
const vm = require("vm");

const lanesPath = process.argv[2];
const renderPath = path.join(path.dirname(lanesPath), "app.render.js");

class FakeStyle {
  constructor() {
    this.values = {};
  }

  setProperty(name, value) {
    this.values[name] = String(value);
  }

  removeProperty(name) {
    delete this.values[name];
  }
}

class FakeText {
  constructor(value) {
    this.nodeType = 3;
    this.textContent = String(value);
    this.parentElement = null;
  }

  find() {
    return null;
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

  get textContent() {
    return this.children.map((child) => child.textContent).join("");
  }

  set textContent(value) {
    this.children = [new FakeText(value)];
    for (const child of this.children) child.parentElement = this;
  }

  append(...nodes) {
    for (const node of nodes) {
      node.parentElement = this;
      this.children.push(node);
    }
  }

  replaceChildren(...nodes) {
    this.children = [];
    this.append(...nodes);
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
    if (selector === "[data-target-choice-driver-icon]") {
      return this.find((node) =>
        Object.prototype.hasOwnProperty.call(
          node.dataset || {},
          "targetChoiceDriverIcon",
        ),
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
  laneStore: {
    hasLane() {
      return false;
    },
    laneForId() {
      return null;
    },
    laneChrome() {
      return null;
    },
    subscribe() {},
  },
  document: {
    createElement(tagName) {
      return new FakeElement(tagName);
    },
    createTextNode(value) {
      return new FakeText(value);
    },
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(lanesPath, "utf8"), context, {
  filename: "app.lanes.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function target(overrides = {}) {
  const driver = { name: "codex", model: "gpt-5", effort: "high", ...(overrides.driver || {}) };
  return {
    id: overrides.id || "target-codex",
    statusLine: { lastAssistantAt: "2020-01-01T00:00:00Z" },
    targetIdentity: {
      branch: "feature",
      agent: { state: "configured", name: "codex-hand" },
      driver,
      thread: { state: "bound", threadId: "thread-a" },
    },
  };
}

const withDriver = target();
const metadata = context.targetChoiceMetadata(withDriver);
assert(
  metadata.includes(" · "),
  "the metadata keeps the middle-dot separator between fields",
);

const meta = context.document.createElement("span");
context.renderTargetChoiceMetadata(meta, withDriver);

const icon = meta.querySelector("[data-target-choice-driver-icon]");
assert(icon, "driver icon renders inside the meta line");
assert(meta.children.length === 2, "the meta is the icon then a single text run");
assert(meta.children[0] === icon, "the driver icon leads the meta line");
assert(
  meta.children[1].textContent === " " + metadata,
  "the timer and status follow the icon with the middle dot intact",
);
assert(
  icon.dataset.targetChoiceDriverIcon === "codex",
  "icon marks its driver in the dataset",
);
assert(
  icon.className === "target-choice-driver-icon target-choice-driver-icon--codex",
  "icon carries the per-driver class",
);
assert(icon.title.includes("Codex driver"), "tooltip names the driver");
assert(icon.title.includes("model: gpt-5"), "tooltip carries the model");
assert(icon.title.includes("effort: high"), "tooltip carries the effort");
assert(icon.title.includes("thread: thread-a"), "tooltip carries the thread");
assert(
  icon.getAttribute("aria-label") === icon.title,
  "aria-label mirrors the tooltip",
);
assert(
  icon.style.values["--target-choice-driver-icon-url"] ===
    'url("/static/icons/openai.svg")',
  "icon points at the OpenAI emblem asset",
);

const withoutDriver = target({ driver: { name: "" } });
const plainMeta = context.document.createElement("span");
context.renderTargetChoiceMetadata(plainMeta, withoutDriver);
assert(
  plainMeta.querySelector("[data-target-choice-driver-icon]") === null,
  "a driverless row keeps the plain text metadata",
);
assert(
  plainMeta.children.length === 1,
  "the driverless meta is a single text run",
);
assert(
  plainMeta.textContent === metadata,
  "the driverless meta joins fields with the middle dot",
);
assert(
  meta.children.length !== plainMeta.children.length,
  "the driver row prepends the icon node the driverless row omits",
);
assert(
  meta.textContent.trimStart() === plainMeta.textContent,
  "the leading icon preserves the same dot-joined fields as the driverless row",
);
