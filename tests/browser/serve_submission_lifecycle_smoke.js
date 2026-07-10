const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const maxLifecycleTransitionMs = 500;
const narrowComposerWidthPx = 280;
const desktopComposerWidthPx = 560;
const maxCompactStatusFontPx = 12;
const maxComposerWidthDeltaPx = 3;

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
  const pageConstants = [
    "const narrowComposerWidthPx = " + narrowComposerWidthPx + ";",
    "const desktopComposerWidthPx = " + desktopComposerWidthPx + ";",
  ].join("\n");
  const pageHelpers = [
    submissionSmokeStage,
    submissionSmokeLifecycle,
    submissionSmokeAcceptedLifecycle,
    submissionSmokeReceivedLifecycle,
    submissionSmokeCompletedLifecycle,
    submissionSmokeLaneMessage,
    submissionSmokeLanePayload,
    submissionSmokeSetComposerWidth,
    submissionSmokeHeaderElements,
    submissionSmokeHeaderLayout,
    submissionSmokeHeaderSnapshot,
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
    .join("\n");
  await page.addScriptTag({
    content: pageConstants + "\n" + pageHelpers,
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
  const badge = lane.element.querySelector(
    '[data-lane-view-button="compose"] [data-lane-view-badge]',
  );
  const badgeVisible = Boolean(badge && !badge.hidden);
  return {
    ...submissionSmokeHeaderSnapshot(lane),
    completionMessageKey: submission
      ? laneSubmissionCompletionMessageKey(lane, submission)
      : "",
    disposition: submission ? submission.disposition : "",
    key: submission ? submission.key : "",
    mapSize: lane.submissionLifecycleByKey.size,
    ownKeyPresent: lane.submissionLifecycleByKey.has(key),
    pendingBadgeFontSizePx: badge
      ? Number.parseFloat(getComputedStyle(badge).fontSize)
      : 0,
    pendingBadgeText: badgeVisible ? badge.textContent || "" : "",
    pendingCount: lanePendingDisplayCount(lane),
    placeholder: Array.from(lane.shardTextareas.values())[0]?.placeholder || "",
    stage: submission ? submission.stage : "",
  };
}

function submissionSmokeHeaderSnapshot(lane) {
  const elements = submissionSmokeHeaderElements(lane);
  const { header, time, title } = elements;
  return {
    ...submissionSmokeHeaderLayout(elements),
    state: time.dataset.agentStatus || "",
    stateFontSizePx: Number.parseFloat(getComputedStyle(time).fontSize),
    timeHref: time.tagName === "A" ? time.getAttribute("href") || "" : "",
    timeTagName: time.tagName,
    timeText: time.textContent || "",
    titleText: title.textContent || "",
    titleWidthPx: title.getBoundingClientRect().width,
  };
}

function submissionSmokeHeaderElements(lane) {
  const header = lane.element.querySelector(".composer-band-header--primary");
  if (!header) throw new Error("submission smoke header is missing");
  const body = header.querySelector(".composer-band-body");
  const title = body && body.querySelector(".composer-band-title");
  const time = header.querySelector(".composer-latest-time");
  const menu = header.querySelector(".composer-band-menu-button");
  if (!body || !title || !time || !menu)
    throw new Error("submission smoke header structure is incomplete");
  return { body, header, menu, time, title };
}

function submissionSmokeHeaderLayout({ body, header, menu, time }) {
  const headerRect = header.getBoundingClientRect();
  const timeRect = time.getBoundingClientRect();
  const bodyRect = body.getBoundingClientRect();
  const menuRect = menu.getBoundingClientRect();
  return {
    headerDoesNotOverlap:
      timeRect.right <= bodyRect.left && bodyRect.right <= menuRect.left,
    headerWidthPx: headerRect.width,
    restoredHeaderStructure:
      header.children.length === 3 &&
      header.children[0] === time &&
      header.children[1] === body &&
      header.children[2] === menu,
  };
}

function submissionSmokeSetComposerWidth(lane, widthPx) {
  const primary = lane.element.querySelector(
    '[data-composer-primary-target-id="' + lane.targetId + '"]',
  );
  if (!primary) throw new Error("submission smoke composer is missing");
  primary.style.inlineSize = widthPx + "px";
  primary.style.minInlineSize = widthPx + "px";
  primary.style.maxInlineSize = widthPx + "px";
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
  const laneConfigs = [
    [running, "running", narrowComposerWidthPx],
    [idle, "idle", desktopComposerWidthPx],
  ];
  for (const [lane, state, widthPx] of laneConfigs) {
    lane.lastRenderedStatusLine = {
      agentProcessStatus: state,
      agentVisualStatus: state,
    };
    const host = laneGroupHost(lane);
    syncComposerShards(host, laneGroupMemberLanes(host));
    submissionSmokeSetComposerWidth(lane, widthPx);
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
  const laneCases = [
    [result.running, narrowComposerWidthPx, "running"],
    [result.idle, desktopComposerWidthPx, "idle"],
  ];
  for (const [lane, expectedWidthPx, initialState] of laneCases) {
    for (const stage of ["accepted", "received", "completed"]) {
      const transition = lane[stage];
      if (transition.elapsedMs > maxLifecycleTransitionMs)
        throw new Error(stage + " transition exceeded bound: " + transition.elapsedMs);
      if (transition.snapshot.stage !== stage)
        throw new Error(stage + " progress mismatch: " + JSON.stringify(transition));
      assertSubmissionHeaderIsClean(transition, expectedWidthPx, initialState, stage);
    }
    if (lane.completed.snapshot.completionMessageKey === "")
      throw new Error("completed lifecycle is missing response identity");
    if (lane.completed.snapshot.timeTagName !== "A")
      throw new Error("existing latest-message link was not preserved");
    if (!lane.completed.snapshot.timeHref.endsWith(lane.completed.snapshot.completionMessageKey))
      throw new Error("latest-message link mismatch: " + JSON.stringify(lane.completed));
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

function assertSubmissionHeaderIsClean(
  transition,
  expectedWidthPx,
  initialState,
  stage,
) {
  const snapshot = transition.snapshot;
  if (!snapshot.restoredHeaderStructure)
    throw new Error(stage + " header structure mismatch: " + JSON.stringify(transition));
  if (!snapshot.headerDoesNotOverlap || snapshot.titleWidthPx <= 0)
    throw new Error(stage + " header layout mismatch: " + JSON.stringify(transition));
  if (Math.abs(snapshot.headerWidthPx - expectedWidthPx) > maxComposerWidthDeltaPx)
    throw new Error(stage + " header width mismatch: " + JSON.stringify(transition));
  if (snapshot.stateFontSizePx > maxCompactStatusFontPx)
    throw new Error(stage + " state indicator is not compact: " + JSON.stringify(transition));
  const pending = stage === "completed" ? 0 : 1;
  const badge = pending ? String(pending) : "";
  const state = stage === "completed" ? "idle" : initialState;
  const placeholder = "empty team\n" + pending + " pending, " + state;
  if (snapshot.placeholder !== placeholder)
    throw new Error(stage + " placeholder mismatch: " + JSON.stringify(transition));
  if (snapshot.pendingCount !== pending || snapshot.pendingBadgeText !== badge)
    throw new Error(stage + " pending signal mismatch: " + JSON.stringify(transition));
  if (badge && snapshot.pendingBadgeFontSizePx > maxCompactStatusFontPx)
    throw new Error(stage + " pending badge is not compact: " + JSON.stringify(transition));
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
