// ---- composer shards ---------------------------------------------------------------

// A fused host owns one composer with one shard per member target; the shard
// is the concrete send address. Standalone lanes have exactly one shard.
function syncComposerShards(lane, members) {
  const wanted = members.length ? members : [lane];
  const wantedTargetIds = wanted.map((member) => member.targetId);
  const liveTargetIds = new Set(wantedTargetIds);
  pruneComposerQuoteDrafts(lane, wantedTargetIds);
  pruneComposerAttachments(lane, wantedTargetIds);
  for (const targetId of lane.shardTextareas.keys()) {
    if (!liveTargetIds.has(targetId)) lane.shardTextareas.delete(targetId);
  }
  const shards = wanted.map((member) => {
    let shard = composerShardElementForTarget(lane, member.targetId);
    if (!shard) shard = createComposerShardElement(member.targetId);
    syncComposerShard(lane, shard, member);
    return shard;
  });
  syncComposerShardOrder(lane.shardsEl, shards);
  syncLanePaneMetrics(lane);
}

function composerShardElementForTarget(lane, targetId) {
  for (const child of lane.shardsEl.children) {
    if (child.dataset && child.dataset.shardTargetId === targetId)
      return child;
  }
  return null;
}

function createComposerShardElement(targetId) {
  const shard = document.createElement("div");
  shard.className = "composer-shard";
  shard.dataset.shardTargetId = targetId;
  const quoteStack = document.createElement("div");
  quoteStack.className = "composer-quote-stack";
  quoteStack.dataset.composerQuoteStackTargetId = targetId;
  const primary = document.createElement("div");
  primary.className = "composer-band composer-band--primary";
  primary.dataset.composerPrimaryTargetId = targetId;
  shard.append(quoteStack, primary);
  return shard;
}

function syncComposerShard(lane, shard, member) {
  shard.className = "composer-shard";
  shard.dataset.shardTargetId = member.targetId;
  const quoteStack = composerShardQuoteStack(shard, member.targetId);
  syncComposerQuoteStack(lane, quoteStack, member.targetId);
  const primary = composerShardPrimaryBand(shard, member.targetId);
  primary.className = "composer-band composer-band--primary";
  primary.dataset.composerPrimaryTargetId = member.targetId;
  syncComposerBandAccent(primary, lane, member);
  syncComposerDriverIcon(primary, member);
  const header = composerPrimaryBandHeader(lane, member);
  const previousHeader = primary.querySelector(".composer-band-header--primary");
  if (previousHeader) previousHeader.replaceWith(header);
  else primary.prepend(header);
  syncComposerBandMenuState(primary);
  let textarea = primary.querySelector("textarea");
  syncComposerAttachmentStrip(primary, lane, member.targetId, textarea);
  if (!textarea) {
    textarea = createComposerPrimaryTextarea(lane, member.targetId);
    primary.append(textarea);
  }
  textarea.placeholder = laneComposePlaceholder(member);
  lane.shardTextareas.set(member.targetId, textarea);
}

function syncComposerBandAccent(band, lane, member) {
  band.style.setProperty("--composer-header-accent", composerMemberAccent(lane, member));
}

function composerMemberAccent(lane, member) {
  return messageOccupantAccent(laneMemberAccentIndex(lane, member));
}

function syncComposerDriverIcon(band, member) {
  const icon = composerDriverIcon(member);
  const existing = band.querySelector("[data-composer-driver-icon]");
  if (!icon) {
    existing?.remove();
    return;
  }
  if (existing) existing.replaceWith(icon);
  else band.append(icon);
}

function composerDriverIcon(member) {
  const driver = String((member || {}).driverName || "")
    .trim()
    .toLowerCase();
  const iconPaths = {
    claude: "/static/icons/claude.svg",
    codex: "/static/icons/openai.svg",
    openai: "/static/icons/openai.svg",
  };
  const src = iconPaths[driver];
  if (!src) return null;
  const tooltip = composerDriverTooltip(member, driver);
  const icon = document.createElement("span");
  icon.className = "composer-driver-icon composer-driver-icon--" + driver;
  icon.dataset.composerDriverIcon = driver;
  icon.title = tooltip;
  icon.setAttribute("aria-label", tooltip);
  icon.setAttribute("role", "img");
  icon.style.setProperty("--composer-driver-icon-url", 'url("' + src + '")');
  return icon;
}

