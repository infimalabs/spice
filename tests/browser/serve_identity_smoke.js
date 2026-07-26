const path = require("path");
const { execFile } = require("child_process");
const { promisify } = require("util");
const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { repoRoot, withServePage } = require("./serve_playwright_harness");
const { threadActorId } = require("./payload_factory");

const execFileAsync = promisify(execFile);

function pendingIdentity(count = 0) {
  return {
    pendingInboxCount: count,
    pendingInboxLabel: String(count),
    pendingInboxKeys: [],
    pendingInboxRevision: "smoke-pending-" + count,
    pendingInboxVersion: 1,
  };
}

function mismatchPayload(targetId) {
  const pending = pendingIdentity(0);
  return {
    targetIdentity: {
      targetId,
      worktreeName: "repo",
      branch: "main",
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "unconfigured" },
      thread: { state: "bound", threadId: "thread-b" },
    },
    serveAgentIdentity: {
      actorId: threadActorId("thread-b"),
      driver: { desired: "codex", actual: "claude", transcriptOwner: "claude" },
      launch: {
        desired: { model: "gpt-5.5", effort: "xhigh" },
        actual: {
          model: "claude-opus",
          effort: "low",
          serviceTier: "",
          source: "agent state",
        },
      },
      renewal: { state: "requested" },
      target: {},
      thread: { state: "bound", threadId: "thread-b" },
    },
    laneInfo: {
      summaryRows: [
        { key: "driver actual", value: "claude" },
        { key: "driver desired", value: "codex" },
        { key: "model actual", value: "claude-opus" },
        { key: "model desired", value: "gpt-5.5" },
        { key: "effort actual", value: "low" },
        { key: "effort desired", value: "xhigh" },
        { key: "thread", value: "thread-b", span: true },
        { key: "session", value: "claude" },
        {
          key: "review pressure",
          value:
            "changes on REVIEW-1jNJvRyn by agent-b via task-review; 2 follow-ups",
          span: true,
        },
      ],
      reviewPressure: {
        count: 1,
        openFollowupCount: 2,
        items: [
          {
            reviewedTask: "REVIEW-1jNJvRyn",
            finding: "changes",
            findingSeverity: "changes",
            reviewer: "agent-b",
            source: "task-review",
            followupCount: 2,
          },
        ],
      },
    },
    taskFilters: [],
    laneFilterVersion: "",
    teamIdentity: { state: "none" },
    ...pending,
    statusLine: { ...pending, agentProcessStatus: "running" },
  };
}

function assertIdentityResult(result) {
  const expected = {
    driverName: "claude -> codex",
    driverModel: "claude-opus -> gpt-5.5",
    driverEffort: "low -> xhigh",
    driverIconName: "claude",
  };
  for (const [key, value] of Object.entries(expected)) {
    if (result[key] !== value)
      throw new Error(key + " mismatch: " + JSON.stringify(result));
  }
  for (const text of [
    "driver: claude -> codex",
    "model: claude-opus -> gpt-5.5",
    "effort: low -> xhigh",
    "thread: thread-b",
    "session: claude",
  ]) {
    if (!result.tooltip.includes(text))
      throw new Error("tooltip missing " + text + ": " + result.tooltip);
  }
  for (const text of [
    "driver actualclaude",
    "driver desiredcodex",
    "model actualclaude-opus",
    "model desiredgpt-5.5",
    "sessionclaude",
    "review pressurechanges on REVIEW-1jNJvRyn by agent-b via task-review; 2 follow-ups",
  ]) {
    if (!result.infoText.includes(text))
      throw new Error("lane info missing " + text + ": " + result.infoText);
  }
}

async function seedTypedAgentTeam(backendDir, targetId) {
  const script = [
    "from pathlib import Path",
    "import sys",
    "from spice.serve.team.store import ServeTeamStore",
    'store = ServeTeamStore(path=Path(sys.argv[1]) / "data" / "spiceteams.sqlite3")',
    'actor_id = "target:" + sys.argv[2]',
    "store.create_team(members=[actor_id])",
    "store.record_agent_identity(",
    "    actor_id=actor_id,",
    "    target_id=sys.argv[2],",
    '    renewal_state="pending",',
    "    renewal_revision=7,",
    ")",
  ].join("\n");
  await execFileAsync(
    path.join(repoRoot, ".venv", "bin", "python"),
    ["-c", script, backendDir, targetId],
    { cwd: repoRoot },
  );
}

async function assertTypedAgentTeamLoads(page, backendDir) {
  await page.waitForSelector(".lane");
  await page.waitForFunction(
    () =>
      typeof laneStore !== "undefined" &&
      typeof liveBusRequest !== "undefined" &&
      typeof applyTeamSnapshotPayload !== "undefined" &&
      laneStore.targetsSnapshot().length > 0,
  );
  const targetId = await page.evaluate(() => laneStore.targetsSnapshot()[0].id);
  await seedTypedAgentTeam(backendDir, targetId);
  const result = await page.evaluate(async (expectedTargetId) => {
    const response = await liveBusRequest("teams.refresh", { query: {} });
    applyTeamSnapshotPayload(response.payload, { force: true });
    const member = response.payload.snapshot.teams
      .flatMap((team) => team.members || [])
      .find((candidate) => candidate.agentId === "target:" + expectedTargetId);
    const lane = laneStore.laneForId(expectedTargetId);
    return {
      lanePresent: Boolean(lane),
      targetId: expectedTargetId,
      actorId: member?.agentFacts?.actorId || "",
      renewalRevision: member?.agentFacts?.renewalRevision,
      updatedAtType: typeof member?.agentFacts?.updatedAt,
    };
  }, targetId);
  if (
    !result.lanePresent ||
    result.actorId !== "target:" + targetId ||
    result.renewalRevision !== 7 ||
    result.updatedAtType !== "number"
  )
    throw new Error(
      "typed agent team did not load its lane: " + JSON.stringify(result),
    );
  return result;
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-identity-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      const typedAgentTeam = await assertTypedAgentTeamLoads(
        page,
        server.backendDir,
      );
      await installIsolatedLaneFixture(page, {
        globals: ["renderLanePayloadPresentation"],
      });
      const result = await page.evaluate((payload) => {
        const lane = resolveIsolatedLane("identity-smoke-team");
        renderLanePayloadPresentation(lane, {
          ...payload,
          targetIdentity: { ...payload.targetIdentity, targetId: lane.targetId },
        });
        const icon = lane.element.querySelector("[data-composer-driver-icon]");
        const infoValues = Array.from(
          lane.element.querySelectorAll(".lane-info-cell"),
        ).map((cell) => cell.textContent);
        return {
          driverName: lane.driverName,
          driverModel: lane.driverModel,
          driverEffort: lane.driverEffort,
          driverIconName: lane.driverIconName,
          tooltip: icon ? icon.getAttribute("title") : "",
          infoText: infoValues.join(" "),
        };
      }, mismatchPayload(""));
      assertIdentityResult(result);
      return { ...result, typedAgentTeam, url: server.url };
    },
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
