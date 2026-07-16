// Keyed submission lifecycle state. The server emits cumulative accepted,
// received, and completed stages; reconnect payloads can reconstruct the same
// progression from retained assistant messages carrying ack_keys.

const laneSubmissionLifecycleLimit = 50;
const laneSubmissionStageRanks = Object.freeze({
  accepted: 1,
  received: 2,
  completed: 3,
});

function applyLaneSubmissionLifecycle(lane, rawSubmission) {
  if (!(lane.submissionLifecycleByKey instanceof Map))
    throw new Error("lane submission lifecycle map is required");
  const incoming = normalizedLaneSubmission(rawSubmission);
  const current = lane.submissionLifecycleByKey.get(incoming.key) || null;
  const currentRank = current ? laneSubmissionStageRank(current.stage) : 0;
  const incomingRank = laneSubmissionStageRank(incoming.stage);
  const next = {
    key: incoming.key,
    stage: currentRank > incomingRank ? current.stage : incoming.stage,
    disposition: incoming.disposition || (current ? current.disposition : ""),
    stages: {
      ...((current && current.stages) || {}),
      ...incoming.stages,
    },
    durationsMs: {
      ...((current && current.durationsMs) || {}),
      ...incoming.durationsMs,
    },
  };
  lane.submissionLifecycleByKey.set(next.key, next);
  while (lane.submissionLifecycleByKey.size > laneSubmissionLifecycleLimit) {
    const oldest = lane.submissionLifecycleByKey.keys().next().value;
    lane.submissionLifecycleByKey.delete(oldest);
  }
  return !current || JSON.stringify(current) !== JSON.stringify(next);
}

function normalizedLaneSubmission(rawSubmission) {
  if (!rawSubmission || typeof rawSubmission !== "object")
    throw new Error("lane submission payload is required");
  const key = String(rawSubmission.key || "").trim();
  if (!key) throw new Error("lane submission key is required");
  const stage = String(rawSubmission.stage || "");
  laneSubmissionStageRank(stage);
  const rawStages = rawSubmission.stages;
  if (!rawStages || typeof rawStages !== "object")
    throw new Error("lane submission stages are required");
  const stages = {};
  for (const name of Object.keys(laneSubmissionStageRanks)) {
    if (!Object.prototype.hasOwnProperty.call(rawStages, name)) continue;
    stages[name] = normalizedLaneSubmissionStage(rawStages[name], name);
  }
  if (!stages[stage]) throw new Error("lane submission current stage is required");
  const durationsMs = rawSubmission.durationsMs || {};
  if (typeof durationsMs !== "object")
    throw new Error("lane submission durations must be an object");
  return {
    key,
    stage,
    disposition: String(rawSubmission.disposition || ""),
    stages,
    durationsMs: { ...durationsMs },
  };
}

function normalizedLaneSubmissionStage(rawStage, name) {
  if (!rawStage || typeof rawStage !== "object")
    throw new Error("lane submission " + name + " stage is required");
  const at = String(rawStage.at || "");
  const source = String(rawStage.source || "");
  const evidence = String(rawStage.evidence || "");
  if (!at || !source || !evidence)
    throw new Error("lane submission " + name + " stage is incomplete");
  const stage = { at, source, evidence };
  if (rawStage.sourceAt) stage.sourceAt = String(rawStage.sourceAt);
  return stage;
}

function laneSubmissionStageRank(stage) {
  const rank = laneSubmissionStageRanks[String(stage || "")];
  if (!rank) throw new Error("unknown lane submission stage: " + stage);
  return rank;
}

function reconcileLaneSubmissionMessages(lane, messages) {
  if (!(lane.submissionLifecycleByKey instanceof Map))
    throw new Error("lane submission lifecycle map is required");
  const ordered = [...(messages || [])]
    .filter((message) => message && typeof message === "object")
    .sort(compareLaneSubmissionMessages);
  let changed = false;
  for (const submission of [...lane.submissionLifecycleByKey.values()]) {
    if (submission.stage === "completed") continue;
    const receipt = ordered.find((message) =>
      laneSubmissionMessageHasKey(message, submission.key),
    );
    if (!receipt) continue;
    const completion = laneSubmissionCompletionMessage(ordered, receipt);
    const stages = {
      received: laneSubmissionStageFromMessage(receipt),
    };
    if (completion) stages.completed = laneSubmissionStageFromMessage(completion);
    changed =
      applyLaneSubmissionLifecycle(lane, {
        key: submission.key,
        stage: completion ? "completed" : "received",
        disposition: laneSubmissionMessageDisposition(receipt, submission.key),
        stages,
        durationsMs: {},
      }) || changed;
  }
  return changed;
}

function laneSubmissionMessageHasKey(message, key) {
  return Array.isArray(message.ack_keys) && message.ack_keys.includes(key);
}

function laneSubmissionMessageDisposition(message, key) {
  return Array.isArray(message.nack_keys) && message.nack_keys.includes(key)
    ? "refused"
    : "acked";
}

function laneSubmissionCompletionMessage(messages, receipt) {
  if (laneSubmissionMessageIsCompletion(receipt)) return receipt;
  return (
    messages.find(
      (message) =>
        laneSubmissionMessageIsCompletion(message) &&
        laneSubmissionMessageFollows(message, receipt),
    ) || null
  );
}

function laneSubmissionMessageIsCompletion(message) {
  return message.kind === "final" || message.kind === "reply";
}

function laneSubmissionMessageFollows(message, receipt) {
  const messageIndex = Number(message.index);
  const receiptIndex = Number(receipt.index);
  if (Number.isFinite(messageIndex) && Number.isFinite(receiptIndex))
    return messageIndex > receiptIndex;
  return Date.parse(message.timestamp || "") > Date.parse(receipt.timestamp || "");
}

function compareLaneSubmissionMessages(left, right) {
  const leftIndex = Number(left.index);
  const rightIndex = Number(right.index);
  if (Number.isFinite(leftIndex) && Number.isFinite(rightIndex))
    return leftIndex - rightIndex;
  return Date.parse(left.timestamp || "") - Date.parse(right.timestamp || "");
}

function laneSubmissionStageFromMessage(message) {
  const at = String(message.timestamp || "");
  const evidence = String(message.key || "");
  if (!at || !evidence) throw new Error("submission message identity is required");
  return {
    at,
    source: message.kind === "reply" ? "reply-log" : "transcript",
    evidence,
    sourceAt: at,
  };
}

function syncLaneSubmissionLifecycleUi(lane) {
  const host = laneGroupHost(lane);
  syncComposerShards(host, laneGroupMemberLanes(host));
  renderLaneViewShell(host);
  syncComposerPlaceholders(host);
}
