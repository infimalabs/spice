const fs = require("fs");
const vm = require("vm");

const submissionsPath = process.argv[2];
const lifecycleFixtureInsertCount = 55;
const lifecycleFixtureLimit = 50;
const context = { console, Date, Map, Number, Object, Set, JSON };
vm.createContext(context);
vm.runInContext(fs.readFileSync(submissionsPath, "utf8"), context, {
  filename: "app.submissions.js",
});

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function lane(id) {
  return {
    targetId: id,
    knownMessageKeys: new Set(),
    submissionLifecycleByKey: new Map(),
  };
}

function stage(at, source, evidence) {
  return { at, source, evidence };
}

function lifecycle(key, current, stages) {
  return {
    key,
    stage: current,
    disposition: current === "accepted" ? "" : "acked",
    stages,
    durationsMs: {},
  };
}

const first = lane("first");
const sibling = lane("sibling");
const acceptedAt = "2026-07-10T00:00:00.000000Z";
const receivedAt = "2026-07-10T00:00:01.000000Z";
const completedAt = "2026-07-10T00:00:02.000000Z";
context.applyLaneSubmissionLifecycle(
  first,
  lifecycle("key-a", "accepted", {
    accepted: stage(acceptedAt, "inbox-write", "key-a"),
  }),
);
context.applyLaneSubmissionLifecycle(
  first,
  lifecycle("key-a", "completed", {
    accepted: stage(acceptedAt, "inbox-write", "key-a"),
    received: stage(receivedAt, "transcript", "message-ack"),
    completed: stage(completedAt, "transcript", "message-final"),
  }),
);
context.applyLaneSubmissionLifecycle(
  first,
  lifecycle("key-a", "received", {
    accepted: stage(acceptedAt, "inbox-write", "key-a"),
    received: stage(receivedAt, "transcript", "message-ack"),
  }),
);
assert(
  context.latestLaneSubmission(first).stage === "completed",
  "lower-stage replay must preserve completed lifecycle",
);
assert(
  context.latestLaneSubmission(sibling) === null,
  "submission state must remain scoped to its fused member",
);

const reconnect = lane("reconnect");
context.applyLaneSubmissionLifecycle(
  reconnect,
  lifecycle("key-b", "accepted", {
    accepted: stage(acceptedAt, "inbox-write", "key-b"),
  }),
);
const messages = [
  {
    key: "message-ack-b",
    index: 1,
    kind: "assistant",
    timestamp: receivedAt,
    ack_keys: ["key-b"],
    nack_keys: [],
  },
  {
    key: "message-final-b",
    index: 2,
    kind: "final",
    timestamp: completedAt,
    ack_keys: [],
    nack_keys: [],
  },
];
reconnect.knownMessageKeys = new Set(messages.map((message) => message.key));
context.reconcileLaneSubmissionMessages(reconnect, messages);
const reconnected = context.latestLaneSubmission(reconnect);
assert(reconnected.stage === "completed", "reconnect messages must restore completion");
assert(
  context.laneSubmissionCompletionMessageKey(reconnect, reconnected) ===
    "message-final-b",
  "completed lifecycle must link to the matching final response",
);

const bounded = lane("bounded");
for (let index = 0; index < lifecycleFixtureInsertCount; index += 1) {
  const key = "key-" + index;
  context.applyLaneSubmissionLifecycle(
    bounded,
    lifecycle(key, "accepted", {
      accepted: stage(acceptedAt, "inbox-write", key),
    }),
  );
}
assert(
  bounded.submissionLifecycleByKey.size === lifecycleFixtureLimit,
  "client lifecycle state must remain at its declared bound",
);
assert(
  context.latestLaneSubmission(bounded).key === "key-54",
  "client lifecycle bound must retain the newest submission",
);
