// Lane controls: speech/lifetime state and form submission.

// ---- sliders / lifetime / speech --------------------------------------------------

function laneEffectiveLifetime(lane) {
  const host = laneGroupHost(lane);
  return agentLifetimeLabels.includes(host.lifetime)
    ? host.lifetime
    : defaultAgentLifetime;
}

function laneEffectiveSpeechMode(lane) {
  const host = laneGroupHost(lane);
  return speechModes.includes(host.speechMode)
    ? host.speechMode
    : defaultSpeechMode;
}

function setLaneSpeechMode(lane, mode) {
  const host = laneGroupHost(lane);
  host.speechMode = speechModes.includes(mode) ? mode : defaultSpeechMode;
  persistLaneHints();
  syncLaneEffectiveControls(host);
  if (host.speechMode === "quiet") abortLaneSpeech(host);
}

function setLaneLifetime(lane, label) {
  const host = laneGroupHost(lane);
  const lifetime = agentLifetimeLabels.includes(label) ? label : defaultAgentLifetime;
  host.lifetimeRequestId = Math.max(0, Number(host.lifetimeRequestId) || 0) + 1;
  host.serverLifetime = laneServerLifetime(host);
  host.lifetime = lifetime;
  host.pendingLifetimeCommit = lifetime;
  host.pendingLifetimeRequestId = host.lifetimeRequestId;
  host.pendingLifetimeConfigRevision = Math.max(
    0,
    Number(host.configRevision) || 0,
  );
  syncLaneEffectiveControls(host);
  renderFilterPills();
  updateLaneLifetimeForLane(host);
}

function laneServerLifetime(lane) {
  const host = laneGroupHost(lane);
  return agentLifetimeLabels.includes(host.serverLifetime)
    ? host.serverLifetime
    : agentLifetimeLabels.includes(host.lifetime)
      ? host.lifetime
      : defaultAgentLifetime;
}

function laneLifetimeRevision(options = {}) {
  return Math.max(0, Number(options.configRevision) || 0);
}

function serverLifetimeSupersedesPending(host, options = {}) {
  if (options.force) return true;
  if (options.supersedePending !== true) return false;
  const revision = laneLifetimeRevision(options);
  return (
    revision > 0 &&
    revision > Math.max(0, Number(host.pendingLifetimeConfigRevision) || 0)
  );
}

function serverLifetimeSettlesPending(host, lifetime, options = {}) {
  if (host.pendingLifetimeCommit !== lifetime) return false;
  const requestId = Math.max(0, Number(options.requestId) || 0);
  if (requestId) return host.pendingLifetimeRequestId === requestId;
  const revision = laneLifetimeRevision(options);
  return (
    revision > 0 &&
    revision > Math.max(0, Number(host.pendingLifetimeConfigRevision) || 0)
  );
}

function clearLaneLifetimeCommitState(host) {
  host.pendingLifetimeCommit = "";
  host.pendingLifetimeConfigRevision = 0;
  host.pendingLifetimeRequestId = 0;
}

function updateLaneLifetimeForLane(lane) {
  const host = laneGroupHost(lane);
  if (host.emptyTeam && host.teamId) {
    updateEmptyTeamLifetimeForLane(host);
    return;
  }
  updateTaskDrainForLane(host);
}

function updateEmptyTeamLifetimeForLane(host) {
  const requestedLifetime = laneEffectiveLifetime(host);
  const requestId = Math.max(0, Number(host.pendingLifetimeRequestId) || 0);
  requestTeamCommand(
    teamCommandPayload("updateTeamConfig", {
      teamId: host.teamId,
      configPatch: { lifetime: requestedLifetime },
    }),
  )
    .then(() => {
      if (!laneLifetimeCommitMatches(host, requestedLifetime, { requestId }))
        return;
      clearLaneLifetimeCommitState(host);
      host.serverLifetime = requestedLifetime;
      syncLaneEffectiveControls(host);
    })
    .catch(() => {
      rollbackLaneLifetimeCommit(host, requestedLifetime, "", { requestId });
      setLaneTransientStatus(host, "lifetime update failed");
    });
}

function applyServerLaneLifetime(lane, lifetime, options = {}) {
  if (!agentLifetimeLabels.includes(lifetime)) return false;
  const host = laneGroupHost(lane);
  if (host.pendingLifetimeCommit && lifetime !== host.pendingLifetimeCommit) {
    if (!serverLifetimeSupersedesPending(host, options)) return false;
    clearLaneLifetimeCommitState(host);
  } else if (serverLifetimeSettlesPending(host, lifetime, options)) {
    clearLaneLifetimeCommitState(host);
  }
  const previous = host.lifetime;
  host.serverLifetime = lifetime;
  host.lifetime = lifetime;
  syncLaneEffectiveControls(host);
  return previous !== lifetime;
}

function laneLifetimeCommitMatches(host, lifetime, options = {}) {
  const requestId = Math.max(0, Number(options.requestId) || 0);
  return (
    host.pendingLifetimeCommit === lifetime &&
    (!requestId || host.pendingLifetimeRequestId === requestId)
  );
}

function clearLaneLifetimeCommit(lane, lifetime, options = {}) {
  const host = laneGroupHost(lane);
  if (laneLifetimeCommitMatches(host, lifetime, options))
    clearLaneLifetimeCommitState(host);
}

function rollbackLaneLifetimeCommit(
  lane,
  lifetime,
  serverLifetime = "",
  options = {},
) {
  const host = laneGroupHost(lane);
  if (!laneLifetimeCommitMatches(host, lifetime, options)) return false;
  const authoritativeLifetime = agentLifetimeLabels.includes(serverLifetime)
    ? serverLifetime
    : laneServerLifetime(host);
  clearLaneLifetimeCommitState(host);
  return applyServerLaneLifetime(host, authoritativeLifetime, { force: true });
}

