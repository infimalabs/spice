// Global spice menu and target-team drag/drop controls.

const spiceMenuNewTeamDropId = "__new_team_drop__";

// ---- spice context menu -----------------------------------------------------------

function toggleSpiceMenu() {
  if (spiceMenuEl) closeSpiceMenu();
  else openSpiceMenu();
}

function openSpiceMenu() {
  if (spiceMenuEl) {
    positionSpiceMenu();
    return;
  }
  spiceMenuEl = document.createElement("div");
  spiceMenuEl.className = "spice-context-menu";
  spiceMenuEl.setAttribute("role", "menu");
  spiceMenuEl.setAttribute("aria-label", "spice menu");
  document.body.append(spiceMenuEl);
  openLaneButton.setAttribute("aria-expanded", "true");
  spiceMenuPositionHandler = () => positionSpiceMenu();
  spiceMenuDismissHandler = (event) => {
    const target = event.target;
    if (spiceMenuEl && target instanceof Node && spiceMenuEl.contains(target))
      return;
    if (target instanceof Node && openLaneButton.contains(target)) return;
    closeSpiceMenu();
  };
  spiceMenuKeyHandler = (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closeSpiceMenu();
      openLaneButton.focus();
    }
  };
  window.addEventListener("resize", spiceMenuPositionHandler);
  window.addEventListener("scroll", spiceMenuPositionHandler, true);
  document.addEventListener("pointerdown", spiceMenuDismissHandler, true);
  document.addEventListener("keydown", spiceMenuKeyHandler, true);
  renderSpiceMenu();
  positionSpiceMenu();
  refreshTargets().finally(() => {
    renderSpiceMenu();
    positionSpiceMenu();
  });
}

function closeSpiceMenu() {
  if (!spiceMenuEl) return;
  spiceMenuRenderPending = false;
  clearSpiceMenuTargetDrag();
  if (spiceMenuPositionHandler) {
    window.removeEventListener("resize", spiceMenuPositionHandler);
    window.removeEventListener("scroll", spiceMenuPositionHandler, true);
    spiceMenuPositionHandler = null;
  }
  if (spiceMenuDismissHandler) {
    document.removeEventListener("pointerdown", spiceMenuDismissHandler, true);
    spiceMenuDismissHandler = null;
  }
  if (spiceMenuKeyHandler) {
    document.removeEventListener("keydown", spiceMenuKeyHandler, true);
    spiceMenuKeyHandler = null;
  }
  spiceMenuEl.remove();
  spiceMenuEl = null;
  openLaneButton.setAttribute("aria-expanded", "false");
}

function renderSpiceMenu() {
  if (!spiceMenuEl) return;
  if (spiceMenuTargetDragState) {
    spiceMenuRenderPending = true;
    return;
  }
  spiceMenuRenderPending = false;
  clearSpiceMenuTargetDrag();
  spiceMenuEl.replaceChildren(
    renderSpiceMenuActions(),
    renderSpiceMenuTargets(),
    renderSpiceMenuVersion(),
  );
}

function spiceMenuRuntimeVersion() {
  return (
    (typeof spiceServeBranding === "object" &&
      spiceServeBranding &&
      typeof spiceServeBranding.version === "string" &&
      spiceServeBranding.version.trim()) ||
    ""
  );
}

function renderSpiceMenuVersion() {
  const footer = document.createElement("div");
  footer.className = "spice-menu-version";
  const version = spiceMenuRuntimeVersion();
  footer.textContent = version ? "v" + version : "";
  return footer;
}

function flushPendingSpiceMenuRender() {
  if (!spiceMenuRenderPending || !spiceMenuEl || spiceMenuTargetDragState)
    return;
  renderSpiceMenu();
  positionSpiceMenu();
}

