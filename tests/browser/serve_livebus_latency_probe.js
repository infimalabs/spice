// Live-bus latency probe: a deterministic, repeatable diagnostic that drives
// the REAL emit-to-card path under approximately eight concurrent lanes and
// records per-stage timestamps so the diagnosis rests on measured intervals,
// not static inference.
//
// What it measures, and how the clocks line up:
//   * message emit           -- node Date.now() around `spice task add`
//   * watcher detection      -- server watchTiming.changeDetectedWallMs
//   * signature/payload compute -- server watchTiming.signatureMs / payloadMs
//   * send-lock wait and hold   -- session frame telemetry (bus.pong), diffed
//                                  per pass so each pass owns its lock intervals
//   * socket delivery        -- browser receivedAt - server preSendWallMs
//   * browser card render    -- browser renderedAt - receivedAt
// Server time.time() and browser/node Date.now() are epoch milliseconds on the
// same host, so cross-process deltas are meaningful; the perf-counter deltas in
// watchTiming time the CPU-bound compute independent of clock choice.
//
// Three passes verify current delivery and coalescing behaviour:
//   * Pass A -- all lanes focused, concurrent full-push baseline.
//   * Pass C -- one focused lane at operator cadence; the remaining lanes
//     coalesce into lanes.dirty without delaying the focused card.
//   * Pass B -- all lanes focused again after prior atomic task-event
//     replacements, proving every persistent kqueue watcher rearmed and the
//     removed per-card topology refresh did not reappear as reconcile traffic.
//
// The submit-to-ack reply shares ONE send_lock with every watch-push frame
// (livebus.py `_send`, whose json write runs INSIDE the lock). A real
// cross-agent `lane.send` would inject into a live peer's inbox, so this probe
// does not fire one; instead it reads the shared send-lock wait/hold telemetry
// accumulated under the concurrent fan-out, which is exactly the serialization a
// reply faces. The stubbed client-mark submit profile lives in
// serve_submit_latency_smoke.js.
//
// Cards that never arrive inside the per-frame budget are recorded as misses
// (render rate), not thrown: a dropped or deferred card is measurement data.
//
// Usage: node tests/browser/serve_livebus_latency_probe.js
//   PROBE_LANES (default 8), PROBE_ROUNDS (default 5) tune the load.

const { execFile } = require("child_process");
const { promisify } = require("util");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const execFileAsync = promisify(execFile);

const probeOrigin = "ack:20260101T000000000000Z";
const probeLaneCount = Number(process.env.PROBE_LANES || 8); // env-policy: allow
const probeRounds = Number(process.env.PROBE_ROUNDS || 5); // env-policy: allow
const probeReadyTimeoutMs = 25000;
const probeAddTimeoutMs = 30000;
// A card that has not rendered within this budget after the add returns is a
// miss, recorded as data rather than throwing.
const probeFrameBudgetMs = 20000;
// Watcher activation for a full lane set is itself multi-second and highly
// variable (measured 8s to >60s across runs), so its wait is budgeted well
// above the per-emit budget and its duration is reported as a headline stage.
const probeActivateTimeoutMs = 150000;
const reconcileFrameTypes = [
  "targets.payload",
  "teams.payload",
  "lanes.dirty",
  "lane.unsubscribed",
  "lane.configured",
];

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-livebus-latency-probe-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof targets !== "undefined" &&
          Array.isArray(targets) &&
          targets.length > 0 &&
          typeof addLane === "function" &&
          typeof laneStates !== "undefined" &&
          typeof handleLiveBusMessage === "function" &&
          typeof liveBusRequest === "function",
        { timeout: probeReadyTimeoutMs },
      );
      // Subscribe→activate is measured across the whole set: node and the page
      // share the host clock, so stamping just before the addLane fan-out and
      // again once every watcher reports active bounds the slowest lane's
      // activation, a latency the diagnosis reports in its own right.
      const subscribeStartMs = Date.now();
      const lanes = await bindLanes(page, probeLaneCount);
      if (lanes.length === 0)
        throw new Error("no bound targets available for the live-bus probe");
      await waitAllWatchersActive(page, lanes);
      const activateMs = Date.now() - subscribeStartMs;
      await installProbeHook(page);

      // Pass A -- concurrent focused fan-out baseline.
      await forceLanesFocused(page, lanes);
      const beforeA = await snapshotTelemetry(page);
      await resetFrameLog(page);
      const passASamples = await runPass(page, server, lanes, "A");
      const afterA = await snapshotTelemetry(page);
      const passAReconcile = await reconcileFrameCounts(page);

      // Pass C -- one focused lane, with true background lanes coalesced.
      await waitAllWatchersActive(page, lanes);
      const focusLane = lanes[0];
      await setLaneFocus(page, lanes, [focusLane.targetId]);
      const beforeC = await snapshotTelemetry(page);
      await resetFrameLog(page);
      const passCSamples = await runSingleFocusPass(page, server, focusLane);
      const afterC = await snapshotTelemetry(page);
      const passCReconcile = await reconcileFrameCounts(page);

      // Pass B -- repeat concurrent fan-out after atomic task replacements.
      const beforeB = await snapshotTelemetry(page);
      await resetFrameLog(page);
      const passBSamples = await runPass(page, server, lanes, "B", {
        reforceFocusEachRound: true,
      });
      const afterB = await snapshotTelemetry(page);
      const passBReconcile = await reconcileFrameCounts(page);

      return buildReport({
        url: server.url,
        lanes,
        activateMs,
        passA: {
          samples: passASamples,
          sendLock: sendLockDelta(beforeA, afterA),
          reconcileFrames: passAReconcile,
        },
        passB: {
          samples: passBSamples,
          sendLock: sendLockDelta(beforeB, afterB),
          reconcileFrames: passBReconcile,
        },
        passC: {
          samples: passCSamples,
          sendLock: sendLockDelta(beforeC, afterC),
          reconcileFrames: passCReconcile,
        },
      });
    },
  );
}

