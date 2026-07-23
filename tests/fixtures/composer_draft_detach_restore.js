const fs = require("fs");
const vm = require("vm");

const composerPath = process.argv[2];
const context = {
  console,
  attachmentStripRenderCount: 0,
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
context.renderComposerAttachmentStrips = () => {
  context.attachmentStripRenderCount += 1;
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
  ]),
  shardAttachments: new Map([
    ["agent-a", [{ id: "attachment-sent" }]],
    ["agent-b", [{ id: "attachment-other" }]],
  ]),
  quoteDrafts: new Map([
    [
      "agent-a",
      [{ id: "quote-sent", quoteText: "quoted sent", text: "sent context" }],
    ],
    [
      "agent-c",
      [{ id: "quote-other", quoteText: "unrelated quote", text: "Keep this context" }],
    ],
  ]),
};

const detached = context.detachLaneComposerDraft(host, "agent-a");

assert(host.shardTextareas.get("agent-a").value === "", "origin draft clears");
assert(
  host.shardTextareas.get("agent-b").value === "Send this",
  "other agent duplicate draft remains",
);
assert(
  host.shardTextareas.get("agent-c").value === "Keep this",
  "unrelated draft remains",
);
assert(
  host.shardAttachments.get("agent-b")[0].id === "attachment-other",
  "other agent attachment remains",
);
assert(
  host.quoteDrafts.get("agent-c")[0].id === "quote-other",
  "other agent quote remains",
);
assert(detached.text === "Send this", "detached text is retained");
assert(detached.attachments[0].id === "attachment-sent", "attachment is retained");
assert(detached.quoteDrafts[0].id === "quote-sent", "quote is retained");

host.shardTextareas.get("agent-a").value = "Keep newer";
host.shardAttachments.set("agent-a", [{ id: "attachment-newer" }]);
host.quoteDrafts.set("agent-a", [
  { id: "quote-newer", quoteText: "newer quote", text: "newer context" },
]);
context.restoreLaneComposerDrafts(host, "agent-a", [detached]);

assert(
  host.shardTextareas.get("agent-a").value === "Send this\n\nKeep newer",
  "detached text restores ahead of newer text",
);
assert(
  host.shardAttachments.get("agent-a").map((item) => item.id).join(",") ===
    "attachment-sent,attachment-newer",
  "attachments restore in draft order",
);
assert(
  host.quoteDrafts.get("agent-a").map((item) => item.id).join(",") ===
    "quote-sent,quote-newer",
  "quotes restore in draft order",
);
assert(context.quoteBandRenderCount === 2, "quote bands render on detach and restore");
assert(
  context.attachmentStripRenderCount === 2,
  "attachment strips render on detach and restore",
);
