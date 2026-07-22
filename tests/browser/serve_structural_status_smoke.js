const os = require("os");
const path = require("path");
const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const screenshotPath = path.join(os.tmpdir(), "spice-structural-status.png");
const maxStatusTransitionMs = 500;
const pipRampStates = ["active", "active-ish", "inactive", "unknown"];
const pipHueToleranceDeg = 8;
const pipFullSaturationTolerance = 0.01;
const groupedPipStates = ["active", "active-ish", "inactive", "unknown"];

function parseRgb(value) {
  const channels = (value.match(/[\d.]+/g) || []).map(Number).slice(0, 3);
  if (channels.length < 3) throw new Error("unparseable pip color: " + value);
  const scale = /color\(|srgb/i.test(value) ? 255 : 1;
  return channels.map((channel) => channel * scale);
}

function rgbToHsl(value) {
  const [r, g, b] = parseRgb(value).map((channel) => channel / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;
  let hue = 0;
  if (delta !== 0) {
    if (max === r) hue = ((g - b) / delta) % 6;
    else if (max === g) hue = (b - r) / delta + 2;
    else hue = (r - g) / delta + 4;
    hue = (hue * 60 + 360) % 360;
  }
  const lightness = (max + min) / 2;
  const saturation =
    delta === 0 ? 0 : delta / (1 - Math.abs(2 * lightness - 1));
  return { hue, saturation, lightness };
}

function assertPipSaturationRamp(colors) {
  const ramp = Object.fromEntries(
    pipRampStates.map((state) => [state, rgbToHsl(colors[state])]),
  );
  for (const state of pipRampStates.slice(0, -1)) {
    if (Math.abs(ramp[state].hue - ramp.active.hue) > pipHueToleranceDeg)
      throw new Error(
        "pip activity shifted hue instead of desaturating: " +
          JSON.stringify(ramp),
      );
  }
  for (let index = 1; index < pipRampStates.length; index += 1) {
    const previous = ramp[pipRampStates[index - 1]].saturation;
    const current = ramp[pipRampStates[index]].saturation;
    if (!(previous > current))
      throw new Error(
        "pip saturation did not fall active>active-ish>inactive>unknown: " +
          JSON.stringify(ramp),
      );
  }
  return ramp;
}

function assertGroupedPipRamp(groupedPips, colors) {
  if (groupedPips.length !== groupedPipStates.length)
    throw new Error(
      "grouped lane did not render four status pips: " + JSON.stringify(groupedPips),
    );
  for (let index = 0; index < groupedPipStates.length; index += 1) {
    const pip = groupedPips[index];
    const expectedActivity = groupedPipStates[index];
    if (
      pip.activity !== expectedActivity ||
      pip.color !== colors[expectedActivity] ||
      pip.width <= 0 ||
      pip.height <= 0 ||
      pip.visibility !== "visible"
    )
      throw new Error(
        "grouped status pip diverged from activity ramp: " +
          JSON.stringify({ groupedPips, colors }),
      );
  }
}

function assertPipActivityBase(colors, activityBase) {
  if (colors.active !== activityBase)
    throw new Error(
      "active pip did not use the full-saturation theme base: " +
        JSON.stringify({ activityBase, colors }),
    );
  const active = rgbToHsl(colors.active);
  if (Math.abs(active.saturation - 1) > pipFullSaturationTolerance)
    throw new Error(
      "active pip was not 100% saturated: " +
        JSON.stringify({ active, activityBase, colors }),
    );
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-structural-status-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: [
          "handleLiveBusMessage",
          "laneComposePlaceholder",
          "laneGroupHost",
          "laneGroupMemberLanes",
          "reconcileLaneGroups",
          "syncFusedLaneChrome",
          "syncFusedLaneLights",
          "syncComposerShards",
          "targetChoiceStatus",
          "updateLiveRelativeTimes",
        ],
      });
      const result = await page.evaluate(runStructuralStatusSmokePage);
      assertStructuralStatusResult(result);
      await page.screenshot({ path: screenshotPath });
      return { ...result, screenshotPath, url: server.url };
    },
  );
}