function positionSpiceMenu() {
  if (!spiceMenuEl) return;
  const margin = 8;
  const buttonRect = openLaneButton.getBoundingClientRect();
  const viewportHeight =
    window.innerHeight || document.documentElement.clientHeight;
  const viewportWidth = window.innerWidth || document.documentElement.clientWidth;
  const top = Math.max(margin, buttonRect.bottom + margin);
  const width = spiceMenuWidthForButton(buttonRect, viewportWidth, margin);
  const left = spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin);
  const height = Math.max(220, viewportHeight - top - margin);
  spiceMenuEl.style.width = width + "px";
  spiceMenuEl.style.left = left + "px";
  spiceMenuEl.style.top = top + "px";
  spiceMenuEl.style.height = "";
  spiceMenuEl.style.maxHeight = height + "px";
}

function spiceMenuWidthForButton(buttonRect, viewportWidth, margin) {
  if (spiceMenuUsesViewportWidth(viewportWidth)) return viewportWidth;
  const availableWidth = Math.max(1, viewportWidth - margin * 2);
  return Math.min(
    availableWidth,
    Math.max(spiceMenuMinimumLaneWidthPx(), buttonRect.width),
  );
}

function spiceMenuLeftForButton(buttonRect, width, viewportWidth, margin) {
  if (spiceMenuUsesViewportWidth(viewportWidth)) return 0;
  const rightAlignedLeft = buttonRect.right - width;
  return Math.max(
    margin,
    Math.min(rightAlignedLeft, viewportWidth - width - margin),
  );
}

function spiceMenuUsesViewportWidth(viewportWidth) {
  return viewportWidth < spiceMenuMinimumLaneWidthPx() + 20;
}

function spiceMenuMinimumLaneWidthPx() {
  const fontSize =
    Number.parseFloat(window.getComputedStyle(document.documentElement).fontSize) ||
    16;
  return 20 * fontSize;
}

function renderSpiceMenuActions() {
  const section = document.createElement("section");
  section.className = "spice-menu-section";
  const heading = document.createElement("div");
  heading.className = "spice-menu-heading";
  heading.textContent = "global";
  const actions = document.createElement("div");
  actions.className = "spice-menu-actions";
  actions.append(
    renderSpiceMenuAction({
      label: "Fast mode",
      detail: fastModeEnabled ? "on" : "off",
      pressed: fastModeEnabled,
      onClick: () => setFastModeEnabled(!fastModeEnabled),
    }),
  );
  section.append(heading, actions);
  return section;
}

function renderSpiceMenuAction({ label, detail = "", pressed = null, onClick }) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "spice-menu-action";
  button.setAttribute(
    "role",
    pressed === null ? "menuitem" : "menuitemcheckbox",
  );
  if (pressed !== null) button.setAttribute("aria-checked", String(pressed));
  button.innerHTML =
    '<span class="spice-menu-action-label"></span>' +
    '<span class="spice-menu-action-detail"></span>';
  button.querySelector(".spice-menu-action-label").textContent = label;
  button.querySelector(".spice-menu-action-detail").textContent = detail;
  button.addEventListener("click", onClick);
  return button;
}

function renderSpiceMenuTargets() {
  const section = document.createElement("section");
  section.className = "spice-menu-section spice-menu-targets";
  const heading = document.createElement("div");
  heading.className = "spice-menu-heading";
  heading.textContent = "open team";
  const list = document.createElement("div");
  list.className = "spice-menu-target-list";
  if (!targetsLoaded) {
    list.textContent = targetsLoading
      ? "loading teams"
      : "team list unavailable";
  } else {
    const choices = targets
      .slice()
      .sort(compareSpiceMenuTargetChoices);
    const groups = spiceMenuTeamGroups(choices);
    list.replaceChildren(...groups.map(renderSpiceMenuTeamGroup));
    if (!groups.length) list.textContent = "no agents available";
  }
  section.append(heading, list);
  return section;
}

