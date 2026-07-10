const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const maxLifecycleTransitionMs = 500;

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-submission-lifecycle-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: [
          "handleLiveBusMessage",
          "laneGroupHost",
          "laneGroupMemberLanes",
          "sendLanePayload",
          "syncComposerShards",
        ],
      });
      await installSubmissionLifecycleSmokeHelpers(page);
      const result = await page.evaluate(runSubmissionLifecycleSmokePage);
      assertSubmissionLifecycleResult(result);
      return { ...result, url: server.url };
    },
  );
}

async function installSubmissionLifecycleSmokeHelpers(page) {
  await page.addScriptTag({
    content: [
      submissionSmokeStage,
      submissionSmokeLifecycle,
      submissionSmokeAcceptedLifecycle,
      submissionSmokeReceivedLifecycle,
      submissionSmokeCompletedLifecycle,
      submissionSmokeLaneMessage,
      submissionSmokeLanePayload,
      submissionSmokeSnapshot,
      submissionSmokeSend,
      submissionSmokePushSubmission,
      submissionSmokePushPayload,
      setupSubmissionLifecycleSmokePage,
      cleanupSubmissionLifecycleSmokePage,
      runSubmissionRunningScenario,
      runSubmissionIdleScenario,
      runSubmissionLifecycleSmokePage,
    ]
      .map((helper) => helper.toString())
      .join("\n"),
  });
}

function submissionSmokeStage(at, source, evidence) {
  return { at, source, evidence, sourceAt: at };
}

function submissionSmokeLifecycle(key, current, stages, disposition = "") {
  return {
    key,
    stage: current,
    disposition,
    stages,
    durationsMs: {},
  };
}

function submissionSmokeAcceptedLifecycle(key) {
  const at = new Date().toISOString();
  return submissionSmokeLifecycle(key, "accepted", {
    accepted: submissionSmokeStage(at, "inbox-write", key),
  });
}

function submissionSmokeReceivedLifecycle(
  key,
  accepted,
  at,
  disposition = "acked",
) {
  return submissionSmokeLifecycle(
    key,
    "received",
    {
      accepted: accepted.stages.accepted,
      received: submissionSmokeStage(
        at,
        disposition === "refused" ? "ack-state" : "transcript",
        key,
      ),
    },
    disposition,
  );
}

function submissionSmokeCompletedLifecycle(key, received, message) {
  return submissionSmokeLifecycle(
    key,
    "completed",
    {
      ...received.stages,
      completed: submissionSmokeStage(
        message.timestamp,
        message.kind === "reply" ? "reply-log" : "transcript",
        message.key,
      ),
    },
    received.disposition,
  );
}

function submissionSmokeLaneMessage(
  key,
  index,
  kind,
  text,
  ackKeys = [],
  nackKeys = [],
) {
  return {
    ack_count: ackKeys.length,
    ack_keys: ackKeys,
    display_html: "<p>" + text + "</p>",
    display_text: text,
    index,
    key,
    kind,
    nack_count: nackKeys.length,
    nack_keys: nackKeys,
    text,
    threadId: "submission-smoke-thread",
    timestamp: new Date(Date.now() + index).toISOString(),
  };
}

function submissionSmokeLanePayload(messages, processStatus) {
  const latest = messages[messages.length - 1];
  const pending = {
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "submission-smoke-" + latest.index,
    pendingInboxVersion: latest.index,
  };
  const complete = latest.kind === "final" || latest.kind === "reply";
  return {
    ...pending,
    messages,
    statusLine: {
      ...pending,
      activityStatus: complete ? "inactive" : "active",
      agentProcessStatus: processStatus,
      agentVisualStatus:
        processStatus === "running" && !complete ? "running" : "idle",
      lastAssistantAt: latest.timestamp,
      latestActivityKind: latest.kind,
      latestActivityPreview: latest.display_text,
      preview: latest.display_text,
    },
  };
}