function composerDriverTooltip(member, driver) {
  const labels = {
    claude: "Claude driver",
    codex: "Codex driver",
    openai: "OpenAI driver",
  };
  const parts = [labels[driver] || "Agent driver"];
  const model = String((member || {}).driverModel || "").trim();
  const effort = String((member || {}).driverEffort || "").trim();
  const threadId = String((member || {}).targetThreadId || "").trim();
  if (model) parts.push("model: " + model);
  if (effort) parts.push("effort: " + effort);
  parts.push("thread: " + (threadId || "unbound"));
  parts.push("source: worktree launch config");
  return parts.join("; ");
}

function composerShardQuoteStack(shard, targetId) {
  let quoteStack = shard.querySelector("[data-composer-quote-stack-target-id]");
  if (!quoteStack) {
    quoteStack = document.createElement("div");
    quoteStack.className = "composer-quote-stack";
    shard.prepend(quoteStack);
  }
  quoteStack.dataset.composerQuoteStackTargetId = targetId;
  return quoteStack;
}

function composerShardPrimaryBand(shard, targetId) {
  let primary = shard.querySelector("[data-composer-primary-target-id]");
  if (!primary) {
    primary = document.createElement("div");
    shard.append(primary);
  }
  primary.dataset.composerPrimaryTargetId = targetId;
  return primary;
}

function composerPrimaryBandHeader(lane, member) {
  const label = laneMemberTargetLabel(member);
  const latest = latestComposerMessage(member);
  const header = composerBandHeader({
    className: "composer-band-header--primary",
    title: composerPrimaryHeaderTitle(latest),
    beforeMenu: composerPrimaryHeaderBeforeMenu(latest, member),
    trailingControl: composerBandMenuTrigger(
      "Composer actions for " + label,
      "Composer actions for " + label,
      () => composerPrimaryMenuActions(lane, member, label),
    ),
  });
  header.title = "Drag composer to move this agent to another lane";
  if (typeof wireComposerMoveDrag === "function")
    wireComposerMoveDrag(lane, header, member.targetId);
  return header;
}

function composerPrimaryMenuActions(lane, member, label) {
  const create = composerBandMenuAction(
    "Create new team",
    "Move only " + label + " to a new team",
  );
  create.disabled = laneGroupMemberLanes(laneGroupHost(lane)).length < 2;
  create.onClick = () => splitComposerAgentFromTeam(lane, member.targetId);

  const leave = composerBandMenuAction(
    "Leave all teams",
    "Remove " + label + " from all teams",
  );
  leave.onClick = () => removeComposerAgentFromTeam(lane, member.targetId);

  const renew = composerBandMenuAction(
    "Renew this agent",
    composerRenewalActionDetail(member),
  );
  renew.pressed = composerRenewalIntentRequested(member);
  renew.disabled = composerRenewalIntentInFlight(member);
  renew.keepOpen = true;
  renew.onClick = (requested) =>
    toggleComposerAgentRenewalIntent(lane, member, requested);

  return [create, leave, renew];
}

function composerBandMenuAction(label, detail) {
  const action = {};
  action.label = label;
  action.detail = detail;
  return action;
}

function composerPrimaryHeaderTitle(latest) {
  return latest ? composerQuotePreview(latest) : "No assistant messages yet";
}

function composerPrimaryHeaderBeforeMenu(latest, member) {
  return [
    latest
      ? composerPrimaryLatestMessageLink(latest, member)
      : composerPrimaryLatestMessageNote(member),
  ];
}

function composerPrimaryLatestMessageLink(latest, member) {
  const time = document.createElement("a");
  time.href = "#" + messageDomId(latest.key);
  time.title = "Jump to latest message";
  time.className = "composer-quote-time composer-latest-time";
  time.dataset.relativeTimestamp = latest.timestamp || "";
  time.dataset.relativeFallback = "message";
  syncComposerHeaderStatus(time, member);
  setRelativeTimeText(time);
  return time;
}

function composerPrimaryLatestMessageNote(member) {
  const note = document.createElement("span");
  note.className = "composer-quote-time composer-latest-time composer-latest-time--empty";
  note.textContent = "no messages";
  note.title = "No latest message";
  syncComposerHeaderStatus(note, member);
  return note;
}

function syncComposerHeaderStatus(element, member) {
  const statusLine = member.lastRenderedStatusLine || {};
  element.dataset.agentStatus =
    statusLine.agentVisualStatus || statusLine.agentProcessStatus || "unknown";
}

function latestComposerMessage(member) {
  return member.knownMessages.find(isComposerLatestMessage);
}

function isComposerLatestMessage(item) {
  return item.kind === "assistant" || item.kind === "final";
}