// Pick up to `count` real bound targets, open a lane for each, and return the
// (targetId, threadId) pairs whose watchers this probe will exercise.
async function bindLanes(page, count) {
  return page.evaluate((count) => {
    const bound = targets.filter(
      (target) => target.targetIdentity?.thread?.state === "bound",
    );
    const lanes = [];
    for (const target of bound.slice(0, count)) {
      if (!laneStates.has(target.id)) addLane(target.id);
      const lane = laneStates.get(target.id);
      if (!lane) continue;
      const threadId = lane.targetThreadId || lane.activeThreadId || "";
      if (threadId) lanes.push({ targetId: lane.targetId, threadId });
    }
    return lanes;
  }, count);
}

// Block until every probed lane reports an active watcher. A single
// waitForFunction over the whole id set (rather than a race of per-lane waits)
// mirrors the proven diagnostic path and settles on the slowest lane.
async function waitAllWatchersActive(page, lanes) {
  const targetIds = lanes.map((lane) => lane.targetId);
  try {
    await page.waitForFunction(
      (targetIds) =>
        targetIds.every((targetId) => {
          const lane = laneStates.get(targetId);
          return Boolean(
            lane &&
              lane.liveBusSubscribed &&
              lane.liveBusWatcherActive &&
              lane.liveBusSubscriptionGeneration,
          );
        }),
      targetIds,
      { timeout: probeActivateTimeoutMs },
    );
  } catch (error) {
    const states = await page.evaluate((ids) => {
      return ids.map((targetId) => {
        const lane = laneStates.get(targetId);
        return {
          targetId,
          exists: Boolean(lane),
          closed: Boolean(lane && lane.closed),
          subscribed: Boolean(lane && lane.liveBusSubscribed),
          subscribePending: Boolean(lane && lane.liveBusSubscribePending),
          watcherActive: Boolean(lane && lane.liveBusWatcherActive),
          generation: lane ? lane.liveBusSubscriptionGeneration || "" : "",
          dirty: Boolean(lane && lane.liveBusDirty),
          transientError: lane?.statusErrorEl?.textContent || "",
        };
      });
    }, targetIds);
    throw new Error(
      "live-bus watcher activation timed out: " + JSON.stringify(states) +
        "\n" + (error.stack || error.message),
    );
  }
}

