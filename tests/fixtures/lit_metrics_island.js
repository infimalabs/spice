const fs = require("fs");
const vm = require("vm");

const panesPath = process.argv[2];

class FakeClassList {
  constructor(owner) {
    this.owner = owner;
  }

  add(...classes) {
    const values = this.values();
    for (const className of classes) values.add(className);
    this.write(values);
  }

  toggle(className, enabled = undefined) {
    const values = this.values();
    const next = enabled === undefined ? !values.has(className) : Boolean(enabled);
    if (next) values.add(className);
    else values.delete(className);
    this.write(values);
  }

  contains(className) {
    return this.values().has(className);
  }

  values() {
    return new Set(String(this.owner.className || "").split(/\s+/).filter(Boolean));
  }

  write(values) {
    this.owner.className = [...values].join(" ");
  }
}

class FakeStyle {
  constructor() {
    this.values = {};
  }

  setProperty(name, value) {
    this.values[name] = value;
  }
}

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName.toLowerCase();
    this.children = [];
    this.className = "";
    this.textContent = "";
    this.style = new FakeStyle();
    this.classList = new FakeClassList(this);
  }

  append(...nodes) {
    this.children.push(...nodes);
  }

  replaceChildren(...nodes) {
    this.children = [...nodes];
  }

  querySelector(selector) {
    if (selector.startsWith(".")) {
      const className = selector.slice(1);
      return this.find((node) => node.classList.contains(className));
    }
    const tagName = selector.toLowerCase();
    return this.find((node) => node.tagName === tagName);
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

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function flushAsyncWork() {
  return new Promise((resolve) => setImmediate(resolve));
}

function createContext() {
  const storage = new Map();
  let loaderCalls = 0;
  const context = {
    URLSearchParams,
    window: {
      location: { search: "" },
      localStorage: {
        getItem(key) {
          return storage.has(key) ? storage.get(key) : null;
        },
        setItem(key, value) {
          storage.set(key, String(value));
        },
      },
      __spiceLitMetricsModuleLoader() {
        loaderCalls += 1;
        return Promise.resolve({
          renderLaneMetricsLitIsland(host, model) {
            const island = new FakeElement("lit-stub");
            island.model = model;
            host.replaceChildren(island);
          },
        });
      },
    },
    document: {
      createElement(tagName) {
        return new FakeElement(tagName);
      },
    },
    browserStorage() {
      return context.window.localStorage;
    },
    loaderCallCount() {
      return loaderCalls;
    },
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(panesPath, "utf8"), context, {
    filename: "app.panes.js",
  });
  return context;
}

function lane(metrics = {}, overrides = {}) {
  return {
    metricsGridEl: new FakeElement("div"),
    metricsSummaryEl: new FakeElement("span"),
    laneMetrics: metrics,
    serverReachable: true,
    ...overrides,
  };
}

(async () => {
  const vanillaContext = createContext();
  const vanillaLane = lane({
    drained: 2,
    acked: 1,
    sends: 3,
    toolCalls: 4,
    uptimeSeconds: 125,
    sparkline: [0, 2, 4],
  });

  vanillaContext.renderLaneMetricsPane(vanillaLane);

  assert(vanillaContext.loaderCallCount() === 0, "default metrics skip Lit loader");
  assert(vanillaLane.metricsSummaryEl.textContent === "live", "summary renders");
  assert(vanillaLane.metricsGridEl.children.length === 6, "six metric cells render");
  assert(
    vanillaLane.metricsGridEl.children[0].children[0].textContent === "2",
    "drained value renders",
  );
  const sparkline = vanillaLane.metricsGridEl.children[5].querySelector(
    ".lane-metric-sparkline",
  );
  assert(sparkline.children.length === 3, "sparkline bars render");
  assert(
    sparkline.children[1].style.values["--lane-metric-sparkline-level"] === "4",
    "sparkline levels are normalized",
  );

  const litContext = createContext();
  litContext.window.location.search = "?litMetrics=1";
  const litLane = lane({
    drained: 5,
    acked: 6,
    sends: 7,
    toolCalls: 8,
    uptimeSeconds: 3600,
    sparkline: [1, 3],
  });

  litContext.renderLaneMetricsPane(litLane);

  assert(litLane.metricsGridEl.children.length === 6, "fallback renders while loading");
  await flushAsyncWork();
  assert(litContext.loaderCallCount() === 1, "Lit opt-in starts one loader");
  assert(litLane.metricsGridEl.children.length === 1, "Lit renderer takes over");
  assert(litLane.metricsGridEl.children[0].tagName === "lit-stub", "Lit island renders");
  assert(
    litLane.metricsGridEl.children[0].model.cells[0].value === "5",
    "Lit renderer receives shared model",
  );

  litContext.renderLaneMetricsPane(litLane);

  assert(litContext.loaderCallCount() === 1, "Lit loader is reused");
  assert(
    litLane.metricsGridEl.children[0].model.cells[4].value === "1h",
    "Lit renderer keeps duration formatting",
  );
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