function submissionSmokeSnapshot(lane, key) {
  const submission = lane.submissionLifecycleByKey.get(key);
  const status = lane.element.querySelector(
    '.composer-submission-status[data-submission-key="' + key + '"]',
  );
  const header = status ? status.closest(".composer-band-header") : null;
  const body = header ? header.querySelector(".composer-band-body") : null;
  const statusRect = status ? status.getBoundingClientRect() : null;
  const bodyRect = body ? body.getBoundingClientRect() : null;
  return {
    disposition: submission ? submission.disposition : "",
    href: status && status.href ? status.getAttribute("href") : "",
    headerGapPx: statusRect && bodyRect ? bodyRect.left - statusRect.right : 0,
    key: status ? status.dataset.submissionKey || "" : "",
    mapSize: lane.submissionLifecycleByKey.size,
    ownKeyPresent: lane.submissionLifecycleByKey.has(key),
    placeholder: Array.from(lane.shardTextareas.values())[0]?.placeholder || "",
    responseKey: status ? status.dataset.submissionResponseKey || "" : "",
    stage: status ? status.dataset.submissionStage || "" : "",
    statusFits: status ? status.scrollWidth <= status.clientWidth : false,
    tagName: status ? status.tagName : "",
  };
}

async function submissionSmokeSend(state, lane, text) {
  const startedAt = performance.now();
  await sendLanePayload(lane, { text }, lane);
  return {
    elapsedMs: performance.now() - startedAt,
    snapshot: submissionSmokeSnapshot(lane, state.keys.get(lane.targetId)),
  };
}

async function submissionSmokePushSubmission(state, lane, submission) {
  const startedAt = performance.now();
  await handleLiveBusMessage(
    JSON.stringify({
      type: "lane.submission",
      targetId: lane.targetId,
      source: "watch",
      submission,
    }),
  );
  return {
    elapsedMs: performance.now() - startedAt,
    snapshot: submissionSmokeSnapshot(lane, submission.key),
  };
}

async function submissionSmokePushPayload(state, lane, messages, processStatus) {
  const startedAt = performance.now();
  await handleLiveBusMessage(
    JSON.stringify({
      type: "lane.payload",
      targetId: lane.targetId,
      source: "watch",
      payload: submissionSmokeLanePayload(messages, processStatus),
    }),
  );
  return {
    elapsedMs: performance.now() - startedAt,
    snapshot: submissionSmokeSnapshot(lane, state.keys.get(lane.targetId)),
  };
}

function setupSubmissionLifecycleSmokePage() {
  const running = resolveIsolatedLane("submission-running-smoke-team");
  const idle = resolveIsolatedLane("submission-idle-smoke-team");
  for (const lane of [running, idle]) {
    const host = laneGroupHost(lane);
    syncComposerShards(host, laneGroupMemberLanes(host));
  }
  const state = {
    idle,
    running,
    keys: new Map([
      [running.targetId, "submission-running-key"],
      [idle.targetId, "submission-idle-key"],
    ]),
    originalLiveBusRequest: liveBusRequest,
  };
  liveBusRequest = async (type, fields = {}) => {
    if (type !== "lane.send") return state.originalLiveBusRequest(type, fields);
    const key = state.keys.get(fields.targetId);
    return {
      result: {
        ok: true,
        agentEnsure: { ok: true },
        key,
        pendingInboxCount: 1,
        pendingInboxKeys: [key],
        pendingInboxRevision: "submission-accepted-" + key,
        pendingInboxVersion: 1,
        requestText: (fields.payload || {}).text || "",
        submission: submissionSmokeAcceptedLifecycle(key),
      },
    };
  };
  window.__spiceSubmissionLifecycleSmoke = state;
  return state;
}

function cleanupSubmissionLifecycleSmokePage() {
  const state = window.__spiceSubmissionLifecycleSmoke;
  if (!state) return;
  liveBusRequest = state.originalLiveBusRequest;
  delete window.__spiceSubmissionLifecycleSmoke;
}

