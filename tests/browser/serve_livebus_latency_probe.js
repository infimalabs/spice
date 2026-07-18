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
// Submit path (Pass S) -- a REAL scratch-backed lane.send:
//   * browser submit marks   -- app enqueueSend/liveBusRequest performance marks
//                               (startedAt, liveBusFrameSentAt, response receipt,
//                               resultAppliedAt) via window.__spiceSubmitLatencySamples
//   * server receive+compute -- lane.sendTiming serverTiming (targetResolveMs,
//                               sendPayloadMs, totalBeforeReplyMs, reply lock/write)
//   * reply send completion  -- reply lock hold / write, totalMs
// The reply shares ONE send_lock with every watch-push frame, but livebus.py
// `_send` now encodes each frame to bytes BEFORE taking the lock, so the lock's
// critical section covers only the socket write and a small lane.sendResult ack
// acquires it as soon as any in-flight write returns rather than queuing behind a
// bulk-payload encode. Pass S fires a genuine lane.send into the isolated scratch
// backend (no operator inbox is touched, no stubbed transport) while the eight
// lanes stay active, so the submit numbers are measured, not modelled.
//
// Cards that never arrive inside the per-frame budget are recorded as misses
// (render rate), not thrown: a dropped or deferred card is measurement data.
//
// Per-pass send-lock telemetry is isolated by zeroing the server's frame
// counters (bus.ping {reset:true}) at each pass boundary, so every pass owns its
// own totals AND maxima rather than inheriting a cumulative high-water mark.
//
// Trace metadata records the measured commit and fixture conditions so every
// published statistic is attributable to a specific code baseline.
//
// Usage: node tests/browser/serve_livebus_latency_probe.js
//   PROBE_LANES (default 8), PROBE_ROUNDS (default 5) tune the load.
//   PROBE_SUBMITS (default 5) tunes the real lane.send submit pass.

const os = require("os");
const path = require("path");
const fsp = require("fs/promises");
const { execFile } = require("child_process");
const { promisify } = require("util");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const execFileAsync = promisify(execFile);

// Seed a fully isolated N-lane environment (ephemeral git repo + private driver
// home + scratch task backend, all under one throwaway root) and return its
// paths. Serve launched with cwd at repoRoot and CLAUDE_CONFIG_DIR at driverHome
// discovers ONLY these scratch worktrees, so a real lane.send in Pass S lands in
// a scratch inbox and no real agent is ever steered.
async function seedScratchLanes(seedRoot, laneCount) {
  // The seeder imports spice, so it must run under the repo venv. Commit to that
  // one interpreter; a missing venv fails loudly here rather than silently
  // falling back to a system python that cannot import spice.
  const python = path.join(repoRoot, ".venv", "bin", "python");
  const { stdout } = await execFileAsync(
    python,
    [
      path.join(repoRoot, "tests", "browser", "test_livebusseed.py"),
      "--root",
      seedRoot,
      "--lanes",
      String(laneCount),
    ],
    { cwd: repoRoot, timeout: probeAddTimeoutMs },
  );
  return JSON.parse(stdout.trim());
}

const probeOrigin = "ack:1jN54zJJ";
const probeLaneCount = Number(process.env.PROBE_LANES || 8); // env-policy: allow
const probeRounds = Number(process.env.PROBE_ROUNDS || 5); // env-policy: allow
const probeSubmits = Number(process.env.PROBE_SUBMITS || 5); // env-policy: allow
const probeSubmitBudgetMs = 20000;
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
  // Everything the probe touches lives under one throwaway root: an ephemeral
  // git repo with `probeLaneCount` worktrees, a private driver home, and a
  // scratch task backend. Serve is pointed at all three, so lane discovery,
  // task-adds, and the Pass S lane.send stay inside this root and cannot reach a
  // real worktree or steer a real agent. The finally removes the whole tree.
  const seedRoot = await fsp.mkdtemp(path.join(os.tmpdir(), "livebus-probe-"));
  try {
    const seed = await seedScratchLanes(seedRoot, probeLaneCount);
    return await withServePage(
      {
        path: "/?smoke=serve-livebus-latency-probe-" + Date.now(),
        contextOptions: { viewport: { width: 1280, height: 720 } },
        cwd: seed.repoRoot,
        env: { CLAUDE_CONFIG_DIR: seed.driverHome },
        args: (scratch) => [
          "serve",
          "--host",
          "127.0.0.1",
          "--port",
          "0",
          "--until",
          scratch.stopFile,
          "--task-backend",
          seed.taskBackend,
        ],
      },
      async ({ page, server }) => runProbe(page, server, seed),
    );
  } finally {
    await removeSeedRoot(seedRoot);
  }
}