function createComposerPrimaryTextarea(lane, targetId) {
  const textarea = document.createElement("textarea");
  textarea.rows = 3;
  textarea.addEventListener("focus", () => expandLanePane(lane));
  textarea.addEventListener("input", () => expandLanePane(lane));
  wireComposerAttachmentIngress(textarea, lane, targetId);
  textarea.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      submitLaneForm(lane, event, targetId);
    }
  });
  return textarea;
}

function syncComposerShardOrder(container, shards) {
  const wanted = new Set(shards);
  for (const child of [...container.children]) {
    if (!wanted.has(child)) child.remove();
  }
  let cursor = container.firstElementChild;
  for (const shard of shards) {
    if (shard === cursor) {
      cursor = cursor.nextElementSibling;
      continue;
    }
    container.insertBefore(shard, cursor);
  }
}

function laneComposePlaceholder(member) {
  const label = laneMemberTargetLabel(member);
  const status = laneComposePlaceholderStatus(member);
  return [label, status].filter(Boolean).join("\n");
}

function laneComposePlaceholderStatus(member) {
  const parts = [];
  const pending = lanePendingDisplayCount(member);
  parts.push(pending + " pending");
  const status = (member.lastRenderedStatusLine || {}).agentProcessStatus || "";
  if (status) parts.push(status);
  return parts.join(", ");
}

function syncComposerPlaceholders(lane) {
  for (const [targetId, textarea] of lane.shardTextareas) {
    const member = laneStates.get(targetId) || lane;
    textarea.placeholder = laneComposePlaceholder(member);
  }
  for (const stack of lane.element.querySelectorAll(
    "[data-composer-quote-stack-target-id]",
  )) {
    const targetId = stack.dataset.composerQuoteStackTargetId || "";
    const member = laneStates.get(targetId) || lane;
    for (const textarea of stack.querySelectorAll("textarea[data-quote-draft-id]")) {
      textarea.placeholder = laneComposePlaceholder(member);
    }
  }
}

function laneComposerDraftText(lane) {
  const host = laneGroupHost(lane);
  let text = "";
  for (const textarea of host.shardTextareas.values()) text += textarea.value;
  for (const attachments of host.shardAttachments.values()) {
    if (attachments.length) text += " attachment";
  }
  for (const drafts of host.quoteDrafts.values()) {
    for (const draft of drafts) text += (draft.quoteText || "") + (draft.text || "");
  }
  return text;
}

function laneComposerTargetDraftText(lane, targetId) {
  const host = laneGroupHost(lane);
  let text = host.shardTextareas.get(targetId)?.value || "";
  if (composerAttachmentDraftsForTarget(host, targetId).length) text += " attachment";
  for (const draft of composerQuoteDraftsForTarget(host, targetId)) {
    text += (draft.quoteText || "") + (draft.text || "");
  }
  return text;
}

function resetLaneComposerDraft(lane, targetId) {
  const host = laneGroupHost(lane);
  const textarea = host.shardTextareas.get(targetId);
  if (textarea) textarea.value = "";
  if (host.shardAttachments.delete(targetId)) renderComposerAttachmentStrips(host);
  if (host.quoteDrafts.delete(targetId)) renderComposerQuoteBands(host);
}

function composerAttachmentStrip(lane, targetId) {
  const wrap = document.createElement("div");
  wrap.className = "composer-attachments";
  wrap.dataset.composerAttachmentsTargetId = targetId;
  fillComposerAttachmentStrip(wrap, lane, targetId);
  return wrap;
}

function syncComposerAttachmentStrip(parent, lane, targetId, beforeNode) {
  let wrap = parent.querySelector(
    "[data-composer-attachments-target-id]",
  );
  if (!wrap) {
    wrap = composerAttachmentStrip(lane, targetId);
  }
  wrap.dataset.composerAttachmentsTargetId = targetId;
  fillComposerAttachmentStrip(wrap, lane, targetId);
  const body = parent.querySelector(".composer-band-body");
  if (body) {
    if (wrap.parentElement !== body) body.append(wrap);
    fillComposerAttachmentStrip(wrap, lane, targetId);
    return;
  }
  if (!wrap.parentElement) parent.insertBefore(wrap, beforeNode || null);
  else if (beforeNode && wrap.nextElementSibling !== beforeNode)
    parent.insertBefore(wrap, beforeNode);
}

