const { execFile } = require("child_process");
const { promisify } = require("util");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const execFileAsync = promisify(execFile);
const liveTaskCardSmokeOrigin = "ack:20260101T000000000000Z";
// Later bound scratch targets avoid unrelated historical image fixtures.
const liveTaskCardTargetOffset = 2;
const liveTaskCardStageTimeoutMs = 10000;

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-task-card-live-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await waitForTaskCardStage(
        page,
        "server-lane",
        () => Boolean(document.querySelector(".lane")),
      );
      await waitForTaskCardStage(
        page,
        "targets-ready",
        () =>
          typeof targets !== "undefined" &&
          Array.isArray(targets) &&
          targets.length > 0 &&
          typeof addLane === "function" &&
          typeof laneStates !== "undefined",
      );
      const lane = await ensureLiveTaskCardLane(page);
      await waitForTaskCardStage(
        page,
        "bound-lane-selected",
        ({ targetId, threadId }) => {
          const selected = laneStates.get(targetId);
          return Boolean(
            selected &&
              selected.targetId === targetId &&
              (selected.targetThreadId || selected.activeThreadId || "") ===
                threadId,
          );
        },
        lane,
        lane.targetId,
      );
      const subscription = await waitForLiveTaskCardSubscription(page, lane);
      const title = "Live task card smoke " + Date.now();
      const acceptanceCriteria = [
        "Live task card appears without page reload",
        "Each acceptance criterion renders on its own row",
      ];
      let navigationsAfterCreate = 0;
      page.on("framenavigated", (frame) => {
        if (frame === page.mainFrame()) navigationsAfterCreate += 1;
      });
      await createTaskForLane(
        server.backendDir,
        lane.threadId,
        title,
        acceptanceCriteria,
      );
      await waitForTaskCardStage(
        page,
        "watch-delivery",
        ({ targetId, title, generation }) => {
          const selected = laneStates.get(targetId);
          return Boolean(
            selected &&
              selected.liveBusSubscriptionGeneration === generation &&
              selected.knownMessages.some((item) =>
                String(item.display_text || item.text || "").includes(title),
              ),
          );
        },
        {
          targetId: lane.targetId,
          title,
          generation: subscription.generation,
        },
        lane.targetId,
      );
      await waitForTaskCardVisible(
        page,
        lane.targetId,
        title,
        acceptanceCriteria,
      );
      if (navigationsAfterCreate !== 0)
        throw new Error("task card appeared after page navigation/reload");
      return {
        requestSequence: subscription.requestSequence,
        subscriptionGeneration: subscription.generation,
        targetId: lane.targetId,
        threadId: lane.threadId,
        title,
        url: server.url,
      };
    },
  );
}

async function waitForLiveTaskCardSubscription(page, lane) {
  await waitForTaskCardStage(
    page,
    "socket-open",
    () => typeof liveBusIsOpen === "function" && liveBusIsOpen(),
    undefined,
    lane.targetId,
  );
  await waitForTaskCardStage(
    page,
    "subscription-generation",
    (targetId) => {
      const selected = laneStates.get(targetId);
      return Boolean(selected && selected.liveBusSubscriptionGeneration);
    },
    lane.targetId,
    lane.targetId,
  );
  await waitForTaskCardStage(
    page,
    "watcher-activation",
    (targetId) => {
      const selected = laneStates.get(targetId);
      return Boolean(
        selected &&
          selected.liveBusSubscribed &&
          selected.liveBusWatcherActive &&
          selected.liveBusSubscriptionGeneration,
      );
    },
    lane.targetId,
    lane.targetId,
  );
  const activeGeneration = await page.evaluate((targetId) => {
    return String(
      laneStates.get(targetId)?.liveBusSubscriptionGeneration || "",
    );
  }, lane.targetId);
  await waitForTaskCardStage(
    page,
    "initial-payload",
    ({ targetId, generation }) => {
      const selected = laneStates.get(targetId);
      return Boolean(
        selected &&
          selected.liveBusSubscriptionGeneration === generation &&
          selected.liveBusSubscribePending === false &&
          selected.latestPayload,
      );
    },
    { targetId: lane.targetId, generation: activeGeneration },
    lane.targetId,
  );
  return page.evaluate(
    (value) => ({
      generation: value,
      requestSequence:
        typeof liveBusRequestSequence === "number"
          ? liveBusRequestSequence
          : null,
    }),
    activeGeneration,
  );
}

