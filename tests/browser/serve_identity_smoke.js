const { withServePage } = require("./serve_playwright_harness");

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
      actorId: "thread:thread-b",
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
            "changes on REVIEW-20260102T000000000001Z by agent-b via task-review; 2 follow-ups",
          span: true,
        },
      ],
      reviewPressure: {
        count: 1,
        openFollowupCount: 2,
        items: [
          {
            reviewedTask: "REVIEW-20260102T000000000001Z",
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
    "review pressurechanges on REVIEW-20260102T000000000001Z by agent-b via task-review; 2 follow-ups",
  ]) {
    if (!result.infoText.includes(text))
      throw new Error("lane info missing " + text + ": " + result.infoText);
  }
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-identity-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForSelector(".lane", { timeout: 10000 });
      await page.waitForFunction(() => Array.isArray(targets) && targets.length > 0, {
        timeout: 10000,
      });
      const result = await page.evaluate((payload) => {
        let lane = Array.from(laneStates.values()).find((item) => !item.emptyTeam);
        if (!lane && targets.length) {
          addLane(targets[0].id);
          lane = laneStates.get(targets[0].id);
        }
        if (!lane) throw new Error("no lane available for identity smoke");
        renderLaneChrome(lane, {
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
      return { ...result, url: server.url };
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
