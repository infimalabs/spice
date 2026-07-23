const fs = require("fs");
const vm = require("vm");

const engineSource = fs.readFileSync(process.argv[2], "utf8");
const wetFrozenSource = fs.readFileSync(process.argv[3], "utf8");
const fullReplaySource = fs.readFileSync(process.argv[4], "utf8");
const eventLogSource = fs.readFileSync(process.argv[5], "utf8");
const laneStoreSource = fs.readFileSync(process.argv[6], "utf8");
const renderSource = fs.readFileSync(process.argv[7], "utf8");
const liveBusSource = fs.readFileSync(process.argv[8], "utf8");
const streamSource = fs.readFileSync(process.argv[9], "utf8");
const context = {
  console,
  Map,
  Set,
  WebSocket: { OPEN: 1, CONNECTING: 0 },
  window: { location: { protocol: "http:", host: "localhost" } },
};
vm.createContext(context);
vm.runInContext(engineSource, context, { filename: "app.mosaic-engine.js" });
vm.runInContext(wetFrozenSource, context, {
  filename: "app.mosaic-wet-frozen.js",
});
vm.runInContext(fullReplaySource, context, {
  filename: "app.mosaic-full-replay.js",
});
vm.runInContext(eventLogSource, context, {
  filename: "app.mosaic-event-log.js",
});
vm.runInContext(laneStoreSource, context, { filename: "app.lane-store.js" });
vm.runInContext(renderSource, context, { filename: "app.render.js" });
vm.runInContext(liveBusSource, context, { filename: "app.live-bus.js" });
vm.runInContext(streamSource, context, { filename: "app.stream.js" });

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

const TRACKS = 12;
const FREEZE = 2;
const RAPID_ARRIVAL_INSERTS = 30;
const INTERLEAVED_ROUNDS = 10;
const FULL_REPLAY_CARDS = 12;

// Tag every card by its stable key and replay the event log one event at a
// time through the SHIPPED mosaicReplayEventLog, so the per-event layouts are
// exactly what the live incremental path commits (freeze is a monotonic
// latch, so replaying prefix 0..k from empty reaches the same latch state as
// incrementally processing 0..k). A card that changes its column (t) across
// any event other than a full replay is placement jitter -- the operator's
// "cards jump around" -- and this audit surfaces it deterministically without
// a browser or the eye.
function layoutsPerEvent(log) {
  const layouts = [];
  for (let k = 0; k < log.events.length; k += 1) {
    const prefix = { ...log, events: log.events.slice(0, k + 1) };
    layouts.push(context.mosaicReplayEventLog(prefix).layout);
  }
  return layouts;
}

function byKey(layout) {
  const map = new Map();
  for (const card of layout) map.set(card.key, card);
  return map;
}

function fixtureCardsAreSeparated(a, b) {
  const tracksAreSeparated =
    a.t + a.span <= b.t || b.t + b.span <= a.t;
  const rowsAreSeparated = a.b + a.n <= b.b || b.b + b.n <= a.b;
  return tracksAreSeparated || rowsAreSeparated;
}

// Column stability: for every card present in two consecutive per-event
// layouts, its column t is identical unless the event that produced the
// second layout is a full replay (the one sanctioned re-column path, a
// geometry change deliberately re-deciding the whole board). Returns the
// count of cards that had to slide vertically (b changed, t held) so callers
// can prove the wet re-rest actually ran rather than passing vacuously.
function auditColumnStability(name, log) {
  const layouts = layoutsPerEvent(log);
  let verticalPushes = 0;
  for (let k = 1; k < layouts.length; k += 1) {
    const event = log.events[k];
    if (event.type === "full-replay") continue;
    const prev = byKey(layouts[k - 1]);
    for (const [key, after] of byKey(layouts[k])) {
      const before = prev.get(key);
      if (!before) continue;
      assert(
        before.t === after.t,
        name +
          ": card " +
          key +
          " changed column on a " +
          event.type +
          " event (t " +
          before.t +
          " -> " +
          after.t +
          "); content resolution must move cards vertically, never sideways",
      );
      if (before.b !== after.b) verticalPushes += 1;
    }
    for (let i = 0; i < layouts[k].length; i += 1) {
      for (let j = i + 1; j < layouts[k].length; j += 1) {
        assert(
          fixtureCardsAreSeparated(layouts[k][i], layouts[k][j]),
          name + ": every card pair is separated after a " + event.type + " event",
        );
      }
    }
  }
  return verticalPushes;
}