function fillComposerAttachmentStrip(wrap, lane, targetId) {
  const attachments = composerAttachmentDraftsForTarget(lane, targetId);
  if (attachments.length)
    wrap.style.setProperty("--composer-attachment-count", String(attachments.length));
  else wrap.style.removeProperty("--composer-attachment-count");
  wrap.hidden = attachments.length === 0;
  wrap
    .closest(".composer-band-body")
    ?.classList.toggle("composer-band-body--attachments", attachments.length > 0);
  wrap
    .closest(".composer-band-header")
    ?.classList.toggle("composer-band-header--attachments", attachments.length > 0);
  if (!attachments.length) {
    wrap.replaceChildren();
    return;
  }
  const list = document.createElement("div");
  list.className = "composer-attachment-list";
  for (const attachment of attachments) {
    const chip = document.createElement("span");
    chip.className = "composer-attachment-chip";
    chip.title = attachment.name;
    const img = document.createElement("img");
    img.src = attachment.dataUrl;
    img.alt = attachment.name;
    const label = document.createElement("span");
    label.className = "composer-attachment-name";
    label.textContent = attachment.name || "image";
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "composer-attachment-remove";
    remove.title = "Remove image";
    remove.setAttribute("aria-label", "Remove image");
    remove.textContent = "×";
    remove.addEventListener("click", () =>
      removeComposerAttachment(lane, targetId, attachment.id),
    );
    chip.append(img, label, remove);
    list.append(chip);
  }
  wrap.replaceChildren(list);
}

function composerAttachmentDraftsForTarget(lane, targetId) {
  return lane.shardAttachments.get(targetId) || [];
}

function renderComposerAttachmentStrips(lane) {
  for (const wrap of lane.element.querySelectorAll(
    "[data-composer-attachments-target-id]",
  )) {
    fillComposerAttachmentStrip(
      wrap,
      lane,
      wrap.dataset.composerAttachmentsTargetId || "",
    );
  }
  syncLanePaneMetrics(lane);
}

function wireComposerAttachmentIngress(textarea, lane, targetId) {
  textarea.addEventListener("paste", (event) =>
    handleComposerAttachmentPaste(lane, targetId, event),
  );
  textarea.addEventListener("dragenter", (event) =>
    handleComposerAttachmentDrag(textarea, event),
  );
  textarea.addEventListener("dragover", (event) =>
    handleComposerAttachmentDrag(textarea, event),
  );
  textarea.addEventListener("dragleave", () =>
    textarea.closest(".composer-band")?.classList.remove("composer-band--drop-ready"),
  );
  textarea.addEventListener("drop", (event) =>
    handleComposerAttachmentDrop(textarea, lane, targetId, event),
  );
}

function composerImageFilesFromTransfer(transfer) {
  return [...(transfer?.files || [])].filter((file) =>
    String(file.type || "").startsWith("image/"),
  );
}

function composerTransferHasImage(transfer) {
  return composerImageFilesFromTransfer(transfer).length > 0 ||
    [...(transfer?.items || [])].some((item) =>
      String(item.type || "").startsWith("image/"),
    );
}

function handleComposerAttachmentPaste(lane, targetId, event) {
  const files = composerImageFilesFromTransfer(event.clipboardData);
  if (!files.length) return;
  event.preventDefault();
  addComposerAttachmentFiles(lane, targetId, files);
}

function handleComposerAttachmentDrag(textarea, event) {
  if (!composerTransferHasImage(event.dataTransfer)) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = "copy";
  textarea.closest(".composer-band")?.classList.add("composer-band--drop-ready");
}

function handleComposerAttachmentDrop(textarea, lane, targetId, event) {
  const files = composerImageFilesFromTransfer(event.dataTransfer);
  textarea.closest(".composer-band")?.classList.remove("composer-band--drop-ready");
  if (!files.length) return;
  event.preventDefault();
  if (files.length) addComposerAttachmentFiles(lane, targetId, files);
}

function addComposerAttachmentFiles(lane, targetId, files) {
  const sourceLane = laneGroupHost(lane);
  for (const file of files) {
    if (!String(file.type || "").startsWith("image/")) continue;
    const current = composerAttachmentDraftsForTarget(lane, targetId);
    if (current.length >= composerAttachmentMaxItems) {
      setLaneTransientStatus(sourceLane, "maximum images attached");
      return;
    }
    if (file.size > composerAttachmentMaxBytes) {
      setLaneTransientStatus(sourceLane, "image is over 8MB");
      continue;
    }
    const reader = new FileReader();
    reader.addEventListener("load", () => {
      const dataUrl = String(reader.result || "");
      if (!dataUrl) return;
      const drafts = composerAttachmentDraftsForTarget(lane, targetId).slice();
      if (drafts.length >= composerAttachmentMaxItems) return;
      drafts.push({
        id: "attachment-" + Date.now() + "-" + drafts.length,
        name: file.name || "pasted-image.png",
        contentType: file.type || "image/png",
        size: file.size || 0,
        dataUrl,
      });
      lane.shardAttachments.set(targetId, drafts);
      renderComposerAttachmentStrips(lane);
      expandLanePane(sourceLane);
    });
    reader.readAsDataURL(file);
  }
}