async function runStructuralStatusSmokePage() {
  const lane = resolveIsolatedLane("structural-status-smoke-team");
  lane.agentName = "spice-b";
  lane.branchName = "main-b";
  activateIsolatedLaneWatch(lane, "structural-status-smoke");
  const host = laneGroupHost(lane);
  syncComposerShards(host, laneGroupMemberLanes(host));
  const textarea =
    lane.shardTextareas.get(lane.targetId) || lane.element.querySelector("textarea");
  if (!textarea) throw new Error("structural status smoke has no composer textarea");

  function pendingIdentity(version) {
    return {
      pendingInboxCount: 0,
      pendingInboxKeys: [],
      pendingInboxRevision: "structural-status-" + version,
      pendingInboxVersion: version,
    };
  }

  function statusPayload({
    activityStatus = "active",
    claimedTask = {},
    index,
    kind,
    preview,
    timestamp,
  }) {
    const pending = pendingIdentity(index);
    const visualStatus = kind === "final" ? "idle" : "running";
    return {
      ...pending,
      messages: [
        {
          ack_count: 0,
          ack_keys: [],
          display_html: "<p>" + preview + "</p>",
          display_text: preview,
          index,
          key: "structural-status-" + index,
          kind,
          text: preview,
          threadId: "structural-status-thread",
          timestamp,
        },
      ],
      statusLine: {
        ...pending,
        activityStatus,
        agentProcessStatus: "running",
        agentVisualStatus: visualStatus,
        claimedTask,
        lastAssistantAt: timestamp,
        latestActivityKind: kind,
        latestActivityPreview: preview,
        preview,
      },
    };
  }

  async function applyWatchPayload(payload) {
    const startedAt = performance.now();
    await handleLiveBusMessage(
      JSON.stringify({
        payload,
        source: "watch",
        subscriptionGeneration: lane.liveBusSubscriptionGeneration,
        targetId: lane.targetId,
        type: "lane.payload",
      }),
    );
    const elapsedMs = performance.now() - startedAt;
    const targetStatus = targetChoiceStatus({
      id: lane.targetId,
      statusLine: payload.statusLine,
      targetIdentity: {
        thread: { state: "bound", threadId: "structural-status-thread" },
      },
    });
    const header = lane.element.querySelector(".composer-latest-time");
    const ageRect = lane.statusTimeEl.getBoundingClientRect();
    const previewRect = lane.statusPreviewEl.getBoundingClientRect();
    const textareaRect = textarea.getBoundingClientRect();
    const shardRect = textarea.closest(".composer-shard").getBoundingClientRect();
    return {
      elapsedMs,
      headerStatus: header ? header.dataset.agentStatus || "" : "",
      latestActivityKind: lane.lastRenderedStatusLine.latestActivityKind || "",
      pipActivity: lane.pipEl.dataset.agentActivity || "",
      pipStatus: lane.pipEl.dataset.agentStatus || "",
      placeholder: textarea.placeholder,
      compactPlaceholder: laneComposePlaceholder(lane),
      composerTitle: textarea.title,
      placeholderOverflowWrap: getComputedStyle(textarea).overflowWrap,
      placeholderWithinCard:
        textareaRect.left >= shardRect.left && textareaRect.right <= shardRect.right,
      statusAge: lane.statusTimeEl.textContent || "",
      statusGapPx: previewRect.left - ageRect.right,
      statusPreview: lane.statusPreviewEl.textContent || "",
      targetStatus,
      visualStatus: lane.lastRenderedStatusLine.agentVisualStatus || "",
    };
  }

  const active = await applyWatchPayload(
    statusPayload({
      claimedTask:
        {
          handle: "UI-1kF5xdSM",
          phase: "todo",
          title:
            "Show the claimed task even when its deliberately long title must wrap inside the agent card",
        },
      index: 1,
      kind: "assistant",
      preview: "Working through the event path.",
      timestamp: new Date(Date.now() - 46 * 1000).toISOString(),
    }),
  );
  const phaseTransition = await applyWatchPayload(
    statusPayload({
      claimedTask: {
        handle: "UI-1kF5xdSM",
        phase: "review",
        title:
          "Show the claimed task even when its deliberately long title must wrap inside the agent card",
      },
      index: 2,
      kind: "assistant",
      preview: "Reviewing the integrated change.",
      timestamp: new Date(Date.now() - 13 * 1000).toISOString(),
    }),
  );
  const activeish = await applyWatchPayload(
    statusPayload({
      activityStatus: "active-ish",
      index: 3,
      kind: "final",
      preview: "Recently finished.",
      timestamp: new Date(Date.now() - 3 * 60 * 1000).toISOString(),
    }),
  );
  const final = await applyWatchPayload(
    statusPayload({
      activityStatus: "inactive",
      index: 4,
      kind: "final",
      preview: "Confirmed fixed.",
      timestamp: new Date(Date.now() - 20 * 60 * 1000).toISOString(),
    }),
  );
  updateLiveRelativeTimes();
  const pipColors = {};
  const renderedPipActivity = lane.pipEl.dataset.agentActivity;
  const renderedPipActivities = [
    "active",
    "active-ish",
    "inactive",
    "unknown",
  ];
  for (const state of renderedPipActivities) {
    lane.pipEl.dataset.agentActivity = state;
    pipColors[state] = getComputedStyle(lane.pipEl).backgroundColor;
  }
  lane.pipEl.dataset.agentActivity = renderedPipActivity;
  const afterRelativeTick = {
    latestActivityKind: lane.lastRenderedStatusLine.latestActivityKind || "",
    pipActivity: lane.pipEl.dataset.agentActivity || "",
    pipStatus: lane.pipEl.dataset.agentStatus || "",
    placeholder: textarea.placeholder,
    statusAge: lane.statusTimeEl.textContent || "",
    visualStatus: lane.lastRenderedStatusLine.agentVisualStatus || "",
  };
  const toolActivity = await applyWatchPayload(
    statusPayload({
      index: 5,
      kind: "presence:function_call",
      preview: "Bash: pytest",
      timestamp: new Date().toISOString(),
    }),
  );
  const activityProbe = document.createElement("span");
  activityProbe.style.color = "hsl(from var(--good) h 100% l)";
  lane.element.append(activityProbe);
  const activityBase = getComputedStyle(activityProbe).color;
  activityProbe.remove();
  const groupedMembers = [
    lane,
    ...renderedPipActivities.slice(1).map((_, index) =>
      resolveIsolatedLane("structural-status-member-" + index),
    ),
  ];
  for (let index = 0; index < groupedMembers.length; index += 1) {
    groupedMembers[index].element.classList.remove("lane--empty-team");
    groupedMembers[index].pipEl.dataset.agentStatus = "running";
    groupedMembers[index].pipEl.dataset.agentActivity =
      renderedPipActivities[index];
  }
  reconcileLaneGroups([groupedMembers.map((member) => member.targetId)]);
  const groupedHost = laneGroupHost(lane);
  syncFusedLaneChrome(groupedHost);
  const groupedPips = Array.from(
    groupedHost.laneLightsEl.querySelectorAll(".lane-light"),
  ).map((pip) => {
    const bounds = pip.getBoundingClientRect();
    const style = getComputedStyle(pip);
    return {
      activity: pip.dataset.agentActivity || "",
      color: style.backgroundColor,
      height: bounds.height,
      status: pip.dataset.agentStatus || "",
      visibility: style.visibility,
      width: bounds.width,
    };
  });
  const liveMembers = groupedMembers.slice(1);
  const liveStatuses = [
    {
      activityStatus: "active",
      agentProcessStatus: "running",
      agentVisualStatus: "running",
      lastAssistantAt: new Date().toISOString(),
      latestActivityKind: "presence:function_call",
      latestActivityPreview: "Bash: pytest",
    },
    {
      activityStatus: "unknown",
      agentProcessStatus: "starting",
      agentVisualStatus: "starting",
      lastAssistantAt: "",
      latestActivityKind: "",
      latestActivityPreview: "",
    },
    {
      activityStatus: "unknown",
      agentProcessStatus: "running",
      agentVisualStatus: "running",
      lastAssistantAt: "",
      latestActivityKind: "",
      latestActivityPreview: "",
    },
  ];
  for (let index = 0; index < liveMembers.length; index += 1) {
    liveMembers[index].latestPayload = { statusLine: liveStatuses[index] };
  }
  updateLiveRelativeTimes();
  const liveGroupedPips = liveMembers.map((member) => {
    const pip = groupedHost.laneLightsEl.querySelector(
      '[data-lane-light-target-id="' + member.targetId + '"]',
    );
    return {
      activity: pip.dataset.agentActivity || "",
      color: getComputedStyle(pip).backgroundColor,
      status: pip.dataset.agentStatus || "",
    };
  });
  const startingHeaderProbe = document.createElement("span");
  startingHeaderProbe.className = "composer-quote-time";
  startingHeaderProbe.dataset.agentStatus = "starting";
  lane.element.append(startingHeaderProbe);
  const startingHeaderColor = getComputedStyle(startingHeaderProbe).color;
  startingHeaderProbe.remove();
  return {
    active,
    activeish,
    activityBase,
    afterRelativeTick,
    final,
    groupedPips,
    liveGroupedPips,
    phaseTransition,
    pipColors,
    startingHeaderColor,
    toolActivity,
  };
}