// Remove the throwaway seed root. The harness has already awaited serve's exit,
// so nothing is writing here -- but fs.rm does not retry ENOTEMPTY/EBUSY unless
// asked, and APFS can briefly report a just-emptied git-worktree directory as
// non-empty. Retry a few times to absorb that. A measurement already produced
// must never be lost to temp-dir housekeeping, so a persistent failure warns
// loudly on stderr rather than throwing over the returned artifact.
async function removeSeedRoot(seedRoot) {
  try {
    await fsp.rm(seedRoot, {
      recursive: true,
      force: true,
      maxRetries: 5,
      retryDelay: 100,
    });
  } catch (error) {
    console.warn(
      "livebus probe: could not remove seed root " +
        seedRoot +
        " (" +
        (error && error.code ? error.code : error && error.message) +
        "); leaving it for the OS to reclaim",
    );
  }
}

async function runProbe(page, server, seed) {
  await page.waitForFunction(
    () =>
      laneStore.targetsSnapshot().length > 0 &&
      typeof addLane === "function" &&
      typeof laneStore !== "undefined" &&
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

  // Each pass zeroes the server frame telemetry at its start (bus.ping
  // {reset:true}) so its send-lock totals AND maxima are isolated to that
  // pass and never inherit a prior pass's cumulative high-water mark.

  // Pass A -- concurrent focused fan-out baseline.
  await forceLanesFocused(page, lanes);
  await resetTelemetry(page);
  await resetFrameLog(page);
  const passASamples = await runPass(page, seed, lanes, "A");
  const afterA = await snapshotTelemetry(page);
  const passAReconcile = await reconcileFrameCounts(page);

  // Pass C -- one focused lane, with true background lanes coalesced.
  await waitAllWatchersActive(page, lanes);
  const focusLane = lanes[0];
  await setLaneFocus(page, lanes, [focusLane.targetId]);
  await resetTelemetry(page);
  await resetFrameLog(page);
  const passCSamples = await runSingleFocusPass(page, seed, focusLane);
  const afterC = await snapshotTelemetry(page);
  const passCReconcile = await reconcileFrameCounts(page);

  // Pass B -- repeat concurrent fan-out after atomic task replacements.
  await resetTelemetry(page);
  await resetFrameLog(page);
  const passBSamples = await runPass(page, seed, lanes, "B", {
    reforceFocusEachRound: true,
  });
  const afterB = await snapshotTelemetry(page);
  const passBReconcile = await reconcileFrameCounts(page);

  // Pass S -- a real scratch-backed lane.send while all lanes stay active.
  await waitAllWatchersActive(page, lanes);
  await forceLanesFocused(page, lanes);
  await resetTelemetry(page);
  await resetFrameLog(page);
  const submitSamples = await runSubmitPass(page, lanes);
  const afterS = await snapshotTelemetry(page);
  const passSReconcile = await reconcileFrameCounts(page);

  const meta = await probeMeta(server, seed, lanes);

  return buildReport({
    meta,
    url: server.url,
    lanes,
    activateMs,
    passA: {
      samples: passASamples,
      sendLock: sendLockStats(afterA),
      reconcileFrames: passAReconcile,
    },
    passB: {
      samples: passBSamples,
      sendLock: sendLockStats(afterB),
      reconcileFrames: passBReconcile,
    },
    passC: {
      samples: passCSamples,
      sendLock: sendLockStats(afterC),
      reconcileFrames: passCReconcile,
    },
    submit: {
      samples: submitSamples,
      sendLock: sendLockStats(afterS),
      reconcileFrames: passSReconcile,
    },
  });
}

// Pick up to `count` real bound targets, open a lane for each, and return the
// (targetId, threadId) pairs whose watchers this probe will exercise.
async function bindLanes(page, count) {
  return page.evaluate((count) => {
    const bound = laneStore.targetsSnapshot().filter(
      (target) => target.targetIdentity?.thread?.state === "bound",
    );
    const lanes = [];
    for (const target of bound.slice(0, count)) {
      if (!laneStore.hasLane(target.id)) addLane(target.id);
      const lane = laneStore.laneForId(target.id);
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
          const lane = laneStore.laneForId(targetId);
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
        const lane = laneStore.laneForId(targetId);
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
          const lane = laneStore.laneForId(targetId);
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
async function runPass(page, seed, lanes, tag, options = {}) {
  const samples = [];
  for (let round = 0; round < probeRounds; round += 1) {
    if (options.reforceFocusEachRound) await forceLanesFocused(page, lanes);
    const roundSamples = await emitRound(page, seed, lanes, tag, round);
    samples.push(...roundSamples);
  }
  return samples;
}

// Sequential emits to one focused lane -- one task, wait for its card, repeat --
// mirroring an operator who submits and waits rather than a concurrent burst.
async function runSingleFocusPass(page, seed, lane) {
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
    await createTaskForThread(seed, lane.threadId, title);
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

// Pass S: fire `probeSubmits` REAL scratch-backed lane.send frames through the
// app's own submit entry point (enqueueSend -> sendLanePayload ->
// liveBusRequest("lane.send")) one at a time, while all lanes stay active, and
// harvest the browser submit-latency sample the app records for each. No
// transport is stubbed: the frame crosses the real socket, the server resolves
// the target and writes the isolated scratch inbox, and the reply plus the
// lane.sendTiming frame carry the server-measured intervals back. Each sample
// carries browser performance marks (startedAt, optimisticRenderedAt,
// liveBusFrameSentAt, response receipt, resultAppliedAt, completedAt), the
// app-computed browser durations, and the merged serverTiming.
async function runSubmitPass(page, lanes) {
  const targetId = lanes[0].targetId;
  return page.evaluate(
    async ({ targetId, count, budgetMs }) => {
      const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
      const samples = window.__spiceSubmitLatencySamples;
      const out = [];
      for (let index = 0; index < count; index += 1) {
        const lane = laneStore.laneForId(targetId);
        if (!lane) {
          out.push({ index, targetId, status: "no-lane", marks: {}, durations: {}, serverTiming: {} });
          continue;
        }
        const before = samples.length;
        const text = "livebus submit probe l0 " + Date.now() + "_" + index;
        // Real submit through the operator entry point: builds the browser
        // marks, sends the lane.send frame, applies the sendResult.
        enqueueSend(lane, { text });
        const deadline = Date.now() + budgetMs;
        let sample = null;
        while (Date.now() < deadline) {
          const candidate = samples[samples.length - 1];
          if (
            samples.length > before &&
            candidate &&
            candidate.completed &&
            candidate.marks &&
            Number.isFinite(candidate.marks.startedAt) &&
            // Wait for the lane.sendTiming frame to merge the full server timing
            // (reply lock hold/write) so the harvested server numbers are whole.
            candidate.serverTiming &&
            Number.isFinite(candidate.serverTiming.replyLockHoldMs)
          ) {
            sample = candidate;
            break;
          }
          await sleep(25);
        }
        out.push(
          sample
            ? {
                index,
                targetId,
                status: sample.status || "",
                textLength: sample.textLength || 0,
                marks: sample.marks || {},
                durations: sample.durations || {},
                serverTiming: sample.serverTiming || {},
              }
            : { index, targetId, status: "miss", marks: {}, durations: {}, serverTiming: {} },
        );
        // Space submits so each fully settles (reply applied, inbox write
        // flushed) before the next real send starts.
        await sleep(75);
      }
      return out;
    },
    { targetId, count: probeSubmits, budgetMs: probeSubmitBudgetMs },
  );
}

async function emitRound(page, seed, lanes, tag, round) {
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
      await createTaskForThread(seed, lane.threadId, title);
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

async function createTaskForThread(seed, threadId, title) {
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
      // Run the task-add inside the seeded repo, against the seeded task backend
      // and driver home, so the emitted card lands on a scratch lane serve is
      // watching and nothing touches a real worktree.
      cwd: seed.repoRoot,
      env: {
        ...process.env, // env-policy: allow
        CLAUDE_CONFIG_DIR: seed.driverHome,
        CODEX_THREAD_ID: threadId,
        SPICE_TASK_BACKEND: seed.taskBackend,
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

// Zero the server's per-frame send-lock telemetry so the NEXT pass starts from
// empty counters. Called at each pass boundary; combined with sendLockStats this
// makes every pass own its totals AND maxima instead of inheriting a cumulative
// high-water mark from an earlier pass.
async function resetTelemetry(page) {
  await page.evaluate(async () => {
    await liveBusRequest("bus.ping", { reset: true });
  });
}

// Per-pass send-lock intervals read directly from the post-pass telemetry. The
// pass was preceded by resetTelemetry, so the absolute counters ARE this pass's
// intervals -- including the wait/hold maxima, which are genuine per-pass single
// -frame high-water marks rather than a lifetime cumulative maximum. Reported per
// frame kind plus a fan-out total whose maxima are the largest single-frame
// interval observed in the pass across kinds.
function sendLockStats(after) {
  const afterFrames = (after && after.frames) || {};
  const perKind = {};
  let totalCount = 0;
  let totalWaitMs = 0;
  let totalHoldMs = 0;
  let waitMax = 0;
  let holdMax = 0;
  for (const kind of Object.keys(afterFrames)) {
    const a = afterFrames[kind] || {};
    const count = num(a.count);
    if (count <= 0) continue;
    const waitTotalMs = num(a.sendLockWaitMsTotal);
    const holdTotalMs = num(a.sendLockHoldMsTotal);
    const waitKindMax = num(a.sendLockWaitMsMax);
    const holdKindMax = num(a.sendLockHoldMsMax);
    perKind[kind] = {
      count,
      waitTotalMs: round2(waitTotalMs),
      waitMeanMs: round2(waitTotalMs / count),
      waitMaxMs: round2(waitKindMax),
      holdTotalMs: round2(holdTotalMs),
      holdMeanMs: round2(holdTotalMs / count),
      holdMaxMs: round2(holdKindMax),
    };
    totalCount += count;
    totalWaitMs += waitTotalMs;
    totalHoldMs += holdTotalMs;
    waitMax = Math.max(waitMax, waitKindMax);
    holdMax = Math.max(holdMax, holdKindMax);
  }
  return {
    totals: {
      count: totalCount,
      waitTotalMs: round2(totalWaitMs),
      waitMeanMs: totalCount ? round2(totalWaitMs / totalCount) : 0,
      waitMaxMs: round2(waitMax),
      holdTotalMs: round2(totalHoldMs),
      holdMeanMs: totalCount ? round2(totalHoldMs / totalCount) : 0,
      holdMaxMs: round2(holdMax),
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

// Flatten one real-send sample into the submit-path stages the diagnosis
// reports: browser-measured intervals (app-computed durations) and the
// server-measured intervals (merged serverTiming). All values are millisecond
// durations self-consistent within their own clock, so no cross-process
// subtraction is performed here.
function submitBreakdown(sample) {
  const browser = sample.durations || {};
  const server = sample.serverTiming || {};
  const finite = (value) => (Number.isFinite(value) ? round2(value) : null);
  return {
    index: sample.index,
    status: sample.status,
    accepted: sample.status === "accepted",
    textLength: sample.textLength || 0,
    // Browser submit path (performance.now() deltas the app records).
    browserOptimisticRenderMs: finite(browser.optimisticRenderMs),
    browserSendResultWaitMs: finite(browser.sendResultWaitMs),
    browserResponseHandlingMs: finite(browser.responseHandlingMs),
    browserTotalMs: finite(browser.totalMs),
    // Server submit path (perf_counter deltas from lane.sendTiming).
    serverTargetResolveMs: finite(server.targetResolveMs),
    serverSendPayloadMs: finite(server.sendPayloadMs),
    serverTotalBeforeReplyMs: finite(server.totalBeforeReplyMs),
    serverReplyLockWaitMs: finite(server.replyLockWaitMs),
    serverReplyLockHoldMs: finite(server.replyLockHoldMs),
    serverReplyWriteMs: finite(server.replyWriteMs),
    serverTotalMs: finite(server.totalMs),
  };
}

const submitStageKeys = [
  "browserOptimisticRenderMs",
  "browserSendResultWaitMs",
  "browserResponseHandlingMs",
  "browserTotalMs",
  "serverTargetResolveMs",
  "serverSendPayloadMs",
  "serverTotalBeforeReplyMs",
  "serverReplyLockWaitMs",
  "serverReplyLockHoldMs",
  "serverReplyWriteMs",
  "serverTotalMs",
];

function summarizeSubmit(submit) {
  const breakdowns = submit.samples.map(submitBreakdown);
  const accepted = breakdowns.filter((row) => row.accepted).length;
  const stages = {};
  for (const key of submitStageKeys)
    stages[key] = summarize(breakdowns.map((row) => row[key]));
  return {
    sampleCount: breakdowns.length,
    acceptedCount: accepted,
    acceptRate: breakdowns.length ? round2(accepted / breakdowns.length) : null,
    stages,
    sendLock: submit.sendLock,
    reconcileFrames: submit.reconcileFrames,
    samples: breakdowns,
  };
}

// Record the code baseline and fixture conditions every published statistic is
// attributable to. The commit is the tree the probe actually ran against, and
// treeDirty flags whether uncommitted edits were present at measurement time.
async function probeMeta(server, seed, lanes) {
  // The commit/dirty stamp describes the live-bus code under measurement, which
  // is the real repo checkout serve was built from -- not the ephemeral seeded
  // repo -- so these git reads run against repoRoot.
  const commit = await gitOut(["rev-parse", "HEAD"]);
  const commitShort = await gitOut(["rev-parse", "--short", "HEAD"]);
  const dirty = (await gitOut(["status", "--porcelain"])).length > 0;
  return {
    source: "tests/browser/serve_livebus_latency_probe.js",
    commit,
    commitShort,
    treeDirty: dirty,
    host: process.platform + " " + os.release() + " " + process.arch,
    node: process.version,
    requestedLanes: probeLaneCount,
    boundLanes: lanes.length,
    seededLanes: seed.lanes.length,
    rounds: probeRounds,
    submits: probeSubmits,
    frameBudgetMs: probeFrameBudgetMs,
    submitBudgetMs: probeSubmitBudgetMs,
    isolation:
      "ephemeral scratch fixture: an init-fresh git repo with one worktree per " +
      "lane, a private CLAUDE_CONFIG_DIR driver home (canned pid-0 transcripts), " +
      "and a scratch task backend, all under a throwaway temp root. serve " +
      "discovers ONLY these worktrees, so every task-add, watch push, and the " +
      "Pass S lane.send stays inside the fixture; no real worktree or agent is touched.",
    conditions: {
      passA: "all bound lanes focused; concurrent per-round fan-out (baseline)",
      passC: "single focused lane; remaining lanes background-coalesced (realistic)",
      passB: "all lanes focused again after atomic task-event replacements (rearm stress)",
      submit: "one real scratch-backed lane.send per submit while all lanes stay active",
    },
    baselineNote:
      "current live-bus baseline: per-card refreshServerTopology amplifier removed; " +
      "livebus.py _send encodes each frame before taking send_lock. Passes A/B/C differ " +
      "only by focus configuration; the send_lock is not artificially stubbed or amplified.",
  };
}

async function gitOut(args) {
  try {
    const { stdout } = await execFileAsync("git", args, { cwd: repoRoot });
    return stdout.trim();
  } catch (error) {
    return "";
  }
}

function buildReport({ meta, url, lanes, activateMs, passA, passB, passC, submit }) {
  return {
    meta,
    url,
    laneCount: lanes.length,
    requestedLaneCount: probeLaneCount,
    rounds: probeRounds,
    submits: probeSubmits,
    subscribeToActivateMs: round2(activateMs),
    passA: summarizePass(passA),
    passB: summarizePass(passB),
    passC: summarizePass(passC),
    submit: summarizeSubmit(submit),
  };
}

// Pretty-print the report, but collapse each per-pass `samples` array element
// onto a single line. The raw samples are archival detail the deterministic
// summarizer never reads (it derives every table from the aggregated stages),
// and expanding them balloons the artifact past the repo file-size gate. Only
// the whitespace inside each sample changes; every measured value is verbatim.
function serializeReport(report) {
  const passKeys = ["passA", "passB", "passC", "submit"];
  const stashed = {};
  const shallow = { ...report };
  for (const key of passKeys) {
    const pass = report[key];
    if (pass && Array.isArray(pass.samples)) {
      stashed[key] = pass.samples;
      shallow[key] = { ...pass, samples: `@@SAMPLES_${key}@@` };
    }
  }
  let text = JSON.stringify(shallow, null, 2);
  for (const key of passKeys) {
    if (!(key in stashed)) continue;
    const rows = stashed[key].map((sample) => "      " + JSON.stringify(sample));
    const compact = rows.length ? "[\n" + rows.join(",\n") + "\n    ]" : "[]";
    text = text.replace(`"@@SAMPLES_${key}@@"`, () => compact);
  }
  return text;
}

if (require.main === module) {
  run()
    .then((report) => {
      console.log(serializeReport(report));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run, serializeReport };
