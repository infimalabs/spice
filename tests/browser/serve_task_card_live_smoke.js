const { execFile } = require("child_process");
const { promisify } = require("util");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const execFileAsync = promisify(execFile);
const liveTaskCardSmokeOrigin = "ack:1jN54zJJ";
// Later bound scratch targets avoid unrelated historical image fixtures.
const liveTaskCardTargetOffset = 2;
const liveTaskCardStageTimeoutMs = 10000;
const liveTaskCardAcceptanceCriteria = [
  "Live task card appears without page reload",
  "Each acceptance criterion renders on its own row",
];
// Metadata rows the live card must surface beyond the acceptance criteria: the
// stored origin spelling passed to `task add`, and the resolved flow pipeline
// for a serve.ui add (config default_flow ["todo", "review"] renders as
// "todo, review"). Each renders in its own <dd>, so an exact-text match pins the
// value that reached the DOM.
const liveTaskCardExpectedMetadata = [liveTaskCardSmokeOrigin, "todo, review"];

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-task-card-live-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    runTaskCardScenario,
  );
}

async function runTaskCardScenario({ page, server }) {
  const { lane, subscription } = await prepareLiveTaskCardLane(page);
  await installTaskCardTimingProbe(page);
  let navigationsAfterCreate = 0;
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) navigationsAfterCreate += 1;
  });
  const first = await createAndObserveTaskCard(
    page,
    server,
    lane,
    subscription,
    "Live task card smoke " + Date.now(),
    "watch-delivery",
  );
  const followup = await createAndObserveTaskCard(
    page,
    server,
    lane,
    subscription,
    "Live task card follow-up " + Date.now(),
    "follow-up-watch-delivery",
  );
  if (navigationsAfterCreate !== 0)
    throw new Error("task card appeared after page navigation/reload");
  return {
    requestSequence: subscription.requestSequence,
    subscriptionGeneration: subscription.generation,
    targetId: lane.targetId,
    threadId: lane.threadId,
    title: first.title,
    followupTitle: followup.title,
    timing: first.timing,
    followupTiming: followup.timing,
    url: server.url,
  };
}

async function prepareLiveTaskCardLane(page) {
  await waitForTaskCardStage(
    page,
    "server-lane",
    () => Boolean(document.querySelector(".lane")),
  );
  await waitForTaskCardStage(
    page,
    "targets-ready",
    () =>
      laneStore.targetsSnapshot().length > 0 &&
      typeof addLane === "function" &&
      typeof laneStore !== "undefined",
  );
  const lane = await ensureLiveTaskCardLane(page);
  await focusLiveTaskCardLane(page, lane);
  await waitForTaskCardStage(
    page,
    "bound-lane-selected",
    ({ targetId, threadId }) => {
      const selected = laneStore.laneForId(targetId);
      return Boolean(
        selected &&
          selected.targetId === targetId &&
          (selected.targetThreadId || selected.activeThreadId || "") === threadId,
      );
    },
    lane,
    lane.targetId,
  );
  const subscription = await waitForLiveTaskCardSubscription(page, lane);
  return { lane, subscription };
}

async function createAndObserveTaskCard(
  page,
  server,
  lane,
  subscription,
  title,
  stage,
) {
  const createTiming = await createTaskForLane(
    server.backendDir,
    lane.threadId,
    title,
    liveTaskCardAcceptanceCriteria,
  );
  await waitForTaskCardStage(
    page,
    stage,
    taskCardReachedLane,
    { targetId: lane.targetId, title, generation: subscription.generation },
    lane.targetId,
  );
  await waitForTaskCardVisible(
    page,
    lane.targetId,
    title,
    liveTaskCardAcceptanceCriteria,
  );
  const timing = await taskCardDeliveryTiming(
    page,
    lane.targetId,
    title,
    createTiming,
  );
  return { title, timing };
}

function taskCardReachedLane({ targetId, title, generation }) {
  const selected = laneStore.laneForId(targetId);
  return Boolean(
    selected &&
      selected.liveBusSubscriptionGeneration === generation &&
      selected.knownMessages.some((item) =>
        String(item.display_text || item.text || "").includes(title),
      ),
  );
}