function spiceMenuTeamGroups(choices) {
  const grouped = new Map();
  const unassigned = [];
  for (const target of choices) {
    const teamId = teamIdentityTeamId(target.teamIdentity);
    if (!teamId) {
      unassigned.push(target);
      continue;
    }
    if (!grouped.has(teamId)) {
      grouped.set(teamId, {
        teamId,
        totalCount: targets.filter(
          (item) => teamIdentityTeamId(item.teamIdentity) === teamId,
        ).length,
        targets: [],
        unassigned: false,
      });
    }
    grouped.get(teamId).targets.push(target);
  }
  let groups = [...grouped.values()];
  for (const group of groups) group.targets.sort(compareSpiceMenuTargetChoices);
  groups = placedSpiceMenuTeamGroups(groups);
  unassigned.sort(compareSpiceMenuTargetChoices);
  if (choices.length) {
    groups.push(spiceMenuNewTeamDropGroup());
    groups.push({
      teamId: "",
      totalCount: unassigned.length,
      targets: unassigned,
      unassigned: true,
    });
  }
  return groups;
}

function placedSpiceMenuTeamGroups(groups) {
  const regular = [];
  const placed = [];
  for (const group of groups) {
    const placementIndex = spiceMenuNewTeamPlacementIndex(group);
    if (placementIndex === -1) regular.push(group);
    else placed.push({ group, placementIndex });
  }
  regular.sort(compareSpiceMenuTeamGroups);
  placed.sort(comparePlacedSpiceMenuTeamGroups);
  return [...regular, ...placed.map((item) => item.group)];
}

function comparePlacedSpiceMenuTeamGroups(left, right) {
  if (left.placementIndex !== right.placementIndex)
    return left.placementIndex - right.placementIndex;
  return compareSpiceMenuTeamGroups(left.group, right.group);
}

function spiceMenuNewTeamPlacementIndex(group) {
  const teamId = String(group.teamId || "");
  for (let index = 0; index < spiceMenuNewTeamPlacementHints.length; index++) {
    const hint = spiceMenuNewTeamPlacementHints[index];
    if (hint.teamId && teamId === hint.teamId) return index;
    if (!hint.teamId && teamId === hint.optimisticTeamId) return index;
    if (
      !hint.teamId &&
      group.targets.some((target) => target.id === hint.targetId)
    ) {
      if (teamId && teamId !== hint.optimisticTeamId) hint.teamId = teamId;
      return index;
    }
  }
  return -1;
}

function spiceMenuNewTeamDropGroup() {
  return {
    teamId: spiceMenuNewTeamDropId,
    totalCount: 0,
    targets: [],
    newTeam: true,
    unassigned: false,
  };
}

function compareSpiceMenuTeamGroups(left, right) {
  const byName = spiceMenuTeamSortKey(left).localeCompare(
    spiceMenuTeamSortKey(right),
  );
  if (byName) return byName;
  return String(left.teamId || "").localeCompare(String(right.teamId || ""));
}

function spiceMenuTeamSortKey(group) {
  return group.targets.map(targetChoiceName).join("\n");
}

function compareSpiceMenuTargetChoices(left, right) {
  return compareTargetChoices(left, right);
}

function renderSpiceMenuTeamGroup(group) {
  const container = document.createElement("section");
  container.className = group.unassigned
    ? "spice-menu-team spice-menu-team--unassigned"
    : group.newTeam
      ? "spice-menu-team spice-menu-team--new-team-drop"
      : "spice-menu-team";
  wireSpiceMenuTeamDropTarget(container, group);
  const header = document.createElement("div");
  header.className = "spice-menu-team-header";
  const label = document.createElement("span");
  label.className = "spice-menu-team-label";
  label.textContent = group.unassigned
    ? "agents without team"
    : group.newTeam
      ? "new team"
      : spiceMenuTeamTitle(group);
  const detail = document.createElement("span");
  detail.className = "spice-menu-team-detail";
  detail.textContent = group.unassigned
    ? "drop here to remove from team"
    : group.newTeam
      ? "drop agent to create"
      : spiceMenuTeamDetail(group);
  const choices = document.createElement("div");
  choices.className = "spice-menu-team-targets";
  const targetChoices = group.targets.map((target) =>
    renderTargetChoice(target, group),
  );
  if (group.newTeam) targetChoices.push(spiceMenuNewTeamDropHint());
  if (group.unassigned && !targetChoices.length)
    targetChoices.push(spiceMenuEmptyUnassignedDropHint());
  choices.replaceChildren(...targetChoices);
  header.append(label, detail);
  container.append(header, choices);
  return container;
}