function removeComposerAttachment(lane, targetId, attachmentId) {
  const retained = composerAttachmentDraftsForTarget(lane, targetId).filter(
    (attachment) => attachment.id !== attachmentId,
  );
  if (retained.length) lane.shardAttachments.set(targetId, retained);
  else lane.shardAttachments.delete(targetId);
  renderComposerAttachmentStrips(lane);
}

function laneComposerAttachmentPayloads(lane, targetId) {
  return composerAttachmentDraftsForTarget(lane, targetId).map((attachment) => ({
    name: attachment.name,
    contentType: attachment.contentType,
    size: attachment.size,
    dataUrl: attachment.dataUrl,
  }));
}

function quoteMessageIntoComposer(lane, item) {
  const host = laneGroupHost(lane);
  const producer = item.producerTargetId || host.targetId;
  const targetId = host.shardTextareas.has(producer)
    ? producer
    : host.shardTextareas.keys().next().value;
  if (!targetId) return;
  const textarea =
    host.shardTextareas.get(targetId) ||
    host.shardTextareas.values().next().value;
  if (!textarea) return;
  const draftId = addComposerQuoteDraft(host, targetId, item);
  setLaneSelectedView(host, "compose");
  if (draftId) revealComposerQuoteDraft(host, draftId);
  else textarea.focus();
}

function addComposerQuoteDraft(lane, targetId, item) {
  const drafts = lane.quoteDrafts.get(targetId) || [];
  const messageKey = String(item.key || item.index || item.timestamp || "");
  if (messageKey && drafts.some((draft) => draft.messageKey === messageKey)) {
    renderComposerQuoteBands(lane);
    return drafts.find((draft) => draft.messageKey === messageKey)?.id || "";
  }
  const draft = {
    id: "quote-" + ++lane.nextQuoteDraftId,
    messageKey,
    href: messageKey ? "#" + messageDomId(messageKey) : "",
    timestamp: item.timestamp || "",
    preview: composerQuotePreview(item),
    quoteText: messageCopyText(lane, item) || item.display_text || item.text || "",
    text: "",
  };
  drafts.push(draft);
  drafts.sort((left, right) => {
    const leftTime = Date.parse(left.timestamp || "") || 0;
    const rightTime = Date.parse(right.timestamp || "") || 0;
    return rightTime - leftTime;
  });
  lane.quoteDrafts.set(targetId, drafts);
  renderComposerQuoteBands(lane);
  return draft.id;
}

function composerQuotePreview(item) {
  return String(item.preview || item.display_text || item.text || "assistant message")
    .replace(/\s+/g, " ")
    .trim();
}

function renderComposerQuoteBands(lane) {
  for (const stack of lane.element.querySelectorAll(
    "[data-composer-quote-stack-target-id]",
  )) {
    syncComposerQuoteStack(
      lane,
      stack,
      stack.dataset.composerQuoteStackTargetId || "",
    );
  }
  syncLanePaneMetrics(lane);
}

function syncComposerQuoteStack(lane, stack, targetId) {
  const member = laneStates.get(targetId) || lane;
  const bands = composerQuoteDraftsForTarget(lane, targetId).map((draft) => {
    let band = composerQuoteBandElementForDraft(stack, draft.id);
    if (!band) band = composerQuoteBand(lane, targetId, member, draft);
    else syncComposerQuoteBand(band, lane, targetId, member, draft);
    return band;
  });
  syncComposerQuoteBandOrder(stack, bands);
}

function composerQuoteBandElementForDraft(stack, draftId) {
  for (const child of stack.children) {
    if (child.dataset && child.dataset.composerQuoteBandDraftId === draftId)
      return child;
  }
  return null;
}

