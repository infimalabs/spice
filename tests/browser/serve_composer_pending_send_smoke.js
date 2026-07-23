const { installIsolatedLaneFixture } = require("./serve_isolated_lane_fixture");
const { withServePage } = require("./serve_playwright_harness");

const SUCCESS_TEXT = "pending send resolves successfully";
const FAILURE_TEXT = "pending send restores after failure";
const REPEATED_TEXT = "second keyboard submission without clicking";
const QUEUED_FAILURE_TEXT = "queued after the failing submission";
const NEWER_TEXT = "new draft typed while sends are pending";
const SHARED_BODY = "shared focused body";
const SHARED_QUOTE = "shared quote";
const SHARED_CONTEXT = "shared context";

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-composer-pending-send-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: ["submitLaneForm", "syncComposerPendingSendState"],
      });
      await page.addScriptTag({
        content: [
          configureGroupedComposer,
          pendingSendSnapshot,
          pendingSendSubmission,
          resolveSuccessfulPendingSend,
          runGroupedButtonSubmission,
          runGroupedComposerScenarios,
          runRepeatedKeyboardSubmission,
          waitForPendingSendCount,
          waitForSendCount,
        ]
          .map((helper) => helper.toString())
          .join("\n"),
      });
      const config = {
        failureText: FAILURE_TEXT,
        newerText: NEWER_TEXT,
        queuedFailureText: QUEUED_FAILURE_TEXT,
        repeatedText: REPEATED_TEXT,
        sharedBody: SHARED_BODY,
        sharedContext: SHARED_CONTEXT,
        sharedQuote: SHARED_QUOTE,
        successText: SUCCESS_TEXT,
      };
      const result = await page.evaluate(runPendingSendScenarios, config);
      assertPendingSendScenarios(result, config);
      return { ...result, url: server.url };
    },
  );
}

async function runPendingSendScenarios(config) {
  const lane = resolveIsolatedLane("composer-pending-send-smoke-team");
  syncComposerShards(laneGroupHost(lane), laneGroupMemberLanes(laneGroupHost(lane)));
  const textarea = lane.shardTextareas.get(lane.targetId);
  if (!textarea) throw new Error("composer pending-send smoke needs a textarea");
  const originalLiveBusRequest = liveBusRequest;
  const sends = [];
  liveBusRequest = (type, fields = {}) => {
    if (type !== "lane.send") return originalLiveBusRequest(type, fields);
    return new Promise((resolve) => sends.push({ fields, resolve }));
  };
  try {
    textarea.value = config.successText;
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    lane.formEl.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    const successPending = pendingSendSnapshot(lane, textarea, sends.length);
    textarea.value = config.repeatedText;
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    lane.formEl.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    const afterSecondSubmit = pendingSendSnapshot(lane, textarea, sends.length);
    sends[0].resolve({
      result: {
        ok: true,
        key: "composer-pending-success",
        requestText: config.successText,
        pendingInboxCount: 1,
        pendingInboxKeys: ["composer-pending-success"],
        pendingInboxRevision: "composer-pending-success-revision",
        pendingInboxVersion: 1,
        submission: pendingSendSubmission("composer-pending-success"),
        agentEnsure: { ok: true, threadId: lane.targetThreadId || "" },
      },
    });
    await waitForSendCount(sends, 2);
    const rapidQueued = {
      payloadText: sends[1].fields.payload.text,
      snapshot: pendingSendSnapshot(lane, textarea, sends.length),
    };
    resolveSuccessfulPendingSend(
      sends[1],
      lane,
      "composer-pending-repeat-success",
      sends[1].fields.payload.text,
      2,
    );
    await waitForPendingSendCount(lane, 0);
    const successSettled = pendingSendSnapshot(lane, textarea, sends.length);

    textarea.value = config.failureText;
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    lane.formEl.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    const failureSend = sends[sends.length - 1];
    textarea.value = config.queuedFailureText;
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    lane.formEl.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    textarea.value = config.newerText;
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    const failurePending = pendingSendSnapshot(lane, textarea, sends.length);
    failureSend.resolve({ result: { ok: false, error: "expected smoke failure" } });
    await waitForPendingSendCount(lane, 0);
    const failureSettled = pendingSendSnapshot(lane, textarea, sends.length);
    const grouped = await runGroupedComposerScenarios(sends, config);
    return {
      afterSecondSubmit,
      failurePending,
      failureSettled,
      grouped,
      rapidQueued,
      successPending,
      successSettled,
    };
  } finally {
    liveBusRequest = originalLiveBusRequest;
  }
}

