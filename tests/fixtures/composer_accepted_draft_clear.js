const fs = require("fs");
const vm = require("vm");

const composerPath = process.argv[2];
const context = {
  console,
  quoteBandRenderCount: 0,
  laneGroupHost(lane) {
    return lane.groupHost || lane;
  },
  markdownBlockQuote(raw) {
    return String(raw || "")
      .replace(/\r\n?/g, "\n")
      .split("\n")
      .map((line) => "> " + line)
      .join("\n")
      .trim();
  },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(composerPath, "utf8"), context, {
  filename: "app.composer.js",
});
context.renderComposerQuoteBands = () => {
  context.quoteBandRenderCount += 1;
};

function textarea(value) {
  return { value };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const host = {
  shardTextareas: new Map([
    ["agent-a", textarea("Send this")],
    ["agent-b", textarea("Send this")],
    ["agent-c", textarea("Keep this")],
    ["agent-d", textarea("Send this")],
    ["agent-e", textarea("")],
    ["agent-f", textarea("")],
  ]),
  shardAttachments: new Map([["agent-d", [{ id: "attachment-1" }]]]),
  quoteDrafts: new Map([
    [
      "agent-e",
      [{ quoteText: "quoted duplicate", text: "Send this with context" }],
    ],
    ["agent-f", [{ quoteText: "unrelated quote", text: "Keep this context" }]],
  ]),
};

context.clearAcceptedComposerDrafts(host, "agent-a", "Send this");

assert(host.shardTextareas.get("agent-a").value === "", "origin draft clears");
assert(host.shardTextareas.get("agent-b").value === "", "duplicate draft clears");
assert(
  host.shardTextareas.get("agent-c").value === "Keep this",
  "unrelated draft remains",
);
assert(
  host.shardTextareas.get("agent-d").value === "",
  "duplicate text with attachment clears",
);
assert(
  host.shardAttachments.get("agent-d").length === 1,
  "non-origin attachment remains",
);

context.clearAcceptedComposerDrafts(
  host,
  "agent-a",
  "> quoted duplicate\n\nSend this with context",
);

assert(!host.quoteDrafts.has("agent-e"), "duplicate quote draft clears");
assert(host.quoteDrafts.has("agent-f"), "unrelated quote draft remains");
assert(context.quoteBandRenderCount === 1, "quote bands rerender after quote clear");