// Configure lane focus explicitly: each id in `focusedTargetIds` takes the
// full-push path (coalesce_background_update returns False for focused:true),
// every other probed lane is set focused:false so its changes defer into a
// coalesced lanes.dirty frame. Passing every id reproduces the concurrent
// full-push stress; passing one reproduces the realistic single-focus operator.
async function setLaneFocus(page, lanes, focusedTargetIds) {
  const focusedSet = focusedTargetIds;
  await page.evaluate(
    async ({ targetIds, focused }) => {
      const focusedLookup = new Set(focused);
      await Promise.all(
        targetIds.map((targetId) => {
          const lane = laneStates.get(targetId);
          if (!lane) return Promise.resolve();
          const base =
            typeof laneMessageQuery === "function"
              ? laneMessageQuery(lane)
              : {
                  limit: 200,
                  after: lane.newestMessageKey || "",
                  threadId: lane.targetThreadId || "",
                };
          return liveBusRequest("lane.configure", {
            targetId,
            query: { ...base, focused: focusedLookup.has(targetId) },
          }).catch(() => {});
        }),
      );
    },
    {
      targetIds: lanes.map((lane) => lane.targetId),
      focused: focusedSet,
    },
  );
}

async function forceLanesFocused(page, lanes) {
  await setLaneFocus(
    page,
    lanes,
    lanes.map((lane) => lane.targetId),
  );
}

// Wrap the live-bus message handler so each watch-origin lane.payload frame
// records its browser receipt time (before the app applies it) and its render
// time (after application completes), keyed to the emitted card text; every
// frame's type and arrival time also lands in a log for reconcile accounting.
async function installProbeHook(page) {
  await page.evaluate(() => {
    if (window.__probeInstalled) return;
    window.__probeInstalled = true;
    window.__probeFrames = [];
    window.__frameLog = [];
    const originalHandle = handleLiveBusMessage;
    // eslint-disable-next-line no-global-assign
    handleLiveBusMessage = async function (data) {
      const receivedAt = Date.now();
      let entry = null;
      try {
        const message = JSON.parse(data || "{}");
        window.__frameLog.push({ at: receivedAt, type: message.type || "?" });
        if (message && message.type === "lane.payload" && message.watchTiming) {
          const messages = (message.payload && message.payload.messages) || [];
          entry = {
            receivedAt,
            targetId: message.targetId || "",
            watchTiming: message.watchTiming,
            texts: messages.map((item) =>
              String(item.display_text || item.text || ""),
            ),
          };
        }
      } catch (error) {
        entry = null;
      }
      const result = await originalHandle(data);
      if (entry) {
        entry.renderedAt = Date.now();
        window.__probeFrames.push(entry);
      }
      return result;
    };
  });
}

async function resetFrameLog(page) {
  await page.evaluate(() => {
    window.__frameLog = [];
  });
}

async function reconcileFrameCounts(page) {
  return page.evaluate((types) => {
    const counts = {};
    for (const type of types) {
      counts[type] = 0;
    }
    const frames = window.__frameLog || [];
    for (const frame of frames) {
      if (Object.prototype.hasOwnProperty.call(counts, frame.type)) {
        counts[frame.type] += 1;
      }
    }
    return counts;
  }, reconcileFrameTypes);
}

// One pass: `probeRounds` rounds, each emitting a uniquely titled task to every
// lane at once (the concurrent fan-out) and blocking on each card's arrival.
async function runPass(page, server, lanes, tag, options = {}) {
  const samples = [];
  for (let round = 0; round < probeRounds; round += 1) {
    if (options.reforceFocusEachRound) await forceLanesFocused(page, lanes);
    const roundSamples = await emitRound(page, server, lanes, tag, round);
    samples.push(...roundSamples);
  }
  return samples;
}

// Sequential emits to one focused lane -- one task, wait for its card, repeat --
// mirroring an operator who submits and waits rather than a concurrent burst.
async function runSingleFocusPass(page, server, lane) {
  const samples = [];
  for (let round = 0; round < probeRounds; round += 1) {
    const title =
      "livebus probe C r" +
      round +
      " l0 " +
      Date.now() +
      "_" +
      round +
      "_0";
    const emitStart = Date.now();
    await createTaskForThread(server.backendDir, lane.threadId, title);
    const emitReturned = Date.now();
    const frame = await waitForProbeFrame(page, lane.targetId, title);
    samples.push({
      tag: "C",
      round,
      laneIndex: 0,
      targetId: lane.targetId,
      emitStart,
      emitReturned,
      addDurationMs: emitReturned - emitStart,
      rendered: Boolean(frame),
      receivedAt: frame ? frame.receivedAt : null,
      renderedAt: frame ? frame.renderedAt : null,
      watchTiming: frame ? frame.watchTiming : null,
    });
  }
  return samples;
}

