const { withServePage } = require("./serve_playwright_harness");

// Team columns weight by agent count: a fused team host carries
// --lane-weight = member count, so in the horizontal swimlanes row a
// two-agent team renders twice a solo team's width and a six-agent team
// renders six times a solo team's width. The viewport is wide enough that
// the 20em .lane min-width floor stays inactive for every measured phase,
// so the flex ratios are the pure observable.
const TEAM_WIDTH_VIEWPORT = { width: 2560, height: 800 };
// Flex distributes outer widths grow-proportionally from a zero basis; the
// only chrome outside that math is each lane's 1px borders, well under
// this relative bound. An unweighted regression measures 1.0 on both
// phases, far outside it.
const TEAM_WIDTH_RATIO_RELATIVE_TOLERANCE = 0.04;

function targetPayload(id, threadId, teamId) {
  return {
    id,
    name: id,
    branch: id,
    targetIdentity: {
      targetId: id,
      worktreeName: id,
      branch: id,
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "unconfigured" },
      thread: { state: "bound", threadId },
    },
    serveAgentIdentity: {
      actorId: "thread:" + threadId,
      target: { id },
      thread: { state: "bound", threadId },
    },
    teamIdentity: {
      state: "member",
      teamId,
      teamRevision: 1,
      configRevision: 1,
    },
    taskFilters: [],
    laneFilterVersion: "",
    lifetime: "Drive",
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "pending-" + id,
    pendingInboxVersion: 1,
    statusLine: {
      pendingInboxCount: 0,
      pendingInboxKeys: [],
      pendingInboxRevision: "pending-" + id,
      pendingInboxVersion: 1,
    },
  };
}

function team(teamId, memberIds, revision) {
  return {
    teamId,
    revision,
    config: {
      revision,
      lifetime: "Drive",
      taskFilters: [],
      taskFilterEntries: [],
    },
    splitBack: {},
    members: memberIds.map((agentId) => ({ agentId: "target:" + agentId })),
  };
}

function teamsSnapshotPayload(revision, teams) {
  return {
    revision,
    changed: true,
    snapshot: {
      globalSettings: { fastMode: false },
      teams,
    },
  };
}

// Applies one multi-team snapshot and measures the visible (non-shadowed)
// lane geometry: the weighted host width, the solo width, their ratio, and
// the row invariants (no overlap past the container, min-width floor held).
async function measureTeamWidths(page, phase) {
  return page.evaluate(
    ({ allTargets, payload, weightedMemberIds, soloId }) => {
      const memberAgentIds = new Set(
        payload.snapshot.teams
          .flatMap((entry) => entry.members)
          .map((member) => member.agentId),
      );
      targets = allTargets.filter((target) =>
        memberAgentIds.has("target:" + target.id),
      );
      targetById = new Map(targets.map((target) => [target.id, target]));
      applyTeamSnapshotPayload(payload, { force: true });

      const host = laneGroupHost(laneStates.get(weightedMemberIds[0]));
      const solo = laneStates.get(soloId);
      if (laneGroupMemberTargetIds(host).length < weightedMemberIds.length)
        throw new Error("weighted team did not fuse into one host lane");

      const visible = [...laneStates.values()].filter(
        (lane) =>
          isLaneOpen(lane) &&
          lane.element.classList.contains("lane--shadowed") === false,
      );
      const swimlanesEl = document.querySelector(".swimlanes");
      const hostWidth = host.element.getBoundingClientRect().width;
      const soloWidth = solo.element.getBoundingClientRect().width;
      const minWidthPx = Number.parseFloat(
        getComputedStyle(solo.element).minWidth,
      );
      return {
        visibleLaneCount: visible.length,
        hostWidth,
        soloWidth,
        ratio: hostWidth / soloWidth,
        minWidthPx,
        rowScrollWidth: swimlanesEl.scrollWidth,
        rowClientWidth: swimlanesEl.clientWidth,
      };
    },
    phase,
  );
}

function teamWidthFixture() {
  const pairIds = ["a1", "a2"];
  const packIds = ["a1", "a2", "a3", "a4", "a5", "a6"];
  const allTargets = [...packIds, "b1"].map((id) =>
    targetPayload(id, id + "-thread", id === "b1" ? "team-solo" : "team-pack"),
  );
  return {
    pair: {
      allTargets,
      payload: teamsSnapshotPayload(1, [
        team("team-pack", pairIds, 1),
        team("team-solo", ["b1"], 1),
      ]),
      weightedMemberIds: pairIds,
      soloId: "b1",
    },
    pack: {
      allTargets,
      payload: teamsSnapshotPayload(2, [
        team("team-pack", packIds, 2),
        team("team-solo", ["b1"], 2),
      ]),
      weightedMemberIds: packIds,
      soloId: "b1",
    },
  };
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-team-width-" + Date.now(),
      contextOptions: { viewport: TEAM_WIDTH_VIEWPORT },
    },
    async ({ page }) => {
      await page.waitForFunction(
        // Braced body: lizard (the complexity gate) misparses a braceless
        // arrow followed by an object-literal argument.
        () => {
          return typeof applyTeamSnapshotPayload === "function";
        },
        { timeout: 10000 },
      );
      const fixture = teamWidthFixture();
      const pair = await measureTeamWidths(page, fixture.pair);
      const pack = await measureTeamWidths(page, fixture.pack);
      return { pair, pack };
    },
  );
}

function assertPhase(label, phase, expectedRatio) {
  if (phase.visibleLaneCount !== 2)
    throw new Error(
      label + " did not render exactly two team columns: " + JSON.stringify(phase),
    );
  const bound = expectedRatio * TEAM_WIDTH_RATIO_RELATIVE_TOLERANCE;
  if (Math.abs(phase.ratio - expectedRatio) > bound)
    throw new Error(
      label +
        " team width ratio missed " +
        expectedRatio +
        ":1: " +
        JSON.stringify(phase),
    );
  if (phase.soloWidth < phase.minWidthPx)
    throw new Error(
      label + " solo column fell under the usable min width: " + JSON.stringify(phase),
    );
  if (phase.rowScrollWidth > phase.rowClientWidth)
    throw new Error(
      label + " row overflowed its container at this viewport: " + JSON.stringify(phase),
    );
}

function assertResult(result) {
  assertPhase("pair (2:1)", result.pair, 2);
  assertPhase("pack (6:1)", result.pack, 6);
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

module.exports = { run, assertResult };
