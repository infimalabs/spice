const { withServePage } = require("./serve_playwright_harness");
const {
  targetPayload,
  teamPayload,
  teamSnapshot,
} = require("./payload_factory");

// A differential team snapshot for one lane must not repaint every other lane.
// The fixture seeds two real, settled mosaic lanes, wraps every render/write
// boundary, then advances only the active lane's team revision. Completion is
// callback-driven (MutationObserver microtask + animation frames), never a
// guessed quiet window.

const LANE_ISOLATION_CONTROL_ID = "lane-isolation-control";
const LANE_ISOLATION_ACTIVE_ID = "lane-isolation-active";
const LANE_ISOLATION_FULL_SPAN = 12;

function laneIsolationItem(prefix, index) {
  const text = prefix + " message " + index;
  return {
    ack_count: 0,
    ack_keys: [],
    display_html: text,
    display_text: text,
    index,
    key: prefix + "-" + index,
    kind: "assistant",
    text,
    timestamp: new Date(2026, 0, 1, 0, 0, index).toISOString(),
  };
}

function laneIsolationRect(node) {
  const rect = node.getBoundingClientRect();
  const rounded = (value) => Math.round(value * 1000) / 1000;
  return {
    bottom: rounded(rect.bottom),
    height: rounded(rect.height),
    left: rounded(rect.left),
    right: rounded(rect.right),
    top: rounded(rect.top),
    width: rounded(rect.width),
  };
}

function laneIsolationBounds(lane) {
  const cards = Array.from(
    lane.mosaicPlaneEl.querySelectorAll("[data-mosaic-card]"),
    (node) => ({ key: node.dataset.messageKey, ...laneIsolationRect(node) }),
  ).sort((a, b) => a.top - b.top || a.left - b.left || a.key.localeCompare(b.key));
  return {
    cards,
    host: laneIsolationRect(lane.messagesEl),
    topCard: cards[0] || null,
  };
}

function laneIsolationSnapshot(lane) {
  return {
    baseSpan: lane.mosaicGeometry.baseSpan,
    bounds: laneIsolationBounds(lane),
    cards: lane.mosaicCards.map((card) => ({ ...card })),
    fingerprint: lane.renderedMessageFingerprint,
    settled: Boolean(lane.mosaicSettled),
    styles: Array.from(
      lane.mosaicPlaneEl.querySelectorAll("[data-mosaic-card]"),
      (node) => ({
        height: node.style.height,
        key: node.dataset.messageKey,
        transform: node.style.transform,
        transition: node.style.transition,
        width: node.style.width,
      }),
    ),
    width: lane.messagesEl.clientWidth,
  };
}

function laneIsolationSeed(fixture) {
  laneStore.replaceTargets(fixture.targets);
  applyTeamSnapshotPayload(fixture.initialSnapshot, { force: true });
  const control = laneStore.laneForId(LANE_ISOLATION_CONTROL_ID);
  control.element.style.flex = "0 0 400px";
  for (const targetId of [
    LANE_ISOLATION_CONTROL_ID,
    LANE_ISOLATION_ACTIVE_ID,
  ]) {
    const lane = laneStore.laneForId(targetId);
    if (!lane) throw new Error("lane-isolation fixture did not create " + targetId);
    const items = Array.from({ length: 5 }, (_, index) =>
      laneIsolationItem(targetId, index + 1),
    );
    lane.knownMessages = items;
    lane.knownMessageKeys = new Set(items.map((item) => item.key));
    lane.retainedMessageLimit = items.length;
    lane.renderedMessageFingerprint = "";
    renderMessagesIfChanged(lane);
    lane.mosaicSettled = true;
  }
}

function laneIsolationAfterTwoFrames() {
  return new Promise((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(resolve));
  });
}

async function laneIsolationAwaitControlAnimations() {
  const lane = laneStore.laneForId(LANE_ISOLATION_CONTROL_ID);
  const animations = lane.element
    .getAnimations({ subtree: true })
    .filter(
      (animation) =>
        animation.playState !== "finished" &&
        animation.effect.getComputedTiming().iterations !== Infinity,
    );
  await Promise.allSettled(animations.map((animation) => animation.finished));
  await laneIsolationAfterTwoFrames();
}