function syncComposerQuoteBandOrder(stack, bands) {
  const wanted = new Set(bands);
  for (const child of [...stack.children]) {
    if (!wanted.has(child)) child.remove();
  }
  let cursor = stack.firstElementChild;
  for (const band of bands) {
    if (band === cursor) {
      cursor = cursor.nextElementSibling;
      continue;
    }
    stack.insertBefore(band, cursor);
  }
}

function revealComposerQuoteDraft(lane, draftId) {
  const textarea = [...lane.element.querySelectorAll("[data-quote-draft-id]")]
    .find((element) => element.dataset.quoteDraftId === draftId);
  if (!(textarea instanceof HTMLTextAreaElement)) return;
  const band = textarea.closest(".composer-band");
  const shard = textarea.closest(".composer-shard");
  if (band instanceof HTMLElement && shard instanceof HTMLElement) {
    shard.scrollTop = Math.max(0, band.offsetTop - shard.offsetTop);
  }
  textarea.focus({ preventScroll: true });
}

function composerQuoteDraftsForTarget(lane, targetId) {
  return lane.quoteDrafts.get(targetId) || [];
}

function composerBandHeader({
  className,
  title,
  beforeMenu = [],
  trailingControl = null,
}) {
  const header = document.createElement("div");
  header.className = "composer-band-header " + className;
  const body = document.createElement("div");
  body.className = "composer-band-body";
  const label = document.createElement("span");
  label.className = "composer-band-title";
  label.textContent = title;
  body.append(label);
  header.append(...beforeMenu, body);
  if (trailingControl) header.append(trailingControl);
  return header;
}

function composerBandMenuTrigger(menuTitle, menuLabel, menuActions) {
  const trigger = document.createElement("button");
  trigger.type = "button";
  trigger.className = "composer-band-menu-button";
  trigger.title = menuTitle;
  trigger.setAttribute("aria-label", menuLabel);
  trigger.setAttribute("aria-haspopup", "menu");
  trigger.setAttribute("aria-expanded", "false");
  trigger.replaceChildren(composerBandMenuIcon());
  trigger.addEventListener("click", (event) => {
    event.stopPropagation();
    const actions =
      typeof menuActions === "function" ? menuActions() : menuActions || [];
    toggleComposerBandMenu(trigger, actions);
  });
  return trigger;
}

function composerBandMenuIcon() {
  const icon = document.createElement("span");
  icon.className = "composer-band-menu-icon";
  icon.setAttribute("aria-hidden", "true");
  icon.style.background =
    "linear-gradient(currentColor, currentColor) 0 0 / 100% 1.5px no-repeat, " +
    "linear-gradient(currentColor, currentColor) 0 50% / 100% 1.5px no-repeat, " +
    "linear-gradient(currentColor, currentColor) 0 100% / 100% 1.5px no-repeat";
  icon.style.display = "block";
  icon.style.height = "8px";
  icon.style.width = "11px";
  return icon;
}

function composerBandCloseButton(closeTitle, closeLabel, onClose) {
  const close = document.createElement("button");
  close.type = "button";
  close.className = "composer-band-close-button";
  close.title = closeTitle;
  close.setAttribute("aria-label", closeLabel || closeTitle);
  close.textContent = "×";
  close.addEventListener("click", (event) => {
    event.stopPropagation();
    onClose();
  });
  return close;
}

function toggleComposerBandMenu(trigger, actions) {
  const band = trigger.closest(".composer-band");
  if (!band) return;
  const open = trigger.getAttribute("aria-expanded") === "true";
  closeComposerBandMenusExcept(band);
  closeComposerBandMenu(band);
  if (open) return;
  const menu = document.createElement("div");
  menu.className = "composer-band-menu spice-menu-actions";
  menu.setAttribute("role", "menu");
  for (const action of actions) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "composer-band-menu-action spice-menu-action";
    const hasPressed = action.pressed !== undefined && action.pressed !== null;
    button.setAttribute("role", hasPressed ? "menuitemcheckbox" : "menuitem");
    if (hasPressed) button.setAttribute("aria-checked", String(action.pressed));
    button.disabled = Boolean(action.disabled);
    if (action.detail) button.title = action.detail;
    button.innerHTML =
      '<span class="spice-menu-action-label"></span>' +
      '<span class="spice-menu-action-detail"></span>';
    button.querySelector(".spice-menu-action-label").textContent = action.label;
    button.querySelector(".spice-menu-action-detail").textContent =
      action.detail || "";
    button.addEventListener("click", () => {
      if (!action.keepOpen) closeComposerBandMenu(band);
      const previousPressed = hasPressed
        ? button.getAttribute("aria-checked") === "true"
        : null;
      const nextPressed = hasPressed ? !previousPressed : null;
      if (hasPressed) syncComposerBandMenuActionPressed(button, nextPressed);
      const result = action.onClick(nextPressed);
      result?.catch?.(() => {
        if (hasPressed) syncComposerBandMenuActionPressed(button, previousPressed);
      });
    });
    menu.append(button);
  }
  const textarea = band.querySelector("textarea");
  band.insertBefore(menu, textarea || null);
  band.classList.add("composer-band--menu-open");
  trigger.setAttribute("aria-expanded", "true");
  syncComposerBandMenuDismissHandler();
}

