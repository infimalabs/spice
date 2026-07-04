const { execFile } = require("child_process");
const { promisify } = require("util");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const execFileAsync = promisify(execFile);
const liveTaskCardSmokeOrigin = "ack:20260101T000000000000Z";
// Later bound scratch targets avoid unrelated historical image fixtures.
const liveTaskCardTargetOffset = 2;

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-task-card-live-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForSelector(".lane", { timeout: 10000 });
      await page.waitForFunction(
        () =>
          Array.isArray(targets) &&
          targets.length > 0 &&
          typeof addLane === "function" &&
          typeof laneStates !== "undefined",
        { timeout: 10000 },
      );
      const lane = await ensureLiveTaskCardLane(page);
      await waitForLiveTaskCardSubscription(page, lane.targetId);
      const title = "Live task card smoke " + Date.now();
      let navigationsAfterCreate = 0;
      page.on("framenavigated", (frame) => {
        if (frame === page.mainFrame()) navigationsAfterCreate += 1;
      });
      await createTaskForLane(server.backendDir, lane.threadId, title);
      try {
        await page.getByText("Task capture: " + title + " (serve.ui)").waitFor({
          state: "visible",
          timeout: 10000,
        });
      } catch (error) {
        throw new Error(
          "timed out waiting for live task card: " +
            JSON.stringify(await taskCardDiagnostics(page, lane.targetId)) +
            "\n" +
            (error.stack || error.message),
        );
      }
      if (navigationsAfterCreate !== 0)
        throw new Error("task card appeared after page navigation/reload");
      return {
        targetId: lane.targetId,
        threadId: lane.threadId,
        title,
        url: server.url,
      };
    },
  );
}

async function waitForLiveTaskCardSubscription(page, targetId) {
  await page.waitForFunction(
    (id) => {
      const lane = laneStates.get(id);
      return Boolean(
        lane &&
          lane.liveBusSubscribed &&
          lane.latestPayload &&
          typeof liveBusIsOpen === "function" &&
          liveBusIsOpen(),
      );
    },
    targetId,
    { timeout: 10000 },
  );
  await page.waitForTimeout(250);
}

async function ensureLiveTaskCardLane(page) {
  return page.evaluate((targetOffset) => {
    const boundTargets = targets.filter(
      (target) => target.targetIdentity?.thread?.state === "bound",
    );
    const choices = boundTargets.length ? boundTargets : targets;
    const target = choices[Math.min(targetOffset, choices.length - 1)];
    if (!target) throw new Error("no target available for task-card smoke");
    let lane = laneStates.get(target.id);
    if (!lane) {
      addLane(target.id);
      lane = laneStates.get(target.id);
    }
    if (!lane) throw new Error("no lane available for task-card smoke");
    const threadId = lane.targetThreadId || lane.activeThreadId || "";
    if (!threadId) throw new Error("lane has no bound thread for task-card smoke");
    return { targetId: lane.targetId, threadId };
  }, liveTaskCardTargetOffset);
}

async function taskCardDiagnostics(page, targetId) {
  return page.evaluate((id) => {
    const lane = laneStates.get(id);
    const messages = lane
      ? lane.knownMessages.map((item) => ({
          display: item.display_text || item.text || "",
          kind: item.kind || "",
          source: item.source_kind || "",
        }))
      : [];
    return {
      liveBusOpen: typeof liveBusIsOpen === "function" && liveBusIsOpen(),
      messages,
      subscribed: Boolean(lane && lane.liveBusSubscribed),
      targetId: id,
      threadId: lane ? lane.targetThreadId || lane.activeThreadId || "" : "",
    };
  }, targetId);
}

async function createTaskForLane(backendDir, threadId, title) {
  const command = process.env.SPICE_SERVE_BIN || "spice"; // env-policy: allow
  const { stdout, stderr } = await execFileAsync(
    command,
    [
      "task",
      "add",
      title,
      "--project",
      "serve.ui",
      "--acceptance",
      "Live task card appears without page reload",
      "--origin",
      liveTaskCardSmokeOrigin,
    ],
    {
      cwd: repoRoot,
      env: {
        ...process.env, // env-policy: allow
        CODEX_THREAD_ID: threadId,
        SPICE_TASK_BACKEND: backendDir,
      },
      timeout: 10000,
    },
  );
  if (!stdout.includes("created "))
    throw new Error("task add did not report creation:\n" + stdout + stderr);
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
