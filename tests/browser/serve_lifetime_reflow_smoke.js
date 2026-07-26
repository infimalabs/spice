const { withServePage } = require("./serve_playwright_harness");
const { targetPayload, installScript } = require("./payload_factory");

// Repro + regression for UI-1kBMcKKZ. Root cause: updateTeamConfig woke the
// shared task event file (in every lane's signature), so flipping ONE team's
// lifetime re-pushed EVERY team's lanes -> cross-team reflow. Fix: server records
// updateTeamConfig with wake=False; the client resubscribes only the lanes whose
// team config revision actually advanced. This asserts the routing: flip team A's
// config and confirm team A's lanes resubscribe while team B's do not.

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-lifetime-reflow-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof subscribeLaneToLiveBus === "function" &&
          typeof laneStore !== "undefined",
        { timeout: 10000 },
      );
      await page.addScriptTag({ content: installScript });
      return page.evaluate(
        async ({ alpha, delta, beta }) => {
          const team = (teamId, memberIds, lifetime, configRevision) =>
            window.spicePayloads.teamPayload({
              teamId,
              memberIds,
              lifetime,
              revision: configRevision,
            });
          const apply = (teamA, teamB, revision) =>
            applyTeamSnapshotPayload(
              window.spicePayloads.teamSnapshot({
                revision,
                teams: [teamA, teamB],
              }),
              { force: true },
            );

          applyTargetsPayload({ workTrees: [alpha, delta, beta] });
          apply(
            team("team-a", ["alpha", "delta"], "Drive", 1),
            team("team-b", ["beta"], "Drive", 1),
            1,
          );

          // Mark every lane subscribed so the fix's resubscribe guard is live,
          // and instrument which lanes resubscribe.
          const resubscribes = {};
          for (const lane of laneStore.lanesSnapshot())
            lane.liveBusSubscribed = true;
          const original = subscribeLaneToLiveBus;
          // eslint-disable-next-line no-global-assign
          subscribeLaneToLiveBus = function (lane) {
            resubscribes[lane.targetId] = (resubscribes[lane.targetId] || 0) + 1;
            return undefined; // do not issue a real bus request in the repro
          };

          // Flip ONLY team A's lifetime (config revision 1 -> 2). Team B is
          // byte-identical at config revision 1.
          apply(
            team("team-a", ["alpha", "delta"], "Steer", 2),
            team("team-b", ["beta"], "Drive", 1),
            2,
          );
          await new Promise((r) => setTimeout(r, 50));
          // eslint-disable-next-line no-global-assign
          subscribeLaneToLiveBus = original;

          return {
            alphaResub: resubscribes["alpha"] || 0,
            deltaResub: resubscribes["delta"] || 0,
            betaResub: resubscribes["beta"] || 0,
          };
        },
        {
          alpha: targetPayload({
            id: "alpha",
            threadId: "alpha-thread",
            teamId: "team-a",
          }),
          delta: targetPayload({
            id: "delta",
            threadId: "delta-thread",
            teamId: "team-a",
          }),
          beta: targetPayload({
            id: "beta",
            threadId: "beta-thread",
            teamId: "team-b",
          }),
        },
      );
    },
  );
}

function assertResult(result) {
  if (result.alphaResub < 1 || result.deltaResub < 1)
    throw new Error(
      "acting team A lanes did not resubscribe on config change: " +
        JSON.stringify(result),
    );
  if (result.betaResub !== 0)
    throw new Error(
      "CROSS-TEAM REFLOW: team B resubscribed on team A config change: " +
        JSON.stringify(result),
    );
}

if (require.main === module) {
  run()
    .then((result) => {
      assertResult(result);
      console.log(JSON.stringify(result, null, 2));
      console.log("team A resubscribed; team B untouched");
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run, assertResult };