function syncComposerBandMenuActionPressed(button, pressed) {
  button.setAttribute("aria-checked", String(Boolean(pressed)));
}

function closeComposerBandMenu(band) {
  band.querySelector(".composer-band-menu")?.remove();
  band.classList.remove("composer-band--menu-open");
  const trigger = band.querySelector(".composer-band-menu-button");
  if (trigger) trigger.setAttribute("aria-expanded", "false");
  syncComposerBandMenuDismissHandler();
}

function closeComposerBandMenusExcept(exceptBand) {
  for (const band of document.querySelectorAll(".composer-band--menu-open")) {
    if (band !== exceptBand) closeComposerBandMenu(band);
  }
  syncComposerBandMenuDismissHandler();
}

function syncComposerBandMenuDismissHandler() {
  const hasOpenMenu = document.querySelector(".composer-band--menu-open");
  if (hasOpenMenu && !composerBandMenuDismissHandler) {
    composerBandMenuDismissHandler = dismissComposerBandMenusOnPointerDown;
    document.addEventListener("pointerdown", composerBandMenuDismissHandler, true);
  } else if (!hasOpenMenu && composerBandMenuDismissHandler) {
    document.removeEventListener(
      "pointerdown",
      composerBandMenuDismissHandler,
      true,
    );
    composerBandMenuDismissHandler = null;
  }
}

function dismissComposerBandMenusOnPointerDown(event) {
  const target = event.target;
  if (!(target instanceof Node)) return;
  for (const band of document.querySelectorAll(".composer-band--menu-open")) {
    const menu = band.querySelector(".composer-band-menu");
    const trigger = band.querySelector(".composer-band-menu-button");
    if (menu?.contains(target) || trigger?.contains(target)) continue;
    closeComposerBandMenu(band);
  }
  syncComposerBandMenuDismissHandler();
}

function syncComposerBandMenuState(band) {
  const open = [...band.children].some((child) =>
    child.classList?.contains("composer-band-menu"),
  );
  band.classList.toggle("composer-band--menu-open", open);
  const trigger = band.querySelector(".composer-band-menu-button");
  if (trigger) trigger.setAttribute("aria-expanded", open ? "true" : "false");
  syncComposerBandMenuDismissHandler();
}

function composerQuoteBand(lane, targetId, member, draft) {
  const band = document.createElement("div");
  syncComposerQuoteBand(band, lane, targetId, member, draft);
  return band;
}

function syncComposerQuoteBand(band, lane, targetId, member, draft) {
  band.className = "composer-band composer-band--quote";
  band.title = draft.quoteText || draft.preview;
  band.dataset.composerQuoteBandDraftId = draft.id;
  syncComposerBandAccent(band, lane, member);
  const header = composerQuoteBandHeader(lane, targetId, member, draft);
  const previousHeader = band.querySelector(".composer-band-header--quote");
  if (previousHeader) previousHeader.replaceWith(header);
  else band.prepend(header);
  syncComposerBandMenuState(band);
  let textarea = band.querySelector("textarea");
  syncComposerAttachmentStrip(band, lane, targetId, textarea);
  if (!textarea) {
    textarea = createComposerQuoteTextarea(lane, targetId, draft);
    band.append(textarea);
  } else {
    textarea.dataset.quoteDraftId = draft.id;
    if (document.activeElement !== textarea && textarea.value !== (draft.text || ""))
      textarea.value = draft.text || "";
  }
  textarea.placeholder = laneComposePlaceholder(member);
}

