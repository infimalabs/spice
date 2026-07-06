const { withServePage } = require("./serve_playwright_harness");

// Repro for UI-1kBMcKKZ: flipping ONE team's lifetime must not reflow ANOTHER
// team's already-painted cards. Two teams (A: alpha+delta, B: beta). Seed team B
// with cards, snapshot their positions, flip team A's lifetime Drive->Steer, and
// re-measure team B. Any movement of team B's cards is the cross-team reflow.

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
    teamIdentity: { state: "member", teamId, teamRevision: 1, configRevision: 1 },
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
      path: "/?smoke=serve-lifetime-reflow-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof renderMessagesIfChanged === "function" &&
          typeof laneStates !== "undefined",
        { timeout: 10000 },
      );
      return page.evaluate(
        async ({ alpha, delta, beta }) => {
          const team = (teamId, memberIds, lifetime, revision) => ({
            teamId,
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
            members: memberIds.map((id) => ({ agentId: "target:" + id })),
          });
          const snapshot = (teamA, teamB, revision) => ({
            revision,
            changed: true,
            snapshot: {
              globalSettings: { fastMode: false },
              teams: [teamA, teamB],
            },
          });

          targets = [alpha, delta, beta];
          targetById = new Map(targets.map((t) => [t.id, t]));
          applyTeamSnapshotPayload(
            snapshot(
              team("team-a", ["alpha", "delta"], "Drive", 1),
              team("team-b", ["beta"], "Drive", 1),
              1,
            ),
            { force: true },
          );

          const betaLane = laneStates.get("beta");
          if (!betaLane) throw new Error("missing beta lane");
          betaLane.knownMessages = Array.from({ length: 6 }, (_, index) => ({
            ack_count: 0,
            ack_keys: [],
            index,
            key: "reflow-" + index,
            kind: "assistant",
            timestamp: new Date(2027, 0, 1, 0, index, index).toISOString(),
            display_html: Array.from(
              { length: 1 + (index % 4) },
              (_, i) => "line " + i,
            ).join("<br>"),
            display_text: "x",
            text: "x",
          }));
          renderMessagesIfChanged(betaLane);
          await new Promise((r) => setTimeout(r, 300));

          const measure = () => {
            const root =
              betaLane.messagesEl || document.querySelector('[data-lane="beta"]');
            const cards = root
              ? Array.from(root.querySelectorAll("[data-mosaic-card]"))
              : [];
            return cards.map((el) => {
              const r = el.getBoundingClientRect();
              return { key: el.dataset.mosaicCard, top: r.top, left: r.left };
            });
          };

          const before = measure();
          // Flip ONLY team A's lifetime; team B is byte-identical.
          applyTeamSnapshotPayload(
            snapshot(
              team("team-a", ["alpha", "delta"], "Steer", 2),
              team("team-b", ["beta"], "Drive", 1),
              2,
            ),
            { force: true },
          );
          await new Promise((r) => setTimeout(r, 300));
          const after = measure();

          const moved = before.filter((b, i) => {
            const a = after[i];
            return !a || Math.abs(a.top - b.top) > 0.5 || Math.abs(a.left - b.left) > 0.5;
          });
          return {
            betaCardCount: before.length,
            movedCount: moved.length,
            moved,
            before,
            after,
          };
        },
        {
          alpha: targetPayload("alpha", "alpha-thread", "team-a"),
          delta: targetPayload("delta", "delta-thread", "team-a"),
          beta: targetPayload("beta", "beta-thread", "team-b"),
        },
      );
    },
  );
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log(JSON.stringify(result, null, 2));
      if (result.betaCardCount === 0)
        throw new Error("no team-B cards rendered; fixture did not seed");
      if (result.movedCount > 0)
        throw new Error("REFLOW CONFIRMED: team-B cards moved on team-A lifetime flip");
      console.log("no cross-team reflow observed");
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
