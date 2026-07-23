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
const FOCUS_RESTORE_TEXT = "restore focus to submitting composer";
const FOCUS_PRESERVE_TEXT = "preserve focus on the second composer";

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-composer-pending-send-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await installIsolatedLaneFixture(page, {
        globals: [
          "closeLane",
          "submitLaneForm",
          "syncComposerPendingSendState",
        ],
      });
      await page.addScriptTag({
        content: [
          configureGroupedComposer,
          pendingSendSnapshot,
          pendingSendSubmission,
          resolveSuccessfulPendingSend,
          runCrossComposerFocusScenarios,
          runGroupedButtonSubmission,
          runGroupedCloseProtectionScenario,
          runGroupedComposerScenarios,
          runRepeatedKeyboardSubmission,
          waitForNextSend,
          waitForPendingSendCount,
        ]
          .map((helper) => helper.toString())
          .join("\n"),
      });
      const config = {
        failureText: FAILURE_TEXT,
        focusPreserveText: FOCUS_PRESERVE_TEXT,
        focusRestoreText: FOCUS_RESTORE_TEXT,
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
  const sendDispatchEvents = new EventTarget();
  const sends = [];
  liveBusRequest = (type, fields = {}) => {
    if (type !== "lane.send") return originalLiveBusRequest(type, fields);
    return new Promise((resolve) => {
      sends.push({ fields, resolve });
      sendDispatchEvents.dispatchEvent(new Event("send-dispatched"));
    });
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
    const secondSendDispatched = waitForNextSend(sendDispatchEvents);
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
    await secondSendDispatched;
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
    const crossFocus = await runCrossComposerFocusScenarios(sends, config);
    return {
      afterSecondSubmit,
      crossFocus,
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
  const closeProtection = await runGroupedCloseProtectionScenario(
    sends,
    config,
    host,
    member,
    memberTextarea,
  );
  return {
    ...button,
    closeProtection,
    commandPending,
    commandFirstSettled,
    ...repeated,
    hostTargetId: host.targetId,
    memberTargetId: member.targetId,
  };
}

async function runGroupedCloseProtectionScenario(
  sends,
  config,
  host,
  member,
  memberTextarea,
) {
  const teamId = "composer-pending-close-protection";
  host.teamId = teamId;
  member.teamId = teamId;
  const submitMemberDraft = (text) => {
    memberTextarea.value = text;
    memberTextarea.dispatchEvent(new Event("input", { bubbles: true }));
    memberTextarea.dispatchEvent(
      new KeyboardEvent("keydown", {
        bubbles: true,
        cancelable: true,
        key: "Enter",
        metaKey: true,
      }),
    );
  };
  const sendStart = sends.length;
  submitMemberDraft(config.failureText);
  submitMemberDraft(config.queuedFailureText);
  const dispatched = sends.slice(sendStart);
  const warningTexts = [];
  const originalConfirm = window.confirm;
  window.confirm = (text) => {
    warningTexts.push(text);
    return false;
  };
  try {
    closeLane(host);
  } finally {
    window.confirm = originalConfirm;
  }
  const closeCanceled = {
    closeState: host.serverCloseRequested ? "requested" : "canceled",
    hostState: isLaneOpen(host) ? "open" : "closed",
    memberState: isLaneOpen(member) ? "open" : "closed",
    snapshot: pendingSendSnapshot(member, memberTextarea, dispatched.length),
    warningTexts,
  };

  memberTextarea.value = config.newerText;
  memberTextarea.dispatchEvent(new Event("input", { bubbles: true }));
  dispatched[0].resolve({
    result: { ok: false, error: "expected grouped close-protection failure" },
  });
  await waitForPendingSendCount(member, 0);

  return {
    closeCanceled,
    restored: pendingSendSnapshot(member, memberTextarea, sends.length),
  };
}

function configureGroupedComposer(host, member) {
  laneStore.applyLaneGroups([[host.targetId, member.targetId]], { isLaneOpen });
}

// Two independent (ungrouped) composers. A keyboard submit resets and refocuses
// its own textarea before the backend reply. If the user moves to the second
// composer while the send is pending, completion must leave that focus alone.
async function runCrossComposerFocusScenarios(sends, config) {
  const first = resolveIsolatedLane("composer-focus-first");
  const second = resolveIsolatedLane("composer-focus-second");
  syncComposerShards(
    laneGroupHost(first),
    laneGroupMemberLanes(laneGroupHost(first)),
  );
  syncComposerShards(
    laneGroupHost(second),
    laneGroupMemberLanes(laneGroupHost(second)),
  );
  const firstTextarea = first.shardTextareas.get(first.targetId);
  const secondTextarea = second.shardTextareas.get(second.targetId);
  if (!firstTextarea || !secondTextarea)
    throw new Error("cross-composer focus smoke needs both composer textareas");

  const submitByKeyboard = (textarea, text) => {
    textarea.value = text;
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    textarea.focus();
    const start = sends.length;
    textarea.dispatchEvent(
      new KeyboardEvent("keydown", {
        bubbles: true,
        cancelable: true,
        key: "Enter",
        metaKey: true,
      }),
    );
    return sends.slice(start);
  };
  const resolveSends = (pending, keyPrefix, version) =>
    pending.forEach((send, index) =>
      resolveSuccessfulPendingSend(
        send,
        laneStore.laneForId(send.fields.targetId),
        keyPrefix + index,
        send.fields.payload.text,
        version,
      ),
    );
  const focusLabel = () => {
    if (document.activeElement === firstTextarea) return "first";
    if (document.activeElement === secondTextarea) return "second";
    return "blurred";
  };

  const restorePending = submitByKeyboard(firstTextarea, config.focusRestoreText);
  const restorePendingFocus = focusLabel();
  resolveSends(restorePending, "composer-focus-restore-", 5);
  await waitForPendingSendCount(first, 0);
  const restoreSettledFocus = focusLabel();

  const preservePending = submitByKeyboard(
    firstTextarea,
    config.focusPreserveText,
  );
  secondTextarea.focus();
  const preservePendingFocus = focusLabel();
  resolveSends(preservePending, "composer-focus-preserve-", 6);
  await waitForPendingSendCount(first, 0);
  const preserveSettledFocus = focusLabel();

  return {
    firstTargetId: first.targetId,
    preservePendingFocus,
    preserveSends: preservePending.length,
    preserveSettledFocus,
    restorePendingFocus,
    restoreSends: restorePending.length,
    restoreSettledFocus,
    secondTargetId: second.targetId,
  };
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
  const current = () => Math.max(0, Number(lane.pendingSubmissionCount) || 0);
  if (current() === expected) return Promise.resolve(expected);
  const form = laneGroupHost(lane).formEl;
  return new Promise((resolve) => {
    const observer = new MutationObserver(() => {
      if (current() !== expected) return;
      observer.disconnect();
      resolve(expected);
    });
    observer.observe(form, {
      attributeFilter: ["data-pending-send-count"],
      attributes: true,
    });
  });
}

function waitForNextSend(events) {
  return new Promise((resolve) =>
    events.addEventListener("send-dispatched", resolve, { once: true }),
  );
}

function expectEqual(actual, expected, message) {
  if (Object.is(actual, expected)) return;
  throw new Error(message + ": expected " + expected + ", got " + actual);
}

function assertPendingSendScenarios(result, config) {
  assertRapidPendingSendScenario(result, config);
  assertFailedPendingSendScenario(result, config);
  assertCrossComposerFocusScenario(result.crossFocus);
  assertGroupedCommandSendScenario(result.grouped, config);
  assertGroupedButtonSendScenario(result.grouped, config);
  assertGroupedCloseProtectionScenario(result.grouped.closeProtection, config);
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

function assertGroupedCloseProtectionScenario(closeProtection, config) {
  expectEqual(
    closeProtection.closeCanceled.warningTexts.length,
    1,
    "fused close warning count",
  );
  expectEqual(
    closeProtection.closeCanceled.warningTexts[0],
    "Unsubmitted steering has not received a backend key yet. Leave anyway?",
    "fused close warning text",
  );
  expectEqual(
    closeProtection.closeCanceled.hostState,
    "open",
    "canceled fused close host state",
  );
  expectEqual(
    closeProtection.closeCanceled.memberState,
    "open",
    "canceled fused close member state",
  );
  expectEqual(
    closeProtection.closeCanceled.closeState,
    "canceled",
    "canceled fused close request state",
  );
  expectEqual(
    closeProtection.closeCanceled.snapshot.sendCount,
    1,
    "non-host active send count",
  );
  expectEqual(
    closeProtection.closeCanceled.snapshot.pendingCount,
    2,
    "non-host pending and queued count",
  );
  expectEqual(
    closeProtection.closeCanceled.snapshot.queueCount,
    1,
    "non-host queued send count",
  );
  expectEqual(
    closeProtection.closeCanceled.snapshot.text,
    "",
    "non-host detached drafts at close",
  );
  expectEqual(
    closeProtection.restored.text,
    config.failureText +
      "\n\n" +
      config.queuedFailureText +
      "\n\n" +
      config.newerText,
    "fused close retains failed queued and newer drafts in order",
  );
  expectEqual(
    closeProtection.restored.pendingCount,
    0,
    "fused close restored pending count",
  );
  expectEqual(
    closeProtection.restored.queueCount,
    0,
    "fused close restored queue count",
  );
}

function assertCrossComposerFocusScenario(crossFocus) {
  expectEqual(crossFocus.restoreSends, 1, "restore keyboard send count");
  expectEqual(
    crossFocus.restorePendingFocus,
    "first",
    "submitting composer remains focused while pending",
  );
  expectEqual(
    crossFocus.restoreSettledFocus,
    "first",
    "same-composer focus remains after delayed completion",
  );
  expectEqual(crossFocus.preserveSends, 1, "preserve keyboard send count");
  expectEqual(
    crossFocus.preservePendingFocus,
    "second",
    "user focus moved to second composer while pending",
  );
  expectEqual(
    crossFocus.preserveSettledFocus,
    "second",
    "cross-composer focus preserved through delayed completion",
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