async function runGroupedComposerScenarios(sends, config) {
  const host = resolveIsolatedLane("composer-target-host");
  const member = resolveIsolatedLane("composer-target-member");
  configureGroupedComposer(host, member);
  const sharedDraft = (id) => ({
    id,
    quoteText: config.sharedQuote,
    text: config.sharedContext,
  });
  host.quoteDrafts.set(host.targetId, [sharedDraft("host-command-quote")]);
  host.quoteDrafts.set(member.targetId, [sharedDraft("member-command-quote")]);
  syncComposerShards(host, [host, member]);
  const hostTextarea = host.shardTextareas.get(host.targetId);
  const memberTextarea = host.shardTextareas.get(member.targetId);
  if (!hostTextarea || !memberTextarea)
    throw new Error("grouped composer smoke needs both target textareas");
  hostTextarea.value = config.sharedBody;
  memberTextarea.value = config.sharedBody;
  hostTextarea.focus();
  const commandStart = sends.length;
  hostTextarea.dispatchEvent(
    new KeyboardEvent("keydown", {
      bubbles: true,
      cancelable: true,
      key: "Enter",
      metaKey: true,
    }),
  );
  const commandSends = sends.slice(commandStart);
  const commandPending = {
    focused: pendingSendSnapshot(host, hostTextarea, commandSends.length),
    other: pendingSendSnapshot(host, memberTextarea, commandSends.length),
    payloads: commandSends.map((send) => ({
      targetId: send.fields.targetId,
      text: send.fields.payload.text,
    })),
  };
  resolveSuccessfulPendingSend(
    commandSends[0],
    host,
    "composer-command-success",
    commandSends[0].fields.payload.text,
    2,
  );
  await waitForPendingSendCount(host, 0);
  const commandFirstSettled = {
    focusState:
      document.activeElement === hostTextarea ? "focused" : "blurred",
    focusedText: hostTextarea.value,
    otherQuoteState: host.quoteDrafts.has(member.targetId) ? "retained" : "cleared",
    otherState: memberTextarea.disabled ? "locked" : "editable",
    otherText: memberTextarea.value,
  };

  const repeated = await runRepeatedKeyboardSubmission(
    sends,
    config,
    host,
    hostTextarea,
    commandFirstSettled.focusState,
  );
  const button = await runGroupedButtonSubmission(
    sends,
    host,
    member,
    hostTextarea,
    memberTextarea,
  );
  return {
    ...button,
    commandPending,
    commandFirstSettled,
    ...repeated,
    hostTargetId: host.targetId,
    memberTargetId: member.targetId,
  };
}

function configureGroupedComposer(host, member) {
  laneStore.applyLaneGroups([[host.targetId, member.targetId]], { isLaneOpen });
}

async function runRepeatedKeyboardSubmission(
  sends,
  config,
  host,
  hostTextarea,
  focusBeforeSubmit,
) {
  hostTextarea.value = config.repeatedText;
  hostTextarea.dispatchEvent(new Event("input", { bubbles: true }));
  const repeatedStart = sends.length;
  hostTextarea.dispatchEvent(
    new KeyboardEvent("keydown", {
      bubbles: true,
      cancelable: true,
      key: "Enter",
      metaKey: true,
    }),
  );
  const repeatedSends = sends.slice(repeatedStart);
  const commandRepeatedPending = {
    focusBeforeSubmit,
    payloads: repeatedSends.map((send) => ({
      targetId: send.fields.targetId,
      text: send.fields.payload.text,
    })),
  };
  resolveSuccessfulPendingSend(
    repeatedSends[0],
    host,
    "composer-command-repeat-success",
    repeatedSends[0].fields.payload.text,
    3,
  );
  await waitForPendingSendCount(host, 0);
  return {
    commandRepeatedPending,
    commandRepeatedSettled: {
      focusState:
        document.activeElement === hostTextarea ? "focused" : "blurred",
      focusedText: hostTextarea.value,
    },
  };
}

