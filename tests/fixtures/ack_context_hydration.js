const fs = require("fs");
const vm = require("vm");

const ctx = {
  Array,
  Boolean,
  Date,
  JSON,
  Map,
  Math,
  Number,
  Object,
  Set,
  String,
  console,
};
ctx.window = ctx;
ctx.document = { querySelectorAll: () => [] };
ctx.payloadHasField = () => false;
ctx.targetIdentityThreadId = () => "";
ctx.laneGroupHost = (lane) => lane;
ctx.laneGroupMemberLanes = (lane) => [lane];
vm.createContext(ctx);
vm.runInContext(fs.readFileSync(process.argv[2], "utf8"), ctx);

function buildLane() {
  return {
    ackContextByKey: new Map(),
    activeThreadId: "thread-1",
    knownMessageKeys: new Set(),
    knownMessages: [],
    missingAckContextKeys: new Set(),
    occupants: new Map(),
    recentSentAckKeys: [],
    retainedMessageLimit: 50,
  };
}

function buildMessage(key, ackKey) {
  return {
    ack_count: 1,
    ack_keys: [ackKey],
    ack_segments: [{ keys: [ackKey], html: "" }],
    index: 1,
    key,
    kind: "assistant",
    threadId: "thread-1",
    timestamp: "2026-07-23T04:00:00.000Z",
  };
}

function buildContext(key, text) {
  return {
    attachments: [],
    found: true,
    html: "<p>" + text + "</p>",
    key,
    priority: "",
    text,
  };
}

function contextState(lane, key) {
  const context = ctx.ackContextForKey(lane, key);
  if (context) return "resolved";
  if (ctx.ackContextConfirmedMissing(lane, key)) return "missing";
  return "pending";
}

let activeTrace = null;
const originalUpsertKnownMessage = ctx.upsertKnownMessage;
ctx.upsertKnownMessage = function (lane, item, position) {
  if (activeTrace) activeTrace.push(contextState(lane, item.ack_keys[0]));
  return originalUpsertKnownMessage(lane, item, position);
};

function mergeWithTrace(lane, payload) {
  activeTrace = [];
  ctx.mergePayloadMessages(lane, payload);
  const trace = activeTrace;
  activeTrace = null;
  return trace;
}

const initialLane = buildLane();
const initialKey = "initial-key";
const initialTrace = mergeWithTrace(initialLane, {
  messages: [buildMessage("initial-message", initialKey)],
  ackContexts: [buildContext(initialKey, "initial context")],
});

const reconnectLane = buildLane();
const reconnectKey = "reconnect-key";
mergeWithTrace(reconnectLane, {
  messages: [buildMessage("reconnect-message", reconnectKey)],
  ackContexts: [{ key: reconnectKey, found: false }],
});
const reconnectTrace = mergeWithTrace(reconnectLane, {
  messages: [buildMessage("reconnect-message", reconnectKey)],
  ackContexts: [buildContext(reconnectKey, "reconnected context")],
});

const earlyLane = buildLane();
const earlyKey = "early-key";
ctx.applyPayloadAckContexts(earlyLane, {
  ackContexts: [buildContext(earlyKey, "early context")],
});
const earlyTrace = mergeWithTrace(earlyLane, {
  messages: [buildMessage("early-message", earlyKey)],
});

process.stdout.write(
  JSON.stringify({
    early: {
      final: contextState(earlyLane, earlyKey),
      text: earlyLane.ackContextByKey.get(earlyKey).text,
      trace: earlyTrace,
    },
    initial: {
      final: contextState(initialLane, initialKey),
      text: initialLane.ackContextByKey.get(initialKey).text,
      trace: initialTrace,
    },
    reconnect: {
      final: contextState(reconnectLane, reconnectKey),
      text: reconnectLane.ackContextByKey.get(reconnectKey).text,
      trace: reconnectTrace,
    },
  }),
);