function laneIsolationInstallTrace() {
  const control = laneStore.laneForId(LANE_ISOLATION_CONTROL_ID);
  const active = laneStore.laneForId(LANE_ISOLATION_ACTIVE_ID);
  const counts = () => ({
    cardWrites: 0,
    fullReplays: 0,
    groupSyncs: 0,
    messageRenders: 0,
    mosaicRenders: 0,
    scheduledRenders: 0,
  });
  const trace = {
    active: counts(),
    control: counts(),
    controlMosaicRendering: false,
    controlMutations: [],
    observer: null,
  };
  window.__laneIsolationTrace = trace;

  const bucket = (lane) => {
    if (lane === control) return trace.control;
    if (lane === active) return trace.active;
    return null;
  };

  const originalGroupSync = syncFusedLaneChrome;
  syncFusedLaneChrome = function (lane, ...args) {
    const laneCounts = bucket(lane);
    if (laneCounts) laneCounts.groupSyncs += 1;
    return originalGroupSync.call(this, lane, ...args);
  };

  const originalMessageRender = renderMessagesIfChanged;
  renderMessagesIfChanged = function (lane, ...args) {
    const laneCounts = bucket(lane);
    if (laneCounts) laneCounts.messageRenders += 1;
    return originalMessageRender.call(this, lane, ...args);
  };

  const originalScheduleRender = mosaicScheduleRender;
  mosaicScheduleRender = function (lane, ...args) {
    const laneCounts = bucket(laneGroupHost(lane));
    if (laneCounts) laneCounts.scheduledRenders += 1;
    return originalScheduleRender.call(this, lane, ...args);
  };

  const originalMosaicRender = mosaicRenderMessageStream;
  mosaicRenderMessageStream = function (lane, ...args) {
    const laneCounts = bucket(lane);
    if (laneCounts) laneCounts.mosaicRenders += 1;
    if (lane === control) trace.controlMosaicRendering = true;
    try {
      return originalMosaicRender.call(this, lane, ...args);
    } finally {
      if (lane === control) trace.controlMosaicRendering = false;
    }
  };

  const originalFullReplay = mosaicFullReplay;
  mosaicFullReplay = function (...args) {
    if (trace.controlMosaicRendering) trace.control.fullReplays += 1;
    return originalFullReplay.apply(this, args);
  };

  const originalCardApply = mosaicApplyCardPosition;
  mosaicApplyCardPosition = function (node, card, previous, ...args) {
    const next = originalCardApply.call(this, node, card, previous, ...args);
    if (control.element.contains(node) && next !== previous)
      trace.control.cardWrites += 1;
    return next;
  };

  trace.observer = new MutationObserver((records) => {
    trace.controlMutations.push(
      ...records.map((record) => ({
        attribute: record.attributeName || "",
        type: record.type,
      })),
    );
  });
  trace.observer.observe(control.element, {
    attributes: true,
    characterData: true,
    childList: true,
    subtree: true,
  });
}

async function laneIsolationApplyActiveUpdate(updateSnapshot) {
  const control = laneStore.laneForId(LANE_ISOLATION_CONTROL_ID);
  const before = laneIsolationSnapshot(control);
  const bounds = (phase) => ({ phase, value: laneIsolationBounds(control) });
  const controlBounds = [bounds("before")];
  applyTeamSnapshotPayload(updateSnapshot, { force: true });
  controlBounds.push(bounds("after-sync"));
  await Promise.resolve();
  controlBounds.push(bounds("after-microtask"));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  controlBounds.push(bounds("after-first-frame"));
  await new Promise((resolve) => requestAnimationFrame(resolve));
  controlBounds.push(bounds("after-second-frame"));
  const trace = window.__laneIsolationTrace;
  trace.controlMutations.push(
    ...trace.observer.takeRecords().map((record) => ({
      attribute: record.attributeName || "",
      type: record.type,
    })),
  );
  trace.observer.disconnect();
  return {
    active: trace.active,
    after: laneIsolationSnapshot(control),
    before,
    control: trace.control,
    controlBounds,
    controlMutations: trace.controlMutations,
  };
}