function laneLifetimeRuntimeState(lane) {
  const host = laneGroupHost(lane);
  return {
    lifetime: host.lifetime,
    serverLifetime: laneServerLifetime(host),
    pendingLifetimeCommit: host.pendingLifetimeCommit || "",
    pendingLifetimeRequestId: Math.max(
      0,
      Number(host.pendingLifetimeRequestId) || 0,
    ),
    lifetimeRequestId: Math.max(0, Number(host.lifetimeRequestId) || 0),
    pendingLifetimeConfigRevision: Math.max(
      0,
      Number(host.pendingLifetimeConfigRevision) || 0,
    ),
  };
}

function restoreLaneLifetimeRuntimeState(lane, state) {
  if (!state) return;
  const host = laneGroupHost(lane);
  host.lifetime = agentLifetimeLabels.includes(state.lifetime)
    ? state.lifetime
    : defaultAgentLifetime;
  host.serverLifetime = agentLifetimeLabels.includes(state.serverLifetime)
    ? state.serverLifetime
    : host.lifetime;
  host.pendingLifetimeCommit = agentLifetimeLabels.includes(
    state.pendingLifetimeCommit,
  )
    ? state.pendingLifetimeCommit
    : "";
  host.pendingLifetimeRequestId = Math.max(
    0,
    Number(state.pendingLifetimeRequestId) || 0,
  );
  host.lifetimeRequestId = Math.max(0, Number(state.lifetimeRequestId) || 0);
  host.pendingLifetimeConfigRevision = Math.max(
    0,
    Number(state.pendingLifetimeConfigRevision) || 0,
  );
}

function syncLaneEffectiveControls(lane) {
  const speechMode = laneEffectiveSpeechMode(lane);
  const lifetime = laneEffectiveLifetime(lane);
  lane.speechRangeEl.value = String(speechModes.indexOf(speechMode));
  syncStackSliderState(lane.speechRangeEl);
  lane.speechLabelEl.textContent =
    speechMode.charAt(0).toUpperCase() + speechMode.slice(1);
  lane.lifetimeRangeEl.value = String(agentLifetimeLabels.indexOf(lifetime));
  const lifetimeAccentState = syncStackSliderState(lane.lifetimeRangeEl);
  const lifetimeHelp = agentLifetimeHelpText(lifetime);
  lane.lifetimeRangeEl.title = lifetimeHelp;
  lane.lifetimeRangeEl.setAttribute(
    "aria-label",
    "Task subscription policy: " + lifetimeHelp,
  );
  lane.lifetimeLabelEl.textContent = lifetime;
  lane.lifetimeLabelEl.title = lifetimeHelp;
  lane.submitEl.textContent = lifetime;
  lane.submitEl.title = "Send with " + lifetime + ": " + lifetimeHelp;
  syncSubmitActionState(lane.submitEl, lifetimeAccentState);
  renderLaneViewShell(lane);
  syncNarrationMediaSession();
}

function syncStackSliderState(input) {
  const wrap = input.closest(".stack-slider");
  const state = controlAccentStateForRange(input);
  if (!wrap) return state;
  wrap.classList.toggle("stack-slider--armed", state === "armed");
  wrap.classList.toggle("stack-slider--maxed", state === "maxed");
  return state;
}

function controlAccentStateForRange(input) {
  const value = Number(input.value);
  const min = Number(input.min || 0);
  const max = Number(input.max || 0);
  if (value <= min) return "armed";
  if (value >= max && value > min) return "maxed";
  return "default";
}

function syncSubmitActionState(button, state) {
  button.classList.toggle("submit-action--armed", state === "armed");
  button.classList.toggle("submit-action--maxed", state === "maxed");
}

// ---- submit ----------------------------------------------------------------------------

function submitLaneForm(lane, event, targetId = "") {
  event.preventDefault();
  const host = laneGroupHost(lane);
  const lifetime = laneEffectiveLifetime(host);
  let submitted = false;
  const targetEntries = targetId
    ? [[targetId, host.shardTextareas.get(targetId)]]
    : host.shardTextareas;
  for (const [submitTargetId, textarea] of targetEntries) {
    if (!textarea) continue;
    const member = laneStates.get(submitTargetId);
    if (!member || !isLaneOpen(member)) continue;
    const text = laneComposerSubmissionText(
      host,
      submitTargetId,
      textarea.value,
    );
    const attachments = laneComposerAttachmentPayloads(host, submitTargetId);
    if (!text) continue;
    const focusAfterReset = keyboardSubmitFocusTarget(
      host,
      event,
      submitTargetId,
    );
    enqueueSend(
      member,
      {
        text,
        lifetime,
        fastMode: fastModeEnabled,
        threadId: member.targetThreadId || "",
        teamId: member.teamId || "",
        teamRevision: member.teamRevision || 0,
        configRevision: member.configRevision || 0,
        attachments,
      },
      host,
      { focusAfterReset },
    );
    submitted = true;
  }
  if (!submitted) setLaneTransientStatus(host, "Message text is required.");
}

function keyboardSubmitFocusTarget(host, event, targetId) {
  if (event.type !== "keydown") return null;
  const target = event.target;
  if (!(target instanceof HTMLTextAreaElement)) return null;
  if (!target.dataset.quoteDraftId) return null;
  const textarea = host.shardTextareas.get(targetId);
  if (!textarea) throw new Error("keyboard quote submit requires main composer");
  return textarea;
}
