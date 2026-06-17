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
  host.serverLifetime = laneServerLifetime(host);
  host.lifetime = lifetime;
  host.pendingLifetimeCommit = lifetime;
  host.pendingLifetimeConfigRevision = Math.max(
    0,
    Number(host.configRevision) || 0,
  );
  syncLaneEffectiveControls(host);
  renderFilterPills();
  updateTaskDrainForLane(host);
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
  if (options.supersedePending === false) return false;
  const revision = laneLifetimeRevision(options);
  return (
    revision > 0 &&
    revision > Math.max(0, Number(host.pendingLifetimeConfigRevision) || 0)
  );
}

function clearLaneLifetimeCommitState(host) {
  host.pendingLifetimeCommit = "";
  host.pendingLifetimeConfigRevision = 0;
}

function applyServerLaneLifetime(lane, lifetime, options = {}) {
  if (!agentLifetimeLabels.includes(lifetime)) return false;
  const host = laneGroupHost(lane);
  if (host.pendingLifetimeCommit && lifetime !== host.pendingLifetimeCommit) {
    if (!serverLifetimeSupersedesPending(host, options)) return false;
    clearLaneLifetimeCommitState(host);
  }
  if (host.pendingLifetimeCommit === lifetime)
    clearLaneLifetimeCommitState(host);
  const previous = host.lifetime;
  host.serverLifetime = lifetime;
  host.lifetime = lifetime;
  syncLaneEffectiveControls(host);
  return previous !== lifetime;
}

function clearLaneLifetimeCommit(lane, lifetime) {
  const host = laneGroupHost(lane);
  if (host.pendingLifetimeCommit === lifetime) clearLaneLifetimeCommitState(host);
}

function rollbackLaneLifetimeCommit(lane, lifetime, serverLifetime = "") {
  const host = laneGroupHost(lane);
  if (host.pendingLifetimeCommit !== lifetime) return false;
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