async function focusLiveTaskCardLane(page, lane) {
  await page.evaluate((targetId) => {
    const selected = laneStore.laneForId(targetId);
    if (!selected) throw new Error("task-card focus lane disappeared");
    setFocusedLiveBusLane(selected);
  }, lane.targetId);
  await waitForTaskCardStage(
    page,
    "focused-lane",
    (targetId) => {
      const selected = laneStore.laneForId(targetId);
      return Boolean(selected && liveBusLaneIsFocused(selected));
    },
    lane.targetId,
    lane.targetId,
  );
  await page.evaluate(async (targetId) => {
    const selected = laneStore.laneForId(targetId);
    if (!selected) throw new Error("task-card focus lane disappeared");
    await liveBusRequest("lane.configure", {
      targetId,
      query: laneMessageQuery(selected),
    });
  }, lane.targetId);
}

async function installTaskCardTimingProbe(page) {
  await page.evaluate(() => {
    window.__spiceTaskCardTimingFrames = [];
    const originalHandle = handleLiveBusMessage;
    // eslint-disable-next-line no-global-assign
    handleLiveBusMessage = async function (data) {
      const receivedAt = Date.now();
      let timingFrame = null;
      try {
        const message = JSON.parse(data || "{}");
        if (
          message.type === "lane.payload" &&
          message.source === "watch" &&
          message.watchTiming
        ) {
          timingFrame = {
            receivedAt,
            targetId: message.targetId || "",
            texts: ((message.payload || {}).messages || []).map((item) =>
              String(item.display_text || item.text || ""),
            ),
            watchTiming: message.watchTiming,
          };
        }
      } catch (error) {
        timingFrame = null;
      }
      const result = await originalHandle(data);
      if (timingFrame) {
        timingFrame.renderedAt = Date.now();
        window.__spiceTaskCardTimingFrames.push(timingFrame);
      }
      return result;
    };
  });
}

async function taskCardDeliveryTiming(
  page,
  targetId,
  title,
  createTiming,
) {
  const frame = await page.evaluate(
    ({ id, expectedTitle }) => {
      const match = (window.__spiceTaskCardTimingFrames || []).find(
        (candidate) =>
          candidate.targetId === id &&
          candidate.texts.some((text) => text.includes(expectedTitle)),
      );
      return match || null;
    },
    { id: targetId, expectedTitle: title },
  );
  if (!frame)
    throw new Error("task-card timing frame missing for " + JSON.stringify(title));
  const watch = frame.watchTiming || {};
  return {
    taskAddStartedWallMs: createTiming.startedAt,
    taskAddReturnedWallMs: createTiming.returnedAt,
    changeDetectedWallMs: watch.changeDetectedWallMs,
    preSendWallMs: watch.preSendWallMs,
    browserReceivedWallMs: frame.receivedAt,
    browserRenderedWallMs: frame.renderedAt,
    stagesMs: {
      taskAdd: createTiming.returnedAt - createTiming.startedAt,
      detectionAfterAddReturn:
        watch.changeDetectedWallMs - createTiming.returnedAt,
      signature: watch.signatureMs,
      payload: watch.payloadMs,
      socket: frame.receivedAt - watch.preSendWallMs,
      browserApplyAndRender: frame.renderedAt - frame.receivedAt,
      totalAfterAddReturn: frame.renderedAt - createTiming.returnedAt,
    },
  };
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
      const selected = laneStore.laneForId(targetId);
      return Boolean(selected && selected.liveBusSubscriptionGeneration);
    },
    lane.targetId,
    lane.targetId,
  );
  await waitForTaskCardStage(
    page,
    "watcher-activation",
    (targetId) => {
      const selected = laneStore.laneForId(targetId);
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
      laneStore.laneForId(targetId)?.liveBusSubscriptionGeneration || "",
    );
  }, lane.targetId);
  await waitForTaskCardStage(
    page,
    "initial-payload",
    ({ targetId, generation }) => {
      const selected = laneStore.laneForId(targetId);
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
    const boundTargets = laneStore.targetsSnapshot().filter(
      (target) => target.targetIdentity?.thread?.state === "bound",
    );
    const target =
      boundTargets[Math.min(targetOffset, boundTargets.length - 1)];
    if (!target) throw new Error("no bound target available for task-card smoke");
    let lane = laneStore.laneForId(target.id);
    if (!lane) {
      addLane(target.id);
      lane = laneStore.laneForId(target.id);
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
    for (const value of liveTaskCardExpectedMetadata) {
      await card.getByText(value, { exact: true }).waitFor({
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
    const lane = laneStore.laneForId(id) || null;
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
      targetCount: laneStore.targetsSnapshot().length,
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
  const startedAt = Date.now();
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
  return { startedAt, returnedAt: Date.now() };
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