function newLog() {
  return context.mosaicCreateEventLog(TRACKS, FREEZE);
}
function insert(log, key, creationIndex, candidates) {
  context.mosaicRecordEventLogEvent(log, {
    type: "insert",
    key,
    creationIndex,
    candidates,
  });
}
function resize(log, key, creationIndex, n) {
  context.mosaicRecordEventLogEvent(log, {
    type: "content-resize",
    key,
    creationIndex,
    n,
  });
}

let totalVerticalPushes = 0;

// Rapid arrival: sequential inserts must never disturb an already-placed
// card's column (insert is zero-movement for existing cards).
{
  const log = newLog();
  for (let i = 0; i < RAPID_ARRIVAL_INSERTS; i += 1)
    insert(log, "c" + i, i, [{ span: [2, 3, 4, 6][i % 4], n: 1 + (i % 4) }]);
  totalVerticalPushes += auditColumnStability("rapid-arrival", log);
}

// A wide card resting over two narrower ones whose oldest sibling then
// resolves tall: the wide card must slide up in place, never hop to the
// freshly-flattened column its neighbour vacated.
{
  const log = newLog();
  insert(log, "A", 0, [{ span: 4, n: 2 }]);
  insert(log, "B", 1, [{ span: 4, n: 2 }]);
  insert(log, "C", 2, [{ span: 4, n: 2 }]);
  insert(log, "D", 3, [{ span: 8, n: 2 }]);
  resize(log, "A", 0, 5);
  totalVerticalPushes += auditColumnStability("wide-over-growing", log);
}

// Two client-only content resizes in sequence: each must preserve a bystander
// card's column. This pure replay case deliberately makes no claim about ACK
// reconciliation; the event-backed trace below exercises that separate seam.
{
  const log = newLog();
  insert(log, "m0", 0, [{ span: 6, n: 2 }]);
  insert(log, "contentA", 1, [{ span: 6, n: 2 }]);
  insert(log, "contentB", 2, [{ span: 6, n: 2 }]);
  insert(log, "m3", 3, [{ span: 6, n: 2 }]);
  resize(log, "contentA", 1, 4);
  resize(log, "contentB", 2, 5);
  totalVerticalPushes += auditColumnStability("two-content-resizes", log);
}

// A longer interleaved stream of inserts and staged content resolutions,
// mirroring live newest-first arrival with late-arriving quotes/images.
{
  const log = newLog();
  let idx = 0;
  for (let round = 0; round < INTERLEAVED_ROUNDS; round += 1) {
    insert(log, "s" + idx, idx, [{ span: [3, 4, 6][idx % 3], n: 1 + (idx % 3) }]);
    idx += 1;
    if (round % 2 === 1) resize(log, "s" + (idx - 2), idx - 2, 3 + (idx % 2));
  }
  totalVerticalPushes += auditColumnStability("interleaved-stream", log);
}

// The audit is not vacuous: across the fixtures above, real wet cards did
// slide vertically to clear growing neighbours -- proving wetReplay's re-rest
// ran and held the column rather than simply never moving anything.
assert(
  totalVerticalPushes > 0,
  "column-stability audit never observed a vertical re-rest; the wet re-place " +
    "path was not exercised, so the no-column-hop result is vacuous",
);