function assertPipStatusResult(result) {
  assertPipSaturationRamp(result.pipColors);
  assertPipActivityBase(result.pipColors, result.activityBase);
  assertGroupedPipRamp(result.groupedPips, result.pipColors);
  const expectedLiveActivities = ["active", "active-ish", "active"];
  for (let index = 0; index < expectedLiveActivities.length; index += 1) {
    const expected = expectedLiveActivities[index];
    const pip = result.liveGroupedPips[index];
    if (pip.activity !== expected || pip.color !== result.pipColors[expected])
      throw new Error(
        "live grouped pip did not follow current member state: " +
          JSON.stringify({ expected, liveGroupedPips: result.liveGroupedPips }),
      );
  }
  if (result.startingHeaderColor !== result.pipColors["active-ish"])
    throw new Error(
      "starting header did not use the green activity rung: " +
        JSON.stringify({
          activeish: result.pipColors["active-ish"],
          startingHeaderColor: result.startingHeaderColor,
        }),
    );
}

function assertStatusTransitionTimes(active, phaseTransition, final) {
  for (const [label, snapshot] of [
    ["active", active],
    ["phaseTransition", phaseTransition],
    ["final", final],
  ]) {
    if (snapshot.elapsedMs > maxStatusTransitionMs)
      throw new Error(label + " status transition exceeded bound: " + snapshot.elapsedMs);
  }
}

