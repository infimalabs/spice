const fs = require("fs");
const vm = require("vm");

const storePath = process.argv[2];
const renderPath = process.argv[3];
const liveBusPath = process.argv[4];
const streamPath = process.argv[5];
const submissionsPath = process.argv[6];
const context = {
  console,
  Map,
  Set,
  WebSocket: { OPEN: 1, CONNECTING: 0 },
  window: { location: { protocol: "http:", host: "localhost" } },
};

vm.createContext(context);
vm.runInContext(fs.readFileSync(storePath, "utf8"), context, {
  filename: "app.lane-store.js",
});
vm.runInContext(fs.readFileSync(renderPath, "utf8"), context, {
  filename: "app.render.js",
});
vm.runInContext(fs.readFileSync(liveBusPath, "utf8"), context, {
  filename: "app.live-bus.js",
});
vm.runInContext(fs.readFileSync(streamPath, "utf8"), context, {
  filename: "app.stream.js",
});
vm.runInContext(fs.readFileSync(submissionsPath, "utf8"), context, {
  filename: "app.submissions.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function message(key, text) {
  return {
    key,
    index: Number(key.split("#").at(-1) || 0),
    timestamp: key.split("#")[0],
    kind: "assistant",
    display_text: text,
    display_html: "<p>" + text + "</p>",
    ack_keys: [],
  };
}

function lane() {
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
    optimisticPendingInboxCount: 0,
    optimisticSubmittedInboxKeys: new Set(),
    optimisticPendingInboxFloor: 0,
    pendingSubmissionCount: 0,
    sendAwaitingBackendCount: 0,
    submissionLifecycleByKey: new Map(),
    ackContextByKey: new Map(),
    missingAckContextKeys: new Set(),
    recentSentAckKeys: [],
    speechPrimed: true,
    speechPrimeStartedAt: Date.now(),
    latestPayload: null,
    viewShellRenders: 0,
    placeholderSyncs: 0,
    messageRenders: 0,
    queuedSpeechKeys: [],
  };
}

const laneStore = vm.runInContext("laneStore", context);
context.laneGroupHost = (item) => item;
context.renderLanePayloadPresentation = (item, payload) => {
  item.renderedChromePayload = payload;
};
context.renderLaneViewBadge = (item) => {
  item.viewShellRenders += 1;
};
context.syncComposerPlaceholders = (item) => {
  item.placeholderSyncs += 1;
};
context.renderMessagesIfChanged = (item) => {
  item.messageRenders += 1;
};
context.queueSpeechForMessages = (item, messages) => {
  item.queuedSpeechKeys.push(...messages.map((entry) => entry.key));
};
context.primeSpeechBoundary = (item) => {
  item.speechPrimed = true;
};
context.subscribeLaneToLiveBus = () => {};
context.refreshServerTopology = () => Promise.resolve();

function pendingChrome(targetId, version, count, keys) {
  return {
    targetId,
    pendingInbox: {
      authority: "inbox",
      order: { epoch: "", revision: version },
      value: { count, label: String(count), keys },
    },
  };
}

async function main() {
  const subject = lane();
  laneStore.registerLane(subject);
  const initial = message("2026-06-22T03:00:00.000000Z#1", "initial");
  const fullPayload = {
    messages: [initial],
    statusLine: {
      preview: "initial",
    },
    chrome: pendingChrome(subject.targetId, 1, 0, []),
  };

  await context.handleLiveBusMessage(
    JSON.stringify({
      type: "lane.payload",
      targetId: subject.targetId,
      payload: fullPayload,
    }),
  );
  assert(
    subject.latestPayload.statusLine.preview === fullPayload.statusLine.preview,
    "full payload remains fallback",
  );
  assert(subject.knownMessages[0].key === initial.key, "full payload seeded stream");
  assert(subject.messageRenders === 1, "full payload rendered messages");
  subject.viewShellRenders = 0;
  subject.placeholderSyncs = 0;

  await context.handleLiveBusMessage(
    JSON.stringify({
      type: "lane.pending",
      targetId: subject.targetId,
      payload: {
        chrome: pendingChrome(
          subject.targetId,
          2,
          2,
          ["inbox-a", "inbox-b"],
        ),
      },
    }),
  );
  const pendingMergedPayload = subject.latestPayload;
  assert(
    laneStore.laneChrome(subject.targetId).pendingInbox.count === 2,
    "pending frame updates canonical count",
  );
  assert(
    subject.latestPayload.statusLine.pendingInboxCount === 2,
    "pending frame updates cached status line",
  );
  assert(subject.viewShellRenders === 1, "pending frame rerenders compose badge");
  assert(subject.placeholderSyncs === 1, "pending frame syncs composer placeholders");

  await context.handleLiveBusMessage(
    JSON.stringify({
      type: "lane.pending",
      targetId: subject.targetId,
      payload: {
        chrome: pendingChrome(subject.targetId, 1, 0, []),
      },
    }),
  );
  assert(
    laneStore.laneChrome(subject.targetId).pendingInbox.count === 2,
    "stale pending frame is ignored",
  );
  assert(subject.viewShellRenders === 1, "stale pending frame does not rerender");

  const appended = message("2026-06-22T03:01:00.000000Z#2", "appended");
  await context.handleLiveBusMessage(
    JSON.stringify({
      type: "lane.append",
      targetId: subject.targetId,
      payload: { messages: [appended] },
    }),
  );
  assert(subject.latestPayload === pendingMergedPayload, "append keeps cached payload");
  assert(subject.knownMessages[0].key === appended.key, "append adds newest message");
  assert(subject.knownMessages[1].key === initial.key, "append preserves older message");
  assert(subject.messageRenders === 2, "append rerenders message list");
  assert(
    subject.queuedSpeechKeys.at(-1) === appended.key,
    "append queues fresh speech through existing path",
  );

  const replacement = message("2026-06-22T03:02:00.000000Z#3", "replacement");
  await context.handleLiveBusMessage(
    JSON.stringify({
      type: "lane.append",
      targetId: subject.targetId,
      payload: {
        messages: [replacement],
        removedMessageKeys: [initial.key],
      },
    }),
  );
  assert(
    subject.knownMessages[0].key === replacement.key,
    "append with removals adds replacement message",
  );
  assert(
    subject.knownMessages[1].key === appended.key,
    "append with removals preserves unrelated cached messages",
  );
  assert(
    !subject.knownMessageKeys.has(initial.key),
    "append with removals drops removed message key",
  );
  assert(subject.messageRenders === 3, "append with removals rerenders message list");
  assert(
    subject.queuedSpeechKeys.at(-1) === replacement.key,
    "append with removals queues speech for replacement",
  );
}

main().catch((error) => {
  console.error(error.stack || error.message);
  process.exit(1);
});