async function runGroupedButtonSubmission(
  sends,
  host,
  member,
  hostTextarea,
  memberTextarea,
) {
  hostTextarea.value = "button host body";
  host.quoteDrafts.set(host.targetId, [
    {
      id: "host-button-quote",
      quoteText: "button host quote",
      text: "button host context",
    },
  ]);
  renderComposerQuoteBands(host);
  const buttonStart = sends.length;
  host.formEl.dispatchEvent(
    new Event("submit", { bubbles: true, cancelable: true }),
  );
  const buttonSends = sends.slice(buttonStart);
  const buttonPayloads = buttonSends.map((send) => ({
    targetId: send.fields.targetId,
    text: send.fields.payload.text,
  }));
  buttonSends.forEach((send, index) =>
    resolveSuccessfulPendingSend(
      send,
      laneStore.laneForId(send.fields.targetId),
      "composer-button-success-" + index,
      send.fields.payload.text,
      4,
    ),
  );
  await Promise.all([
    waitForPendingSendCount(host, 0),
    waitForPendingSendCount(member, 0),
  ]);
  return {
    buttonPayloads,
    buttonSettled: {
      hostQuoteState: host.quoteDrafts.has(host.targetId) ? "retained" : "cleared",
      hostText: hostTextarea.value,
      memberQuoteState: host.quoteDrafts.has(member.targetId)
        ? "retained"
        : "cleared",
      memberText: memberTextarea.value,
    },
  };
}

function pendingSendSnapshot(lane, textarea, sendCount) {
  const shard = textarea.closest(".composer-shard");
  return {
    appearance: getComputedStyle(textarea).opacity === "1" ? "normal" : "dimmed",
    composerState: textarea.disabled ? "locked" : "editable",
    opacity: getComputedStyle(textarea).opacity,
    pendingCount: Number(lane.pendingSubmissionCount) || 0,
    queueCount: lane.pendingSendQueue.length,
    shardPendingCount: Number(shard.dataset.pendingSendCount) || 0,
    sendCount,
    submitState: lane.submitEl.disabled ? "locked" : "editable",
    text: textarea.value,
  };
}

function pendingSendSubmission(key) {
  const at = new Date().toISOString();
  return {
    key,
    stage: "accepted",
    disposition: "",
    stages: { accepted: { at, source: "inbox-write", evidence: key } },
    durationsMs: {},
  };
}

function resolveSuccessfulPendingSend(send, lane, key, requestText, version) {
  send.resolve({
    result: {
      ok: true,
      key,
      requestText,
      pendingInboxCount: 1,
      pendingInboxKeys: [key],
      pendingInboxRevision: key + "-revision",
      pendingInboxVersion: version,
      submission: pendingSendSubmission(key),
      agentEnsure: { ok: true, threadId: lane.targetThreadId || "" },
    },
  });
}

function waitForPendingSendCount(lane, expected) {
  return new Promise((resolve) => {
    const current = () => Math.max(0, Number(lane.pendingSubmissionCount) || 0);
    if (current() === expected) {
      resolve(expected);
      return;
    }
    const timer = setInterval(() => {
      if (current() === expected) {
        clearInterval(timer);
        resolve(expected);
      }
    }, 0);
  });
}

function waitForSendCount(sends, expected) {
  return new Promise((resolve) => {
    if (sends.length === expected) {
      resolve(expected);
      return;
    }
    const timer = setInterval(() => {
      if (sends.length === expected) {
        clearInterval(timer);
        resolve(expected);
      }
    }, 0);
  });
}

function expectEqual(actual, expected, message) {
  if (Object.is(actual, expected)) return;
  throw new Error(message + ": expected " + expected + ", got " + actual);
}

function assertPendingSendScenarios(result, config) {
  assertRapidPendingSendScenario(result, config);
  assertFailedPendingSendScenario(result, config);
  assertGroupedCommandSendScenario(result.grouped, config);
  assertGroupedButtonSendScenario(result.grouped, config);
}

function assertRapidPendingSendScenario(result, config) {
  expectEqual(
    result.successPending.appearance,
    "normal",
    "submitted composer text appearance",
  );
  expectEqual(result.successPending.opacity, "1", "submitted text opacity");
  expectEqual(result.successPending.composerState, "editable", "pending composer state");
  expectEqual(result.successPending.submitState, "editable", "pending submit state");
  expectEqual(result.successPending.text, "", "submitted draft detaches immediately");
  expectEqual(result.successPending.pendingCount, 1, "first pending count");
  expectEqual(result.afterSecondSubmit.sendCount, 1, "queued composer send count");
  expectEqual(result.afterSecondSubmit.pendingCount, 2, "rapid pending count");
  expectEqual(result.afterSecondSubmit.queueCount, 1, "rapid local queue count");
  expectEqual(result.afterSecondSubmit.text, "", "rapid queued draft detaches");
  expectEqual(result.rapidQueued.snapshot.sendCount, 2, "FIFO second dispatch count");
  expectEqual(
    result.rapidQueued.payloadText,
    config.repeatedText,
    "FIFO second dispatch payload",
  );
  expectEqual(result.successSettled.appearance, "normal", "accepted appearance");
  expectEqual(result.successSettled.composerState, "editable", "accepted state");
  expectEqual(result.successSettled.text, "", "accepted composer text");
  expectEqual(result.successSettled.pendingCount, 0, "accepted pending count");
}