function spiceMenuNewTeamDropHint() {
  const hint = document.createElement("div");
  hint.className = "spice-menu-team-new-drop";
  hint.textContent = "Drop agent here";
  return hint;
}

function spiceMenuEmptyUnassignedDropHint() {
  const hint = document.createElement("div");
  hint.className = "spice-menu-team-empty-drop";
  hint.textContent = "Drop agent here";
  return hint;
}

function spiceMenuTeamTitle(group) {
  const names = group.targets.map(targetChoiceName);
  const visible = names.slice(0, 2).join(" + ");
  const overflow = names.length > 2 ? " +" + (names.length - 2) : "";
  return "team " + visible + overflow;
}

function spiceMenuTeamDetail(group) {
  const count = Math.max(group.totalCount || 0, group.targets.length);
  if (count <= 1) return "opens this team";
  return "open any member; " + count + " agents open together";
}

function setFastModeEnabled(enabled) {
  fastModeEnabled = Boolean(enabled);
  persistFastModeEnabled(fastModeEnabled);
  syncFastModeButtonState();
  renderSpiceMenu();
  configureLiveBusLanes();
  setGlobalTransientStatus(fastModeEnabled ? "fast mode on" : "fast mode off");
}

function syncFastModeButtonState() {
  if (typeof openLaneButton === "undefined" || !openLaneButton) return;
  openLaneButton.classList.toggle("spice-menu-button--fast", fastModeEnabled);
  openLaneButton.title = fastModeEnabled
    ? serveBrandMenuTitle() + " - fast mode on"
    : serveBrandMenuTitle();
}

function defaultTeamConfig() {
  return {
    speechMode: defaultSpeechMode,
    lifetime: defaultAgentLifetime,
    selectedView: defaultLaneViewMode,
  };
}

function renderTargetChoice(target, group = null) {
  const alreadyOpen = laneStates.has(target.id);
  let actionLabel = "Create team";
  if (alreadyOpen) actionLabel = "Show team";
  else if (group && !group.unassigned) actionLabel = "Open team";
  const button = targetChoiceButton(target, actionLabel, () => {
    openTargetTeam(target.id).catch(() => {
      setGlobalTransientError("open team failed");
    });
  });
  button.classList.toggle("target-choice--open", alreadyOpen);
  wireSpiceMenuTargetDrag(button, target);
  return button;
}

function wireSpiceMenuTargetDrag(button, target) {
  button.classList.add("target-choice--draggable");
  button.dataset.spiceMenuDragTargetId = target.id;
  button.style.touchAction = "none";
  button.append(spiceMenuTargetDragAffordance());

  let suppressNextClick = false;

  button.addEventListener("click", (event) => {
    if (suppressNextClick) {
      suppressNextClick = false;
      event.preventDefault();
      event.stopPropagation();
    }
  });

  button.addEventListener("pointerdown", (event) => {
    if (event.button !== undefined && event.button !== 0) return;
    event.preventDefault();
    clearSpiceMenuTargetDrag();
    const state = {
      button,
      targetId: target.id,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      offsetX: event.clientX - button.getBoundingClientRect().left,
      offsetY: event.clientY - button.getBoundingClientRect().top,
      dragging: false,
      dragGhost: null,
      overContainer: null,
      overDesktop: false,
      pointerCleanup: null,
      pointerCaptureFailed: false,
    };
    spiceMenuTargetDragState = state;
    state.pointerCleanup = wireSpiceMenuTargetPointerDocumentEvents(target);
    try {
      button.setPointerCapture(event.pointerId);
    } catch (error) {
      state.pointerCaptureFailed = true;
    }
  });

  button.addEventListener("pointermove", (event) => {
    updateSpiceMenuTargetDragFromEvent(event, target, () => {
      suppressNextClick = true;
    });
  });

  button.addEventListener("pointerup", (event) => {
    finishSpiceMenuTargetDragFromEvent(event, target);
  });

  button.addEventListener("pointercancel", (event) => {
    cancelSpiceMenuTargetDragFromEvent(event, target.id);
  });
}