async function ensureLiveTaskCardLane(page) {
  return page.evaluate((targetOffset) => {
    const boundTargets = targets.filter(
      (target) => target.targetIdentity?.thread?.state === "bound",
    );
    const target =
      boundTargets[Math.min(targetOffset, boundTargets.length - 1)];
    if (!target) throw new Error("no bound target available for task-card smoke");
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

async function waitForTaskCardStage(
  page,
  stage,
  predicate,
  argument = undefined,
  targetId = "",
) {
  try {
    await page.waitForFunction(predicate, argument, {
      timeout: liveTaskCardStageTimeoutMs,
    });
  } catch (error) {
    throw new Error(
      "task-card stage " +
        stage +
        " timed out: " +
        JSON.stringify(await taskCardDiagnostics(page, targetId, stage)) +
        "\n" +
        (error.stack || error.message),
    );
  }
}

async function waitForTaskCardVisible(
  page,
  targetId,
  title,
  acceptanceCriteria,
) {
  try {
    await page.getByText("Task capture: " + title + " (serve.ui)").waitFor({
      state: "visible",
      timeout: liveTaskCardStageTimeoutMs,
    });
    const card = page
      .locator("blockquote.task-directive-quote")
      .filter({ hasText: title });
    for (const criterion of acceptanceCriteria) {
      await card.getByText(criterion, { exact: true }).waitFor({
        state: "visible",
        timeout: liveTaskCardStageTimeoutMs,
      });
    }
  } catch (error) {
    throw new Error(
      "task-card stage card-visible timed out: " +
        JSON.stringify(
          await taskCardDiagnostics(page, targetId, "card-visible"),
        ) +
        "\n" +
        (error.stack || error.message),
    );
  }
}

async function taskCardDiagnostics(page, targetId, stage) {
  return page.evaluate(({ id, stageName }) => {
    const stateMap =
      typeof laneStates !== "undefined" && laneStates instanceof Map
        ? laneStates
        : null;
    const lane = stateMap ? stateMap.get(id) : null;
    const observedMessageKeys = lane
      ? lane.knownMessages.map((item) => item.key || "")
      : [];
    const latestPayloadMessageKeys = Array.isArray(lane?.latestPayload?.messages)
      ? lane.latestPayload.messages.map((item) => item.key || "")
      : [];
    return {
      initialPayloadApplied: Boolean(lane && lane.latestPayload),
      latestPayloadMessageKeys,
      liveBusOpen: typeof liveBusIsOpen === "function" && liveBusIsOpen(),
      observedMessageKeys,
      requestSequence:
        typeof liveBusRequestSequence === "number"
          ? liveBusRequestSequence
          : null,
      socketReadyState:
        typeof liveBusSocket !== "undefined" && liveBusSocket
          ? liveBusSocket.readyState
          : null,
      stage: stageName,
      subscribePending: Boolean(lane && lane.liveBusSubscribePending),
      subscribed: Boolean(lane && lane.liveBusSubscribed),
      subscriptionGeneration: lane
        ? lane.liveBusSubscriptionGeneration || ""
        : "",
      targetId: id,
      targetCount:
        typeof targets !== "undefined" && Array.isArray(targets)
          ? targets.length
          : 0,
      threadId: lane ? lane.targetThreadId || lane.activeThreadId || "" : "",
      watcherActive: Boolean(lane && lane.liveBusWatcherActive),
    };
  }, { id: targetId, stageName: stage });
}

async function createTaskForLane(
  backendDir,
  threadId,
  title,
  acceptanceCriteria,
) {
  const command = process.env.SPICE_SERVE_BIN || "spice"; // env-policy: allow
  const acceptanceArgs = acceptanceCriteria.flatMap((criterion) => [
    "--acceptance",
    criterion,
  ]);
  const { stdout, stderr } = await execFileAsync(
    command,
    [
      "task",
      "add",
      title,
      "--project",
      "serve.ui",
      ...acceptanceArgs,
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