async function runSubmissionRunningScenario(state) {
  const lane = state.running;
  const key = state.keys.get(lane.targetId);
  const accepted = await submissionSmokeSend(state, lane, "running lane request");
  const acceptedLifecycle = lane.submissionLifecycleByKey.get(key);
  const receivedEvent = submissionSmokeReceivedLifecycle(
    key,
    acceptedLifecycle,
    new Date().toISOString(),
  );
  const received = await submissionSmokePushSubmission(state, lane, receivedEvent);
  const finalMessage = submissionSmokeLaneMessage(
    "running-final-message",
    2,
    "final",
    "Running lane completed.",
  );
  const completed = await submissionSmokePushPayload(
    state,
    lane,
    [
      submissionSmokeLaneMessage(
        "running-ack-message",
        1,
        "assistant",
        "ACK running request",
        [key],
      ),
      finalMessage,
    ],
    "running",
  );
  const replay = await submissionSmokePushSubmission(
    state,
    lane,
    submissionSmokeCompletedLifecycle(key, receivedEvent, finalMessage),
  );
  return { accepted, completed, received, replay };
}

async function runSubmissionIdleScenario(state) {
  const lane = state.idle;
  const key = state.keys.get(lane.targetId);
  const accepted = await submissionSmokeSend(state, lane, "idle lane request");
  const acceptedLifecycle = lane.submissionLifecycleByKey.get(key);
  const receivedEvent = submissionSmokeReceivedLifecycle(
    key,
    acceptedLifecycle,
    new Date().toISOString(),
    "refused",
  );
  const received = await submissionSmokePushSubmission(state, lane, receivedEvent);
  const reply = submissionSmokeLaneMessage(
    "idle-reply-message",
    2,
    "reply",
    "NACK idle request",
    [key],
    [key],
  );
  const completed = await submissionSmokePushPayload(state, lane, [reply], "idle");
  const replay = await submissionSmokePushSubmission(state, lane, receivedEvent);
  return { accepted, completed, received, replay };
}

async function runSubmissionLifecycleSmokePage() {
  const state = setupSubmissionLifecycleSmokePage();
  try {
    const running = await runSubmissionRunningScenario(state);
    const idle = await runSubmissionIdleScenario(state);
    return {
      idle,
      memberScope: {
        idleOwnOnly:
          state.idle.submissionLifecycleByKey.size === 1 &&
          state.idle.submissionLifecycleByKey.has(state.keys.get(state.idle.targetId)),
        runningOwnOnly:
          state.running.submissionLifecycleByKey.size === 1 &&
          state.running.submissionLifecycleByKey.has(
            state.keys.get(state.running.targetId),
          ),
      },
      running,
    };
  } finally {
    cleanupSubmissionLifecycleSmokePage();
  }
}

function assertSubmissionLifecycleResult(result) {
  for (const lane of [result.running, result.idle]) {
    for (const stage of ["accepted", "received", "completed"]) {
      const transition = lane[stage];
      if (transition.elapsedMs > maxLifecycleTransitionMs)
        throw new Error(stage + " transition exceeded bound: " + transition.elapsedMs);
      if (transition.snapshot.stage !== stage)
        throw new Error(stage + " progress mismatch: " + JSON.stringify(transition));
      if (!transition.snapshot.placeholder.includes("steer " + stage))
        throw new Error(stage + " composer feedback mismatch: " + JSON.stringify(transition));
      if (!transition.snapshot.statusFits || transition.snapshot.headerGapPx < 0)
        throw new Error(stage + " composer status layout mismatch: " + JSON.stringify(transition));
    }
    if (lane.completed.snapshot.tagName !== "A")
      throw new Error("completed progress must link to its response");
    if (lane.completed.snapshot.responseKey === "")
      throw new Error("completed progress is missing response identity");
    if (!lane.completed.snapshot.href.endsWith(lane.completed.snapshot.responseKey))
      throw new Error("completed response link mismatch: " + JSON.stringify(lane.completed));
    if (!lane.completed.snapshot.placeholder.includes("0 pending, idle"))
      throw new Error("completed lane feedback mismatch: " + JSON.stringify(lane.completed));
    if (lane.replay.snapshot.stage !== "completed")
      throw new Error("lower-stage replay moved completed progress backwards");
  }
  if (!result.memberScope.runningOwnOnly || !result.memberScope.idleOwnOnly)
    throw new Error("submission lifecycle leaked across lane members");
  if (result.running.completed.snapshot.disposition !== "acked")
    throw new Error("running completion disposition mismatch");
  if (result.idle.completed.snapshot.disposition !== "refused")
    throw new Error("idle completion disposition mismatch");
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