function wireSpiceMenuTargetPointerDocumentEvents(target) {
  const onMove = (event) => {
    updateSpiceMenuTargetDragFromEvent(event, target);
  };
  const onUp = (event) => {
    finishSpiceMenuTargetDragFromEvent(event, target);
  };
  const onCancel = (event) => {
    cancelSpiceMenuTargetDragFromEvent(event, target.id);
  };
  document.addEventListener("pointermove", onMove);
  document.addEventListener("pointerup", onUp);
  document.addEventListener("pointercancel", onCancel);
  return () => {
    document.removeEventListener("pointermove", onMove);
    document.removeEventListener("pointerup", onUp);
    document.removeEventListener("pointercancel", onCancel);
  };
}

function updateSpiceMenuTargetDragFromEvent(event, target, onStart = null) {
  const state = spiceMenuTargetDragState;
  if (!spiceMenuTargetDragMatches(state, event, target.id)) return;
  if (!state.dragging) {
    const dx = event.clientX - state.startX;
    const dy = event.clientY - state.startY;
    if (Math.abs(dx) < 6 && Math.abs(dy) < 6) return;
    state.dragging = true;
    if (onStart) onStart();
    spiceMenuDragTargetId = target.id;
    state.button?.classList.add("target-choice--dragging");
    state.dragGhost = createSpiceMenuTargetDragGhost(state.button);
  }
  updateSpiceMenuTargetDragGhost(state, event);
  updateSpiceMenuTargetDropTarget(
    state,
    target.id,
    event.clientX,
    event.clientY,
  );
  event.preventDefault();
}

function updateSpiceMenuTargetDropTarget(state, targetId, clientX, clientY) {
  const el = document.elementFromPoint(clientX, clientY);
  const container = /** @type {HTMLElement | null} */ (el?.closest("[data-spice-menu-team-id]") || null);
  if (container !== state.overContainer) {
    state.overContainer?.classList.remove("spice-menu-team--drop-ready");
    state.overContainer = null;
    const teamId = container ? spiceMenuDropTeamId(container) : "";
    if (container && spiceMenuCanDropTargetOnTeamId(teamId, targetId)) {
      container.classList.add("spice-menu-team--drop-ready");
      state.overContainer = container;
    }
  }
  state.overDesktop = spiceMenuDesktopDropTargetFromPoint(clientX, clientY);
  lanesEl.classList.toggle(
    "swimlanes--menu-drop-ready",
    state.overDesktop && !state.overContainer,
  );
}

function finishSpiceMenuTargetDragFromEvent(event, target) {
  const state = spiceMenuTargetDragState;
  if (!spiceMenuTargetDragMatches(state, event, target.id)) return;
  if (state.dragging)
    updateSpiceMenuTargetDropTarget(
      state,
      target.id,
      event.clientX,
      event.clientY,
    );
  let hasMenuDrop = false;
  let menuDropTeamId = "";
  const sourceTarget = targetById.get(target.id) || target;
  let shouldOpenDesktop = false;
  if (state.dragging && state.overContainer) {
    hasMenuDrop = true;
    menuDropTeamId = spiceMenuDropTeamId(
      /** @type {HTMLElement} */ (state.overContainer),
    );
  } else if (state.dragging && state.overDesktop) {
    shouldOpenDesktop = true;
  }
  const suppressClick = state.dragging;
  endMenuTargetDrag(state);
  spiceMenuTargetDragState = null;
  if (suppressClick) suppressNextSpiceMenuDragClick();
  if (hasMenuDrop) {
    moveTargetToMenuTeamOptimisticUi(menuDropTeamId, target.id);
    moveTargetToMenuTeam(menuDropTeamId, target.id, sourceTarget).catch(() => {
      setGlobalTransientError(
        menuDropTeamId === spiceMenuNewTeamDropId
          ? "create team failed"
          : menuDropTeamId
            ? "move to team failed"
            : "remove from team failed",
      );
      refreshServerTopology().catch(() => {});
    });
  } else if (shouldOpenDesktop) {
    flushPendingSpiceMenuRender();
    openTargetTeam(target.id, { keepMenuOpen: true }).catch(() => {
      setGlobalTransientError("open team failed");
    });
  } else {
    flushPendingSpiceMenuRender();
  }
  event.preventDefault();
}