function assertFailedPendingSendScenario(result, config) {
  expectEqual(result.failurePending.composerState, "editable", "failure pending state");
  expectEqual(result.failurePending.pendingCount, 2, "failure queue pending count");
  expectEqual(result.failurePending.queueCount, 1, "failure queue depth");
  expectEqual(result.failurePending.text, config.newerText, "new draft remains editable");
  expectEqual(result.failureSettled.appearance, "normal", "failed appearance");
  expectEqual(result.failureSettled.composerState, "editable", "failed state");
  expectEqual(result.failureSettled.pendingCount, 0, "failed queue pending count");
  expectEqual(
    result.failureSettled.text,
    config.failureText +
      "\n\n" +
      config.queuedFailureText +
      "\n\n" +
      config.newerText,
    "failed and queued drafts restore in order",
  );
}

function assertGroupedCommandSendScenario(grouped, config) {
  expectEqual(grouped.commandPending.payloads.length, 1, "Command-Enter send count");
  expectEqual(
    grouped.commandPending.payloads[0].targetId,
    grouped.hostTargetId,
    "Command-Enter target",
  );
  expectEqual(
    grouped.commandPending.payloads[0].text,
    config.sharedBody + "\n\n> " + config.sharedQuote + "\n\n" + config.sharedContext,
    "Command-Enter merged target stack",
  );
  expectEqual(grouped.commandPending.focused.appearance, "normal", "focused appearance");
  expectEqual(grouped.commandPending.focused.composerState, "editable", "focused state");
  expectEqual(grouped.commandPending.other.appearance, "normal", "other appearance");
  expectEqual(grouped.commandPending.other.composerState, "editable", "other state");
  expectEqual(grouped.commandFirstSettled.focusState, "focused", "restored focus");
  expectEqual(grouped.commandFirstSettled.focusedText, "", "focused accepted text");
  expectEqual(
    grouped.commandFirstSettled.otherText,
    config.sharedBody,
    "other draft text",
  );
  expectEqual(
    grouped.commandFirstSettled.otherQuoteState,
    "retained",
    "other quote state",
  );
  expectEqual(
    grouped.commandFirstSettled.otherState,
    "editable",
    "other settled state",
  );
  expectEqual(
    grouped.commandRepeatedPending.focusBeforeSubmit,
    "focused",
    "repeat submit focus",
  );
  expectEqual(
    grouped.commandRepeatedPending.payloads.length,
    1,
    "repeat Command-Enter send count",
  );
  expectEqual(
    grouped.commandRepeatedPending.payloads[0].targetId,
    grouped.hostTargetId,
    "repeat Command-Enter target",
  );
  expectEqual(
    grouped.commandRepeatedPending.payloads[0].text,
    config.repeatedText,
    "repeat Command-Enter text",
  );
  expectEqual(
    grouped.commandRepeatedSettled.focusState,
    "focused",
    "repeat restored focus",
  );
  expectEqual(
    grouped.commandRepeatedSettled.focusedText,
    "",
    "repeat accepted text",
  );
}

function assertGroupedButtonSendScenario(grouped, config) {
  expectEqual(grouped.buttonPayloads.length, 2, "button fan-out count");
  const hostPayload = grouped.buttonPayloads.find(
    (payload) => payload.targetId === grouped.hostTargetId,
  );
  const memberPayload = grouped.buttonPayloads.find(
    (payload) => payload.targetId === grouped.memberTargetId,
  );
  expectEqual(Boolean(hostPayload), true, "button host payload presence");
  expectEqual(Boolean(memberPayload), true, "button member payload presence");
  expectEqual(
    hostPayload.text,
    "button host body\n\n> button host quote\n\nbutton host context",
    "button host merged stack",
  );
  expectEqual(
    memberPayload.text,
    config.sharedBody + "\n\n> " + config.sharedQuote + "\n\n" + config.sharedContext,
    "button member merged stack",
  );
  expectEqual(grouped.buttonSettled.hostText, "", "button host cleared text");
  expectEqual(grouped.buttonSettled.memberText, "", "button member cleared text");
  expectEqual(grouped.buttonSettled.hostQuoteState, "cleared", "button host quote state");
  expectEqual(
    grouped.buttonSettled.memberQuoteState,
    "cleared",
    "button member quote state",
  );
}

if (require.main === module) {
  run()
    .then((result) => console.log(JSON.stringify(result, null, 2)))
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