// A full replay is the one sanctioned re-column path and stays deterministic:
// replaying the same log twice yields a byte-identical layout.
{
  const log = newLog();
  for (let i = 0; i < FULL_REPLAY_CARDS; i += 1)
    insert(log, "r" + i, i, [{ span: [2, 4, 6][i % 3], n: 1 + (i % 3) }]);
  context.mosaicRecordEventLogEvent(log, {
    type: "full-replay",
    trackCount: TRACKS,
    cards: Array.from({ length: FULL_REPLAY_CARDS }, (_, i) => ({
      key: "r" + i,
      creationIndex: i,
      candidates: [{ span: [6, 3, 4][i % 3], n: 1 + ((i + 1) % 3) }],
    })),
  });
  const first = context.mosaicReplayEventLog(log).layout;
  const second = context.mosaicReplayEventLog(log).layout;
  assert(
    JSON.stringify(first) === JSON.stringify(second),
    "full replay must stay deterministic across repeated replays",
  );
}

function liveBusLane() {
  return {
    targetId: "lane",
    closed: false,
    knownMessages: [],
    knownMessageKeys: new Set(),
    newestMessageKey: "",
    oldestMessageKey: "",
    retainedMessageLimit: 10,
    occupants: new Map(),
    targetThreadId: "thread-a",
    activeThreadId: "thread-a",
    ackContextByKey: new Map(),
    missingAckContextKeys: new Set(),
    recentSentAckKeys: [],
    speechPrimed: true,
  };
}

function liveBusMessage(key, index, ackKey = "") {
  return {
    key,
    index,
    timestamp: "2026-07-23T03:00:0" + index + ".000Z",
    kind: "assistant",
    display_text: key,
    display_html: "<p>" + key + "</p>",
    ack_keys: ackKey ? [ackKey] : [],
  };
}

// Event-backed attribution: serialized lane.append frames enter through the
// shipped LiveBus dispatch. The handler boundary records payload arrival, the
// real applyPayloadAckContexts function records reconciliation, and the render
// callback drives the shipped mosaic event log before recording coordinates.
// Logical stamps capture causal order; coordinates measure the jitter itself.
function configureLiveBusEventFixture(subject) {
  const laneStore = vm.runInContext("laneStore", context);
  laneStore.registerLane(subject);
  context.laneGroupHost = (lane) => lane;
  context.laneGroupMemberLanes = (lane) => [lane];
  context.reconcileLaneSubmissionMessages = () => false;
  context.syncLaneSubmissionLifecycleUi = () => {};
  context.queueSpeechForMessages = () => {};
  context.primeSpeechBoundary = (lane) => {
    lane.speechPrimed = true;
  };
}

function realEventTrace(subject) {
  const ackKeyByFrame = new Map([
    ["ack-a", "quoted-a"],
    ["ack-b", "quoted-b"],
  ]);
  const trace = {
    subject,
    log: newLog(),
    layout: [],
    logicalStamp: 0,
    activeFrame: "",
    seeded: false,
    resolvedFrames: new Set(),
    observations: [],
  };
  trace.observe = (stage, layoutEvent) => {
    const card = trace.layout.find((candidate) => candidate.key === "m3");
    const ackKey = ackKeyByFrame.get(trace.activeFrame) || "";
    const ackState = ackKey
      ? subject.ackContextByKey.has(ackKey)
        ? "resolved"
        : "pending"
      : "initial";
    const position = card ? "t" + card.t + ",b" + card.b : "unplaced";
    trace.observations.push(
      [
        ++trace.logicalStamp,
        stage,
        trace.activeFrame,
        ackState,
        layoutEvent,
        position,
      ].join("|"),
    );
  };
  return trace;
}