function assertActiveStatusResult(active) {
  if (active.latestActivityKind !== "assistant")
    throw new Error("active event did not retain structural activity kind");
  for (const value of [
    active.visualStatus,
    active.pipStatus,
    active.headerStatus,
    active.targetStatus,
  ]) {
    if (value !== "running")
      throw new Error("active event did not render running: " + JSON.stringify(active));
  }
  if (active.pipActivity !== "active")
    throw new Error("active output did not refresh the pip: " + JSON.stringify(active));
  if (!active.placeholder.includes("0 pending, running"))
    throw new Error("active composer status is stale: " + active.placeholder);
  if (
    active.placeholder !==
    "spice-b, main-b\n0 pending, running\nUI-1kF5xdSM, todo"
  )
    throw new Error("main composer task context is incomplete: " + active.placeholder);
  if (active.compactPlaceholder !== active.placeholder)
    throw new Error(
      "compact quote task context diverged from the primary placeholder: " +
        JSON.stringify(active),
    );
  if (
    active.composerTitle !==
    "spice-b, main-b\n0 pending, running\nUI-1kF5xdSM, todo\n" +
      "Show the claimed task even when its deliberately long title must wrap inside the agent card"
  )
    throw new Error("claimed task hover detail is incomplete: " + active.composerTitle);
  if (
    active.placeholderOverflowWrap !== "anywhere" ||
    !active.placeholderWithinCard
  )
    throw new Error("long claimed task breaks its composer card: " + JSON.stringify(active));
}

