const { withServePage } = require("./serve_playwright_harness");
const { targetPayload, teamPayload, teamSnapshot } = require("./payload_factory");

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
        async ({ alpha, beta, snapshot }) => {
          laneStore.replaceTargets([alpha, beta]);
          applyTeamSnapshotPayload(snapshot, { force: true });
          const host = laneStore.lanesSnapshot().find(
            (lane) => !isShadowLane(lane) && laneGroupMemberTargetIds(lane).length === 2,
          );
          if (!host) throw new Error("missing fused team host");
          const betaLane = laneStore.laneForId("beta");
          if (!betaLane) throw new Error("missing beta lane");
          const betaThreadBefore = betaLane.targetThreadId;

          setLaneLifetime(host, "Steer");

          return {
            laneCount: laneStore.lanesSnapshot().length,
            memberIds: laneGroupMemberTargetIds(host),
            hostTeamId: host.teamId,
            hostLifetime: laneEffectiveLifetime(host),
            hostTransientStatus: host.transientStatus || "",
            betaThreadBefore,
            betaThreadAfter: laneStore.laneForId("beta").targetThreadId,
            betaTargetIdentity:
              laneStore.targetForId("beta").targetIdentity.targetId,
            hasTaskDrainUpdater: typeof updateTaskDrainForLane === "function",
            hasTeamConfigUpdater: typeof updateLaneTeamConfigForLane === "function",
          };
        },
        {
          alpha: targetPayload({
            id: "alpha",
            threadId: "alpha-thread",
            teamId: "team-main",
          }),
          beta: targetPayload({
            id: "beta",
            threadId: "beta-thread",
            teamId: "team-main",
          }),
          snapshot: teamSnapshot({
            revision: 1,
            teams: [
              teamPayload({ teamId: "team-main", memberIds: ["alpha", "beta"] }),
            ],
          }),
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