function installLiveBusEventObservers(trace) {
  const handlers = vm.runInContext("liveBusPushHandlers", context);
  const appendHandler = handlers.get("lane.append");
  handlers.set("lane.append", async (message) => {
    trace.activeFrame = message.payload.traceFrame;
    trace.observe("server-payload-arrival", "awaiting-handler");
    try {
      return await appendHandler(message);
    } finally {
      trace.activeFrame = "";
    }
  });
  const applyAckContexts = context.applyPayloadAckContexts;
  context.applyPayloadAckContexts = (lane, payload) => {
    applyAckContexts(lane, payload);
    if ((payload.ackContexts || []).length)
      trace.observe("ack-reconciled", "awaiting-layout");
  };
}

function installRealEventLayoutObserver(trace) {
  context.renderMessagesIfChanged = () => {
    if (!trace.seeded) {
      insert(trace.log, "m0", 0, [{ span: 6, n: 2 }]);
      insert(trace.log, "ackA", 1, [{ span: 6, n: 2 }]);
      insert(trace.log, "ackB", 2, [{ span: 6, n: 2 }]);
      insert(trace.log, "m3", 3, [{ span: 6, n: 2 }]);
      trace.seeded = true;
    }
    if (
      trace.subject.ackContextByKey.has("quoted-a") &&
      !trace.resolvedFrames.has("ack-a")
    ) {
      resize(trace.log, "ackA", 1, 4);
      trace.resolvedFrames.add("ack-a");
    }
    if (
      trace.subject.ackContextByKey.has("quoted-b") &&
      !trace.resolvedFrames.has("ack-b")
    ) {
      resize(trace.log, "ackB", 2, 5);
      trace.resolvedFrames.add("ack-b");
    }
    trace.layout = context.mosaicReplayEventLog(trace.log).layout;
    trace.observe(
      "layout-applied",
      trace.activeFrame === "initial" ? "insert" : "content-resize",
    );
  };
}

async function deliverTraceFrame(subject, traceFrame, messages, ackContexts = []) {
  await context.handleLiveBusMessage(
    JSON.stringify({
      type: "lane.append",
      targetId: subject.targetId,
      payload: { traceFrame, messages, ackContexts },
    }),
  );
}

function expectedRealEventTrace() {
  return [
    "1|server-payload-arrival|initial|initial|awaiting-handler|unplaced",
    "2|layout-applied|initial|initial|insert|t6,b2",
    "3|server-payload-arrival|ack-a|pending|awaiting-handler|t6,b2",
    "4|ack-reconciled|ack-a|resolved|awaiting-layout|t6,b2",
    "5|layout-applied|ack-a|resolved|content-resize|t6,b4",
    "6|server-payload-arrival|ack-b|pending|awaiting-handler|t6,b4",
    "7|ack-reconciled|ack-b|resolved|awaiting-layout|t6,b4",
    "8|layout-applied|ack-b|resolved|content-resize|t6,b4",
  ];
}

async function auditRealEventSeams() {
  const subject = liveBusLane();
  configureLiveBusEventFixture(subject);
  const trace = realEventTrace(subject);
  installLiveBusEventObservers(trace);
  installRealEventLayoutObserver(trace);
  await deliverTraceFrame(subject, "initial", [
    liveBusMessage("m3", 3),
    liveBusMessage("ackB", 2, "quoted-b"),
    liveBusMessage("ackA", 1, "quoted-a"),
    liveBusMessage("m0", 0),
  ]);
  await deliverTraceFrame(subject, "ack-a", [], [
    { key: "quoted-a", found: true, text: "first resolved quote" },
  ]);
  await deliverTraceFrame(subject, "ack-b", [], [
    { key: "quoted-b", found: true, text: "second resolved quote" },
  ]);
  assert(
    JSON.stringify(trace.observations) ===
      JSON.stringify(expectedRealEventTrace()),
    "real event seams must produce the exact arrival, reconciliation, and " +
      "layout coordinate trace",
  );
}

auditRealEventSeams()
  .then(() => console.log("mosaic jitter: all assertions passed"))
  .catch((error) => {
    console.error(error.stack || error.message);
    process.exit(1);
  });
