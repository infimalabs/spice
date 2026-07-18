const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const maxStatusTransitionMs = 500;

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
          "lanePrimaryComposePlaceholder",
          "laneGroupHost",
          "laneGroupMemberLanes",
          "syncComposerShards",
          "targetChoiceStatus",
          "updateLiveRelativeTimes",
        ],
      });
      const result = await page.evaluate(runStructuralStatusSmokePage);
      assertStructuralStatusResult(result);
      return { ...result, url: server.url };
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

  function statusPayload({ claimedTask = {}, index, kind, preview, timestamp }) {
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
        activityStatus: kind === "final" ? "inactive" : "active",
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
      pipStatus: lane.pipEl.dataset.agentStatus || "",
      placeholder: textarea.placeholder,
      compactPlaceholder: laneComposePlaceholder(lane),
      memberPlaceholder: lanePrimaryComposePlaceholder(lane, {
        ...lane,
        targetId: "structural-status-member",
      }),
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
      timestamp: new Date().toISOString(),
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
      timestamp: new Date().toISOString(),
    }),
  );
  const final = await applyWatchPayload(
    statusPayload({
      index: 3,
      kind: "final",
      preview: "Confirmed fixed.",
      timestamp: new Date(Date.now() - 20 * 60 * 1000).toISOString(),
    }),
  );
  updateLiveRelativeTimes();
  const afterRelativeTick = {
    latestActivityKind: lane.lastRenderedStatusLine.latestActivityKind || "",
    pipStatus: lane.pipEl.dataset.agentStatus || "",
    placeholder: textarea.placeholder,
    statusAge: lane.statusTimeEl.textContent || "",
    visualStatus: lane.lastRenderedStatusLine.agentVisualStatus || "",
  };
  return { active, afterRelativeTick, final, phaseTransition };
}

function assertStructuralStatusResult(result) {
  const { active, afterRelativeTick, final, phaseTransition } = result;
  for (const [label, snapshot] of [
    ["active", active],
    ["phaseTransition", phaseTransition],
    ["final", final],
  ]) {
    if (snapshot.elapsedMs > maxStatusTransitionMs)
      throw new Error(label + " status transition exceeded bound: " + snapshot.elapsedMs);
  }
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
  if (!active.placeholder.includes("0 pending, running"))
    throw new Error("active composer status is stale: " + active.placeholder);
  if (
    active.placeholder !==
    "spice-b, main-b · 0 pending, running\n" +
      "UI-1kF5xdSM, todo\n" +
      "Next: Show the claimed task even when its deliberatel…"
  )
    throw new Error("main composer task context is incomplete: " + active.placeholder);
  if (
    active.compactPlaceholder !==
      "spice-b, main-b\n0 pending, running\nUI-1kF5xdSM, todo" ||
    active.memberPlaceholder !== active.compactPlaceholder
  )
    throw new Error(
      "compact member/quote task context changed: " + JSON.stringify(active),
    );
  if (
    active.composerTitle !==
    "UI-1kF5xdSM, todo\n" +
      "Show the claimed task even when its deliberately long title must wrap inside the agent card"
  )
    throw new Error("claimed task hover detail is incomplete: " + active.composerTitle);
  if (
    phaseTransition.placeholder !==
      "spice-b, main-b · 0 pending, running\n" +
        "UI-1kF5xdSM, review\n" +
        "Next: Show the claimed task even when its deliberatel…" ||
    phaseTransition.composerTitle !==
      "UI-1kF5xdSM, review\n" +
        "Show the claimed task even when its deliberately long title must wrap inside the agent card"
  )
    throw new Error(
      "claimed task phase transition is stale: " + JSON.stringify(phaseTransition),
    );
  if (
    active.placeholderOverflowWrap !== "anywhere" ||
    !active.placeholderWithinCard
  )
    throw new Error("long claimed task breaks its composer card: " + JSON.stringify(active));

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

  if (
    afterRelativeTick.latestActivityKind !== "final" ||
    afterRelativeTick.visualStatus !== "idle" ||
    afterRelativeTick.pipStatus !== "idle" ||
    afterRelativeTick.statusAge !== "20m" ||
    !afterRelativeTick.placeholder.includes("0 pending, idle")
  )
    throw new Error(
      "relative-time tick changed structural status: " +
        JSON.stringify(afterRelativeTick),
    );
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
