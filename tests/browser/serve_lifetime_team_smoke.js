const { withServePage } = require("./serve_playwright_harness");

function targetPayload(id, threadId) {
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
      teamId: "team-main",
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

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-lifetime-team-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page }) => {
      await page.waitForFunction(() => typeof applyTeamSnapshotPayload === "function", {
        timeout: 10000,
      });
      return await page.evaluate(
        async ({ alpha, beta }) => {
          const team = (lifetime, revision) => ({
            teamId: "team-main",
            revision,
            config: {
              revision,
              lifetime,
              speechMode: "speak",
              selectedView: "compose",
              taskFilters: [],
              taskFilterEntries: [],
            },
            splitBack: {},
            members: [{ agentId: "target:alpha" }, { agentId: "target:beta" }],
          });
          targets = [alpha, beta];
          targetById = new Map(targets.map((target) => [target.id, target]));
          applyTeamSnapshotPayload(
            {
              revision: 1,
              changed: true,
              snapshot: {
                globalSettings: { fastMode: false },
                teams: [team("Drive", 1)],
              },
            },
            { force: true },
          );
          const host = Array.from(laneStates.values()).find(
            (lane) => !isShadowLane(lane) && laneGroupMemberTargetIds(lane).length === 2,
          );
          if (!host) throw new Error("missing fused team host");
          const betaLane = laneStates.get("beta");
          if (!betaLane) throw new Error("missing beta lane");
          const betaThreadBefore = betaLane.targetThreadId;

          setLaneLifetime(host, "Steer");

          return {
            laneCount: laneStates.size,
            memberIds: laneGroupMemberTargetIds(host),
            hostTeamId: host.teamId,
            hostLifetime: laneEffectiveLifetime(host),
            hostTransientStatus: host.transientStatus || "",
            betaThreadBefore,
            betaThreadAfter: laneStates.get("beta").targetThreadId,
            betaTargetIdentity: targetById.get("beta").targetIdentity.targetId,
            hasTaskDrainUpdater: typeof updateTaskDrainForLane === "function",
            hasTeamConfigUpdater: typeof updateLaneTeamConfigForLane === "function",
          };
        },
        {
          alpha: targetPayload("alpha", "alpha-thread"),
          beta: targetPayload("beta", "beta-thread"),
        },
      );
    },
  );
}

function assertResult(result) {
  if (result.hasTaskDrainUpdater)
    throw new Error("old grouped task-drain updater is still exposed");
  if (!result.hasTeamConfigUpdater)
    throw new Error("team config updater is missing");
  if (result.laneCount !== 2)
    throw new Error("team members disappeared: " + JSON.stringify(result));
  if (result.memberIds.join(",") !== "alpha,beta")
    throw new Error("wrong team members: " + JSON.stringify(result));
  if (result.hostLifetime !== "Steer")
    throw new Error("host lifetime did not update: " + JSON.stringify(result));
  if (result.betaThreadAfter !== result.betaThreadBefore)
    throw new Error("beta thread identity changed: " + JSON.stringify(result));
  if (result.betaTargetIdentity !== "beta")
    throw new Error("beta target identity was overwritten: " + JSON.stringify(result));
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