function cancelSpiceMenuTargetDragFromEvent(event, targetId) {
  const state = spiceMenuTargetDragState;
  if (!spiceMenuTargetDragMatches(state, event, targetId)) return;
  const suppressClick = state.dragging;
  endMenuTargetDrag(state);
  spiceMenuTargetDragState = null;
  if (suppressClick) suppressNextSpiceMenuDragClick();
  flushPendingSpiceMenuRender();
  event.preventDefault();
}

function spiceMenuTargetDragMatches(state, event, targetId) {
  return (
    state &&
    state.pointerId === event.pointerId &&
    state.targetId === targetId
  );
}

function endMenuTargetDrag(state) {
  spiceMenuDragTargetId = "";
  state.pointerCleanup?.();
  state.pointerCleanup = null;
  if (state.button && state.pointerId !== undefined) {
    try {
      if (state.button.hasPointerCapture?.(state.pointerId))
        state.button.releasePointerCapture(state.pointerId);
    } catch (error) {
      state.pointerCaptureFailed = true;
    }
  }
  state.button?.classList.remove("target-choice--dragging");
  state.dragGhost?.remove();
  state.dragGhost = null;
  lanesEl.classList.remove("swimlanes--menu-drop-ready");
  clearSpiceMenuTeamDropHighlights();
}

function clearSpiceMenuTargetDrag() {
  if (spiceMenuTargetDragState) {
    const suppressClick = spiceMenuTargetDragState.dragging;
    endMenuTargetDrag(spiceMenuTargetDragState);
    spiceMenuTargetDragState = null;
    if (suppressClick) suppressNextSpiceMenuDragClick();
  }
  for (const ghost of document.querySelectorAll(".target-choice-drag-ghost"))
    ghost.remove();
  for (const choice of document.querySelectorAll(".target-choice--dragging"))
    choice.classList.remove("target-choice--dragging");
}

function suppressNextSpiceMenuDragClick() {
  const onClick = (event) => {
    event.preventDefault();
    event.stopImmediatePropagation();
  };
  document.addEventListener("click", onClick, true);
  window.setTimeout(() => {
    document.removeEventListener("click", onClick, true);
  }, 0);
}

function createSpiceMenuTargetDragGhost(button) {
  const ghost = /** @type {HTMLElement} */ (button.cloneNode(true));
  const rect = button.getBoundingClientRect();
  ghost.classList.remove("target-choice--dragging");
  ghost.classList.add("target-choice-drag-ghost");
  ghost.style.width = rect.width + "px";
  document.body.append(ghost);
  return ghost;
}

function updateSpiceMenuTargetDragGhost(state, event) {
  if (!state.dragGhost) return;
  const left = event.clientX - state.offsetX;
  const top = event.clientY - state.offsetY;
  state.dragGhost.style.transform =
    "translate(" + left + "px, " + top + "px)";
}

function spiceMenuDesktopDropTargetFromPoint(clientX, clientY) {
  const element = document.elementFromPoint(clientX, clientY);
  if (!(element instanceof Element)) return false;
  if (spiceMenuEl?.contains(element)) return false;
  return lanesEl.contains(element);
}

function spiceMenuTargetDragAffordance() {
  const marker = document.createElement("span");
  marker.className = "target-choice-drag-affordance";
  marker.setAttribute("aria-hidden", "true");
  marker.textContent = "↕";
  return marker;
}

function wireSpiceMenuTeamDropTarget(container, group) {
  container.dataset.spiceMenuTeamId = group.teamId;
  container.dataset.spiceMenuUnassigned = group.unassigned ? "true" : "false";
  container.dataset.spiceMenuNewTeam = group.newTeam ? "true" : "false";
}