async function emitRound(page, server, lanes, tag, round) {
  return Promise.all(
    lanes.map(async (lane, laneIndex) => {
      const title =
        "livebus probe " +
        tag +
        " r" +
        round +
        " l" +
        laneIndex +
        " " +
        Date.now() +
        "_" +
        round +
        "_" +
        laneIndex;
      const emitStart = Date.now();
      await createTaskForThread(server.backendDir, lane.threadId, title);
      const emitReturned = Date.now();
      const frame = await waitForProbeFrame(page, lane.targetId, title);
      return {
        tag,
        round,
        laneIndex,
        targetId: lane.targetId,
        emitStart,
        emitReturned,
        addDurationMs: emitReturned - emitStart,
        rendered: Boolean(frame),
        receivedAt: frame ? frame.receivedAt : null,
        renderedAt: frame ? frame.renderedAt : null,
        watchTiming: frame ? frame.watchTiming : null,
      };
    }),
  );
}

async function createTaskForThread(backendDir, threadId, title) {
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
      "probe",
      "--origin",
      probeOrigin,
    ],
    {
      cwd: repoRoot,
      env: {
        ...process.env, // env-policy: allow
        CODEX_THREAD_ID: threadId,
        SPICE_TASK_BACKEND: backendDir,
      },
      timeout: probeAddTimeoutMs,
    },
  );
  if (!stdout.includes("created "))
    throw new Error("probe task add did not report creation:\n" + stdout + stderr);
}

// Resolve to the browser receipt/render marks for the card carrying `title`, or
// to null if no such card renders within the frame budget -- a miss is data.
async function waitForProbeFrame(page, targetId, title) {
  try {
    await page.waitForFunction(
      ({ targetId, title }) =>
        (window.__probeFrames || []).some(
          (frame) =>
            frame.targetId === targetId &&
            frame.texts.some((text) => text.includes(title)),
        ),
      { targetId, title },
      { timeout: probeFrameBudgetMs },
    );
  } catch (error) {
    return null;
  }
  return page.evaluate(
    ({ targetId, title }) => {
      const frame = (window.__probeFrames || []).find(
        (frame) =>
          frame.targetId === targetId &&
          frame.texts.some((text) => text.includes(title)),
      );
      return {
        receivedAt: frame.receivedAt,
        renderedAt: frame.renderedAt,
        watchTiming: frame.watchTiming,
      };
    },
    { targetId, title },
  );
}

async function snapshotTelemetry(page) {
  return page.evaluate(async () => {
    const pong = await liveBusRequest("bus.ping", {});
    return pong && pong.diagnostics ? pong.diagnostics : null;
  });
}

// Per-pass send-lock intervals: diff the cumulative frame telemetry captured
// before and after the pass so each pass owns exactly the lock wait/hold it
// generated. Reported per frame kind plus a fan-out total.
function sendLockDelta(before, after) {
  const beforeFrames = (before && before.frames) || {};
  const afterFrames = (after && after.frames) || {};
  const kinds = new Set([
    ...Object.keys(beforeFrames),
    ...Object.keys(afterFrames),
  ]);
  const perKind = {};
  let totalCount = 0;
  let totalWaitMs = 0;
  let totalHoldMs = 0;
  for (const kind of kinds) {
    const b = beforeFrames[kind] || {};
    const a = afterFrames[kind] || {};
    const count = num(a.count) - num(b.count);
    if (count <= 0) continue;
    const waitTotalMs = num(a.sendLockWaitMsTotal) - num(b.sendLockWaitMsTotal);
    const holdTotalMs = num(a.sendLockHoldMsTotal) - num(b.sendLockHoldMsTotal);
    perKind[kind] = {
      count,
      waitTotalMs: round2(waitTotalMs),
      waitMeanMs: round2(waitTotalMs / count),
      waitMaxMs: round2(num(a.sendLockWaitMsMax)),
      holdTotalMs: round2(holdTotalMs),
      holdMeanMs: round2(holdTotalMs / count),
      holdMaxMs: round2(num(a.sendLockHoldMsMax)),
    };
    totalCount += count;
    totalWaitMs += waitTotalMs;
    totalHoldMs += holdTotalMs;
  }
  return {
    totals: {
      count: totalCount,
      waitTotalMs: round2(totalWaitMs),
      waitMeanMs: totalCount ? round2(totalWaitMs / totalCount) : 0,
      holdTotalMs: round2(totalHoldMs),
      holdMeanMs: totalCount ? round2(totalHoldMs / totalCount) : 0,
    },
    perKind,
  };
}