const laneIsolationPageHelpers = [
  laneIsolationItem,
  laneIsolationRect,
  laneIsolationBounds,
  laneIsolationSnapshot,
  laneIsolationSeed,
  laneIsolationAfterTwoFrames,
  laneIsolationAwaitControlAnimations,
  laneIsolationInstallTrace,
  laneIsolationApplyActiveUpdate,
];

function laneIsolationFixture() {
  const revision = 900000000;
  const controlTeam = teamPayload({
    memberIds: [LANE_ISOLATION_CONTROL_ID],
    revision,
    teamId: "lane-isolation-control-team",
  });
  const activeTeam = teamPayload({
    memberIds: [LANE_ISOLATION_ACTIVE_ID],
    revision,
    teamId: "lane-isolation-active-team",
  });
  return {
    targets: [
      targetPayload({
        id: LANE_ISOLATION_CONTROL_ID,
        teamId: controlTeam.teamId,
        teamRevision: revision,
        threadId: LANE_ISOLATION_CONTROL_ID + "-thread",
      }),
      targetPayload({
        id: LANE_ISOLATION_ACTIVE_ID,
        teamId: activeTeam.teamId,
        teamRevision: revision,
        threadId: LANE_ISOLATION_ACTIVE_ID + "-thread",
      }),
    ],
    initialSnapshot: teamSnapshot({
      revision,
      teams: [controlTeam, activeTeam],
    }),
    updateSnapshot: teamSnapshot(
      {
        revision: revision + 1,
        teams: [
          teamPayload({
            memberIds: [LANE_ISOLATION_ACTIVE_ID],
            revision: revision + 1,
            teamId: activeTeam.teamId,
          }),
        ],
      },
      {
        differential: true,
        snapshot: { teamCount: 2 },
      },
    ),
  };
}

function assertResult(result) {
  if (result.active.groupSyncs !== 1 || result.active.messageRenders !== 1)
    throw new Error(
      "updated lane did not reconcile exactly once: " + JSON.stringify(result),
    );
  const leaked = Object.entries(result.control).filter(([, count]) => count !== 0);
  if (leaked.length)
    throw new Error(
      "inactive lane entered active-lane render paths: " + JSON.stringify(result),
    );
  if (result.controlMutations.length)
    throw new Error(
      "inactive lane DOM mutated during another lane's update: " +
        JSON.stringify(result),
    );
  if (result.before.baseSpan !== LANE_ISOLATION_FULL_SPAN)
    throw new Error(
      "control stream did not degenerate to one serialized column: " +
        JSON.stringify(result),
    );
  const originalBounds = JSON.stringify(result.before.bounds);
  const movedBounds = result.controlBounds.filter(
    (sample) => JSON.stringify(sample.value) !== originalBounds,
  );
  if (movedBounds.length)
    throw new Error(
      "inactive top-card corners or stream extents moved: " +
        JSON.stringify(result),
    );
  if (JSON.stringify(result.before) !== JSON.stringify(result.after))
    throw new Error(
      "inactive lane geometry or render state changed: " + JSON.stringify(result),
    );
}

async function installLaneIsolationHelpers(page) {
  await page.addScriptTag({
    content:
      "const LANE_ISOLATION_CONTROL_ID = " +
      JSON.stringify(LANE_ISOLATION_CONTROL_ID) +
      ";\nconst LANE_ISOLATION_ACTIVE_ID = " +
      JSON.stringify(LANE_ISOLATION_ACTIVE_ID) +
      ";\n" +
      laneIsolationPageHelpers.map((helper) => helper.toString()).join("\n"),
  });
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-lane-update-isolation-" + Date.now(),
      contextOptions: { viewport: { width: 1600, height: 900 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof mosaicApplyCardPosition === "function" &&
          typeof mosaicScheduleRender === "function" &&
          typeof syncFusedLaneChrome === "function",
        null,
        { timeout: 30000 },
      );
      await installLaneIsolationHelpers(page);
      const fixture = laneIsolationFixture();
      await page.evaluate(laneIsolationSeed, fixture);
      await page.evaluate(laneIsolationAwaitControlAnimations);
      await page.evaluate(laneIsolationInstallTrace);
      return page.evaluate(
        laneIsolationApplyActiveUpdate,
        fixture.updateSnapshot,
      );
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      assertResult(result);
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { assertResult, run };