function clearSpiceMenuTeamDropHighlights() {
  const dropTargets = document.querySelectorAll(".spice-menu-team--drop-ready");
  for (const element of dropTargets)
    element.classList.remove("spice-menu-team--drop-ready");
}

function spiceMenuCanDropTargetOnTeamId(teamId, targetId) {
  if (!targetId) return false;
  const target = targetById.get(targetId);
  if (!target) return false;
  if (teamId === spiceMenuNewTeamDropId) return true;
  return teamIdentityTeamId(target.teamIdentity) !== (teamId || "");
}

function moveTargetToMenuTeamOptimisticUi(teamId, targetId) {
  const target = targetById.get(targetId);
  if (!target) return;
  if (teamIdentityTeamId(target.teamIdentity) === (teamId || "")) return;
  if (teamId === spiceMenuNewTeamDropId)
    rememberSpiceMenuNewTeamPlacement(targetId);
  const teamIdentity =
    teamId === spiceMenuNewTeamDropId
      ? optimisticNewMenuTeamIdentity(targetId)
      : optimisticMenuTeamIdentity(teamId);
  targets = targets.map((item) =>
    item.id === targetId ? { ...item, teamIdentity } : item,
  );
  targetById = new Map(targets.map((item) => [item.id, item]));
  if (spiceMenuEl) renderSpiceMenu();
}

function rememberSpiceMenuNewTeamPlacement(targetId) {
  const id = String(targetId || "");
  if (!id) return;
  spiceMenuNewTeamPlacementHints = spiceMenuNewTeamPlacementHints.filter(
    (hint) => hint.targetId !== id,
  );
  spiceMenuNewTeamPlacementHints.push({
    targetId: id,
    optimisticTeamId: optimisticNewMenuTeamId(id),
    teamId: "",
  });
}

function optimisticNewMenuTeamIdentity(targetId) {
  return {
    state: "member",
    teamId: optimisticNewMenuTeamId(targetId),
    teamRevision: 0,
    configRevision: 0,
  };
}

function optimisticNewMenuTeamId(targetId) {
  return "new-team:" + targetId;
}

function optimisticMenuTeamIdentity(teamId) {
  const id = String(teamId || "");
  if (!id) return { state: "none" };
  for (const target of targets) {
    if (teamIdentityTeamId(target.teamIdentity) === id)
      return { ...target.teamIdentity };
  }
  throw new Error("optimistic menu team identity requires existing team");
}

async function moveTargetToMenuTeam(teamId, targetId, sourceTarget = null) {
  const target = sourceTarget || targetById.get(targetId);
  if (!target) throw new Error("move target requires target");
  if (teamId === spiceMenuNewTeamDropId) {
    await requestTeamCommand(
      teamCommandPayload("createTeam", {
        members: [targetTeamAgentId(target)],
        config: defaultTeamConfig(),
      }),
    );
  } else if (teamId) {
    await requestTeamCommand(
      teamCommandPayload("moveAgentToTeam", {
        teamId,
        agentId: targetTeamAgentId(target),
        agentAliases: targetTeamAgentAliases(target),
      }),
    );
  } else {
    const currentTeamId = teamIdentityTeamId(target.teamIdentity);
    if (!currentTeamId) throw new Error("remove target requires current team");
    await requestTeamCommand(
      teamCommandPayload("removeAgentFromTeam", {
        teamId: currentTeamId,
        agentId: targetTeamAgentId(target),
        agentAliases: targetTeamAgentAliases(target),
      }),
    );
  }
  await refreshServerTopology();
  setGlobalTransientStatus(
    teamId === spiceMenuNewTeamDropId
      ? "new team created"
      : teamId
        ? "team updated"
        : "agent removed from team",
  );
}

function spiceMenuDropTeamId(container) {
  if (container.dataset.spiceMenuNewTeam === "true")
    return spiceMenuNewTeamDropId;
  if (container.dataset.spiceMenuUnassigned === "true") return "";
  return container.dataset.spiceMenuTeamId || "";
}