function num(value) {
  return Number.isFinite(Number(value)) ? Number(value) : 0;
}

// Turn each raw sample into the per-stage intervals the diagnosis reports.
function stageBreakdown(sample) {
  const watch = sample.watchTiming || {};
  const finite = (value) => (Number.isFinite(value) ? value : null);
  return {
    tag: sample.tag,
    round: sample.round,
    laneIndex: sample.laneIndex,
    targetId: sample.targetId,
    rendered: sample.rendered,
    addDurationMs: sample.addDurationMs,
    // Watcher can only fire after the on-disk write, and emitReturned is the
    // write-confirmed instant, so a positive value is a hard lower bound on
    // detection latency; <= 0 means detection landed inside the add subprocess.
    detectionFromReturnMs: sample.rendered
      ? finite(watch.changeDetectedWallMs - sample.emitReturned)
      : null,
    detectionFromStartMs: sample.rendered
      ? finite(watch.changeDetectedWallMs - sample.emitStart)
      : null,
    signatureMs: sample.rendered ? finite(watch.signatureMs) : null,
    payloadMs: sample.rendered ? finite(watch.payloadMs) : null,
    detectToSendMs: sample.rendered ? finite(watch.detectToSendMs) : null,
    socketMs: sample.rendered
      ? finite(sample.receivedAt - watch.preSendWallMs)
      : null,
    renderMs: sample.rendered
      ? finite(sample.renderedAt - sample.receivedAt)
      : null,
    totalFromReturnMs: sample.rendered
      ? finite(sample.renderedAt - sample.emitReturned)
      : null,
    totalFromStartMs: sample.rendered
      ? finite(sample.renderedAt - sample.emitStart)
      : null,
  };
}

function summarize(values) {
  const clean = values
    .filter((value) => Number.isFinite(value))
    .slice()
    .sort((a, b) => a - b);
  if (clean.length === 0) return { n: 0 };
  const quantile = (fraction) =>
    clean[Math.min(clean.length - 1, Math.floor(fraction * clean.length))];
  const sum = clean.reduce((total, value) => total + value, 0);
  return {
    n: clean.length,
    min: round2(clean[0]),
    p50: round2(quantile(0.5)),
    p90: round2(quantile(0.9)),
    max: round2(clean[clean.length - 1]),
    mean: round2(sum / clean.length),
  };
}

function round2(value) {
  if (!Number.isFinite(value)) return null;
  return Math.round(value * 100) / 100;
}

const stageKeys = [
  "addDurationMs",
  "detectionFromReturnMs",
  "detectionFromStartMs",
  "signatureMs",
  "payloadMs",
  "detectToSendMs",
  "socketMs",
  "renderMs",
  "totalFromReturnMs",
  "totalFromStartMs",
];

function summarizePass(pass) {
  const breakdowns = pass.samples.map(stageBreakdown);
  const rendered = breakdowns.filter((row) => row.rendered).length;
  const stages = {};
  for (const key of stageKeys)
    stages[key] = summarize(breakdowns.map((row) => row[key]));
  return {
    sampleCount: breakdowns.length,
    renderedCount: rendered,
    renderRate: breakdowns.length
      ? round2(rendered / breakdowns.length)
      : null,
    stages,
    sendLock: pass.sendLock,
    reconcileFrames: pass.reconcileFrames,
    samples: breakdowns,
  };
}

function buildReport({ url, lanes, activateMs, passA, passB, passC }) {
  return {
    url,
    laneCount: lanes.length,
    requestedLaneCount: probeLaneCount,
    rounds: probeRounds,
    subscribeToActivateMs: round2(activateMs),
    passA: summarizePass(passA),
    passB: summarizePass(passB),
    passC: summarizePass(passC),
  };
}

if (require.main === module) {
  run()
    .then((report) => {
      console.log(JSON.stringify(report, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