function assertPhaseTransitionResult(phaseTransition) {
  if (
    phaseTransition.placeholder !==
      "spice-b, main-b\n0 pending, running\nUI-1kF5xdSM, review" ||
    phaseTransition.composerTitle !==
      "spice-b, main-b\n0 pending, running\nUI-1kF5xdSM, review\n" +
        "Show the claimed task even when its deliberately long title must wrap inside the agent card"
  )
    throw new Error(
      "claimed task phase transition is stale: " + JSON.stringify(phaseTransition),
    );
}

function assertFinalStatusResult(final) {
  if (final.latestActivityKind !== "final")
    throw new Error("final event did not retain structural activity kind");
  for (const value of [
    final.visualStatus,
    final.pipStatus,
    final.headerStatus,
    final.targetStatus,
  ]) {
    if (value !== "idle")
      throw new Error("final event did not render idle: " + JSON.stringify(final));
  }
  if (!final.placeholder.includes("0 pending, idle"))
    throw new Error("final composer status is stale: " + final.placeholder);
  if (final.placeholder !== "spice-b, main-b\n0 pending, idle")
    throw new Error("unclaimed composer placeholder mismatch: " + final.placeholder);
  if (final.composerTitle)
    throw new Error("unclaimed composer retained task hover text: " + final.composerTitle);
  if (final.statusAge !== "20m" || final.statusPreview !== "Confirmed fixed.")
    throw new Error("final lane text mismatch: " + JSON.stringify(final));
  if (final.statusGapPx < 0)
    throw new Error("final lane age and preview are not ordered: " + final.statusGapPx);
  if (final.pipActivity !== "inactive")
    throw new Error("old final output did not age the pip: " + JSON.stringify(final));
}

function assertActiveishStatusResult(activeish) {
  if (
    activeish.latestActivityKind !== "final" ||
    activeish.visualStatus !== "idle" ||
    activeish.pipStatus !== "idle" ||
    activeish.pipActivity !== "active-ish" ||
    String(activeish.statusAge).trim() !== "3m"
  )
    throw new Error(
      "recent final output did not retain the active-ish pip rung: " +
        JSON.stringify(activeish),
    );
}

function assertRelativeTickResult(afterRelativeTick) {
  if (
    afterRelativeTick.latestActivityKind !== "final" ||
    afterRelativeTick.visualStatus !== "idle" ||
    afterRelativeTick.pipActivity !== "inactive" ||
    afterRelativeTick.pipStatus !== "idle" ||
    afterRelativeTick.statusAge !== "20m" ||
    !afterRelativeTick.placeholder.includes("0 pending, idle")
  )
    throw new Error(
      "relative-time tick changed structural status: " +
        JSON.stringify(afterRelativeTick),
    );
}

function assertToolActivityResult(toolActivity) {
  if (
    toolActivity.latestActivityKind !== "presence:function_call" ||
    toolActivity.pipActivity !== "active" ||
    toolActivity.visualStatus !== "running"
  )
    throw new Error(
      "tool activity without commentary did not refresh the pip: " +
        JSON.stringify(toolActivity),
    );
}

function assertStructuralStatusResult(result) {
  const {
    active,
    activeish,
    afterRelativeTick,
    final,
    phaseTransition,
    toolActivity,
  } = result;
  assertPipStatusResult(result);
  assertStatusTransitionTimes(active, phaseTransition, final);
  assertActiveStatusResult(active);
  assertPhaseTransitionResult(phaseTransition);
  assertActiveishStatusResult(activeish);
  assertFinalStatusResult(final);
  assertRelativeTickResult(afterRelativeTick);
  assertToolActivityResult(toolActivity);
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