function composerQuoteBandHeader(lane, targetId, member, draft) {
  let time;
  if (draft.href) {
    const anchor = document.createElement("a");
    anchor.href = draft.href;
    anchor.title = "Jump to quoted message";
    time = anchor;
  } else {
    time = document.createElement("span");
  }
  time.className = "composer-quote-time";
  time.dataset.relativeTimestamp = draft.timestamp || "";
  time.dataset.relativeFallback = "quote";
  syncComposerHeaderStatus(time, member);
  setRelativeTimeText(time);
  const header = composerBandHeader({
    className: "composer-band-header--quote",
    title: draft.preview || "quoted message",
    beforeMenu: [time],
    trailingControl: composerBandCloseButton(
      "Remove quote",
      "Remove quoted composer",
      () => removeComposerQuoteDraft(lane, targetId, draft.id),
    ),
  });
  return header;
}

function createComposerQuoteTextarea(lane, targetId, draft) {
  const textarea = document.createElement("textarea");
  textarea.rows = 2;
  textarea.value = draft.text || "";
  textarea.dataset.quoteDraftId = draft.id;
  textarea.addEventListener("focus", () => expandLanePane(lane));
  textarea.addEventListener("input", () => {
    draft.text = textarea.value;
    expandLanePane(lane);
  });
  wireComposerAttachmentIngress(textarea, lane, targetId);
  textarea.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      submitLaneForm(lane, event, targetId);
    }
  });
  return textarea;
}

function removeComposerQuoteDraft(lane, targetId, draftId) {
  const drafts = composerQuoteDraftsForTarget(lane, targetId).filter(
    (draft) => draft.id !== draftId,
  );
  if (drafts.length) lane.quoteDrafts.set(targetId, drafts);
  else lane.quoteDrafts.delete(targetId);
  renderComposerQuoteBands(lane);
}

function pruneComposerQuoteDrafts(lane, targetIds) {
  const liveTargets = new Set(targetIds);
  for (const targetId of lane.quoteDrafts.keys()) {
    if (!liveTargets.has(targetId)) lane.quoteDrafts.delete(targetId);
  }
}

function pruneComposerAttachments(lane, targetIds) {
  const liveTargets = new Set(targetIds);
  for (const targetId of lane.shardAttachments.keys()) {
    if (!liveTargets.has(targetId)) lane.shardAttachments.delete(targetId);
  }
}

function removeComposerAgentFromTeam(lane, targetId) {
  const host = laneGroupHost(lane);
  const member = laneStates.get(targetId);
  if (!member) return;
  if (laneComposerTargetDraftText(host, targetId).trim()) {
    if (!window.confirm(unsafeDraftWarningText())) return;
  }
  const teamId = member.teamId || host.teamId;
  if (!teamId) return;
  member.serverCloseRequested = true;
  requestTeamCommand(
    teamCommandPayload("removeAgentFromTeam", {
      teamId,
      agentId: laneTeamAgentId(member),
      agentAliases: laneTeamAgentAliases(member),
    }),
  ).catch(() => {
    member.serverCloseRequested = false;
    setLaneTransientStatus(host, "remove agent from team failed");
  });
}

function toggleComposerAgentRenewalIntent(
  lane,
  member,
  requested = !composerRenewalIntentRequested(member),
) {
  const host = laneGroupHost(lane);
  return requestTeamCommand(
    teamCommandPayload("setAgentRenewalIntent", {
      agentId: laneTeamAgentId(member),
      requested,
    }),
  )
    .then(() => {
      setLaneTransientStatus(
        host,
        requested ? "renewal requested" : "renewal cleared",
      );
    })
    .catch(() => {
      setLaneTransientStatus(host, "renewal update failed");
      throw new Error("renewal update failed");
    });
}

function composerRenewalIntent(member) {
  return (member && member.renewalIntent) || {};
}

function composerRenewalIntentRequested(member) {
  return Boolean(composerRenewalIntent(member).requested);
}

function composerRenewalIntentInFlight(member) {
  const state = String(composerRenewalIntent(member).state || "");
  return Boolean(state && state !== "requested");
}

function composerRenewalActionDetail(member) {
  const intent = composerRenewalIntent(member);
  if (intent.requested) return "requested";
  if (intent.state === "pending") return "handoff pending";
  if (intent.state === "started") return "successor started";
  return "request renewal";
}

function laneComposerSubmissionText(lane, targetId, draftText) {
  const quotes = composerQuoteDraftsForTarget(lane, targetId)
    .map((draft) => quoteDraftSubmissionText(draft))
    .filter((part) => part.trim());
  const body = String(draftText || "").trim();
  return [body, ...quotes].filter((part) => part.trim()).join("\n\n");
}

function quoteDraftSubmissionText(draft) {
  const quote = markdownBlockQuote(draft.quoteText);
  const body = String(draft.text || "").trim();
  return [quote, body].filter((part) => part.trim()).join("\n\n");
}
