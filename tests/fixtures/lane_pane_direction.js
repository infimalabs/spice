const fs = require("fs");
const vm = require("vm");

const shellPath = process.argv[2];
const laneViewModes = ["compose", "filters", "metrics", "info"];
const context = {
  defaultLaneViewMode: "compose",
  laneViewModes,
  laneViewMode(value) {
    return laneViewModes.includes(value || "") ? value : "compose";
  },
  laneViewModeIndex(view) {
    return laneViewModes.indexOf(context.laneViewMode(view));
  },
  renderLaneFiltersPane() {},
  renderLaneMetricsPane() {},
  renderLaneInfoPane() {},
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(shellPath, "utf8"), context, {
  filename: "app.shell.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function styleCapture() {
  return {
    values: {},
    setProperty(name, value) {
      this.values[name] = value;
    },
    getPropertyValue(name) {
      return this.values[name] || "";
    },
  };
}

function classListCapture() {
  return {
    toggle() {},
  };
}

function panel(view) {
  return {
    dataset: { laneViewPanel: view },
    style: styleCapture(),
    classList: classListCapture(),
    attrs: new Set(),
    setAttribute(name) {
      this.attrs.add(name);
    },
    removeAttribute(name) {
      this.attrs.delete(name);
    },
  };
}

function lane(selectedView) {
  const panels = Object.fromEntries(
    laneViewModes.map((view) => [view, panel(view)]),
  );
  return {
    selectedView,
    emptyTeam: false,
    viewStackEl: { style: styleCapture() },
    modeRailEl: {
      classList: classListCapture(),
      setAttribute() {},
    },
    element: {
      querySelectorAll(selector) {
        if (selector === "[data-lane-view-button]") return [];
        if (selector === "[data-lane-view-panel]")
          return laneViewModes.map((view) => panels[view]);
        throw new Error("unexpected selector: " + selector);
      },
    },
    panels,
  };
}

function offsetsFor(selectedView) {
  const host = lane(selectedView);
  context.renderLaneViewShell(host);
  return Object.fromEntries(
    laneViewModes.map((view) => [
      view,
      host.panels[view].style.values["--lane-view-x"],
    ]),
  );
}

const compose = offsetsFor("compose");
const filters = offsetsFor("filters");
const metrics = offsetsFor("metrics");

assert(compose.compose === "0%", "compose starts centered");
assert(compose.filters === "-100%", "rightward target starts to the left");
assert(filters.compose === "100%", "rightward move pushes previous pane right");
assert(filters.filters === "0%", "filters centers after rightward move");
assert(filters.metrics === "-100%", "next rightward target stays left");
assert(metrics.filters === "100%", "continuing right pushes filters right");
