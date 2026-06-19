// Lane secondary panes: filters, metrics, and info.

// ---- filters pane -----------------------------------------------------------------------

function laneAssignedTaskFilters(lane) {
  const filters = [];
  for (const member of laneGroupMemberLanes(laneGroupHost(lane))) {
    for (const filter of member.taskFilters || []) {
      if (filter && !filters.includes(filter)) filters.push(filter);
    }
  }
  return filters.sort();
}

function laneFilterInventory(lane) {
  return laneGroupHost(lane).taskFilterInventory;
}

function laneEffectiveAssignedFilterNames(inventory, assignedFilters) {
  const covered = new Set();
  const stems = new Map(
    ((inventory || {}).primaryStems || []).map((stem) => [stem.name, stem]),
  );
  for (const assignedFilter of assignedFilters || []) {
    if (!assignedFilter) continue;
    covered.add(assignedFilter);
    const stem = stems.get(assignedFilter);
    if (!stem) continue;
    for (const stemFilter of stem.filters || []) {
      if (stemFilter) covered.add(stemFilter);
    }
  }
  return covered;
}

function laneFilterAvailableOpenTaskCount(lane, assignedFilters) {
  const inventory = laneFilterInventory(lane);
  if (!inventory) return 0;
  const covered = laneEffectiveAssignedFilterNames(inventory, assignedFilters);
  return (inventory.filters || []).reduce((total, filter) => {
    if (!filter.name || covered.has(filter.name)) return total;
    return total + Math.max(0, Number(filter.openTaskCount) || 0);
  }, 0);
}

function laneAvailableTaskFilters(lane, assignedFilters) {
  const inventory = laneFilterInventory(lane);
  if (!inventory) return [];
  const covered = laneEffectiveAssignedFilterNames(inventory, assignedFilters);
  return (inventory.filters || [])
    .map((filter) => filter.name)
    .filter((filter) => filter && !covered.has(filter))
    .sort();
}

function laneTaskFilterOpenCount(lane, filter) {
  const inventory = laneFilterInventory(lane);
  if (!inventory) return 0;
  const stem = (inventory.primaryStems || []).find(
    (item) => item.name === filter,
  );
  if (stem) return Math.max(0, Number(stem.openTaskCount) || 0);
  const row = (inventory.filters || []).find((item) => item.name === filter);
  return row ? Math.max(0, Number(row.openTaskCount) || 0) : 0;
}

function renderLaneFiltersPane(lane) {
  if (!lane.filtersChipsEl) return;
  const model = laneFilterPaneRenderModel(lane);
  if (model.fingerprint === lane.renderedFilterPaneFingerprint) return;
  lane.renderedFilterPaneFingerprint = model.fingerprint;
  const { filterPolicy, filters, pickerAssignedFilters, privateQueues, queueCount } =
    model;
  for (const filter of [...lane.selectedFilterRemovals]) {
    if (!filters.includes(filter)) lane.selectedFilterRemovals.delete(filter);
  }
  lane.filtersSummaryEl.textContent =
    filterPolicy === "all projects"
      ? "all assignable"
      : filters.length
        ? filters.length + " assigned"
        : "private only";
  lane.filtersQueueEl.textContent = filterPolicy + " " + queueCount + " queues";
  /** @type {(HTMLButtonElement | HTMLSpanElement)[]} */
  const chips = [laneFilterAssignChip(lane, pickerAssignedFilters)];
  for (const filter of filters) {
    chips.push(laneFilterChip(lane, filter));
  }
  for (const queue of privateQueues) {
    chips.push(laneFilterPrivateChip(queue));
  }
  lane.filtersChipsEl.replaceChildren(...chips);
  syncLaneFilterAssignOverlay(lane, filters);
}

function laneFilterPaneRenderModel(lane) {
  const filters = laneAssignedTaskFilters(lane);
  const pickerAssignedFilters = laneFilterPickerAssignedFilters(lane, filters);
  const privateQueues = lanePrivateQueues(lane);
  const removals = [...lane.selectedFilterRemovals].sort();
  const pending = [...lane.filterPickerPendingAssignments].sort();
  const availableFilters = laneAvailableTaskFilters(lane, pickerAssignedFilters);
  const filterPolicy = laneFilterPolicyLabel(laneEffectiveLifetime(lane));
  const queueCount =
    filterPolicy === "all projects"
      ? laneAssignableTaskFilterQueueCount(lane) + privateQueues.length
      : filters.length + privateQueues.length;
  const pickerActions = lane.filterPickerOpen
    ? laneFilterPickerActions(
        lane,
        pickerAssignedFilters,
        lane.filterPickerQuery,
      ).map(laneFilterPickerActionFingerprint)
    : [];
  return {
    filterPolicy,
    filters,
    pickerAssignedFilters,
    privateQueues,
    queueCount,
    fingerprint: JSON.stringify({
      availableFilters,
      availableOpenTaskCount: laneFilterAvailableOpenTaskCount(
        lane,
        pickerAssignedFilters,
      ),
      canCreate: laneFilterCanCreate(lane),
      filterPolicy,
      queueCount,
      filterCounts: filters.map((filter) => [
        filter,
        laneTaskFilterOpenCount(lane, filter),
      ]),
      filters,
      pickerActions,
      pickerFooter: lane.filterPickerOpen ? laneFilterPickerFooterText(lane) : "",
      pickerOpen: lane.filterPickerOpen,
      pickerQuery: lane.filterPickerQuery || "",
      pending,
      privateQueues,
      removals,
    }),
  };
}

function laneFilterPolicyLabel(lifetime) {
  if (agentLifetimeDissolvesTaskBoundary(lifetime)) return "all projects";
  if (agentLifetimeAutoManagesTasks(lifetime)) return "auto";
  return "manual";
}

function laneAssignableTaskFilterQueueCount(lane) {
  const inventory = laneFilterInventory(lane);
  if (!inventory) return 0;
  const catalog = inventory.catalog || {};
  const approved = new Set((catalog.approvedStems || []).filter(Boolean));
  return (inventory.primaryStems || []).filter(
    (stem) => stem.name && stem.name !== "agent" && approved.has(stem.name),
  ).length;
}

function laneFilterPickerActionFingerprint(action) {
  return [
    action.kind || "",
    action.filter || "",
    action.label || "",
    action.detail || "",
  ];
}

function laneFilterChip(lane, filter) {
  const selected = lane.selectedFilterRemovals.has(filter);
  const count = laneTaskFilterOpenCount(lane, filter);
  const chip = document.createElement("button");
  chip.type = "button";
  chip.className = "lane-filter-chip";
  chip.classList.toggle("lane-filter-chip--selected", selected);
  chip.classList.toggle("lane-filter-chip--existing", count > 0);
  chip.classList.toggle("lane-filter-chip--empty", count === 0);
  chip.title = "Select lane filter for removal: " + filter;
  chip.innerHTML =
    '<span class="lane-filter-chip-label"></span>' +
    '<span class="lane-filter-chip-count"></span>';
  chip.querySelector(".lane-filter-chip-label").textContent = filter;
  chip.querySelector(".lane-filter-chip-count").textContent = String(count);
  chip.addEventListener("click", () => {
    if (selected) lane.selectedFilterRemovals.delete(filter);
    else lane.selectedFilterRemovals.add(filter);
    renderLaneFiltersPane(lane);
  });
  return chip;
}

function lanePrivateQueues(lane) {
  const queues = [];
  for (const member of laneGroupMemberLanes(laneGroupHost(lane))) {
    const label = member.agentName || "private";
    const count = Math.max(0, Number(member.privateTaskCount) || 0);
    const key = label + "\n" + (member.targetId || "");
    const existing = queues.find((queue) => queue.key === key);
    if (existing) existing.count += count;
    else queues.push({ key, label, count });
  }
  return queues;
}

function laneFilterPrivateChip(queue) {
  const chip = document.createElement("span");
  chip.className = "lane-filter-chip lane-filter-chip--private";
  chip.title = queue.label + " private queue";
  chip.innerHTML =
    '<span class="lane-filter-chip-label"></span>' +
    '<span class="lane-filter-chip-count"></span>';
  chip.querySelector(".lane-filter-chip-label").textContent = queue.label;
  chip.querySelector(".lane-filter-chip-count").textContent = String(queue.count);
  return chip;
}

function syncLaneFilterAssignOverlay(lane, assignedFilters) {
  const previousPicker = lane.filterPickerOverlayEl;
  const previousScrollTop = laneFilterPickerResultsScrollTop(previousPicker);
  const refocusSearch =
    previousPicker &&
    document.activeElement === previousPicker.querySelector(".lane-filter-search");
  destroyLaneFilterAssignOverlay(lane);
  if (!lane.filterPickerOpen) return;
  const picker = renderLaneFilterPicker(lane, assignedFilters);
  picker.classList.add("lane-filter-picker--overlay");
  document.body.append(picker);
  lane.filterPickerOverlayEl = picker;
  const position = () => positionLaneFilterAssignOverlay(lane);
  const dismiss = (event) => dismissLaneFilterAssignOverlay(lane, event);
  lane.filterPickerOverlayPositionHandler = position;
  lane.filterPickerOverlayDismissHandler = dismiss;
  window.addEventListener("resize", position);
  window.addEventListener("scroll", position, true);
  document.addEventListener("focusin", dismiss, true);
  document.addEventListener("pointerdown", dismiss, true);
  position();
  restoreLaneFilterPickerResultsScroll(picker, previousScrollTop);
  if (refocusSearch) {
    const input = picker.querySelector(".lane-filter-search");
    if (input instanceof HTMLElement) input.focus({ preventScroll: true });
  }
}

function laneFilterPickerResultsScrollTop(picker) {
  const results = picker?.querySelector(".lane-filter-results");
  return results ? results.scrollTop : 0;
}

function restoreLaneFilterPickerResultsScroll(picker, scrollTop) {
  const results = picker?.querySelector(".lane-filter-results");
  if (results) results.scrollTop = scrollTop;
}

function destroyLaneFilterAssignOverlay(lane) {
  if (lane.filterPickerOverlayPositionHandler) {
    window.removeEventListener("resize", lane.filterPickerOverlayPositionHandler);
    window.removeEventListener(
      "scroll",
      lane.filterPickerOverlayPositionHandler,
      true,
    );
    lane.filterPickerOverlayPositionHandler = null;
  }
  if (lane.filterPickerOverlayDismissHandler) {
    document.removeEventListener(
      "focusin",
      lane.filterPickerOverlayDismissHandler,
      true,
    );
    document.removeEventListener(
      "pointerdown",
      lane.filterPickerOverlayDismissHandler,
      true,
    );
    lane.filterPickerOverlayDismissHandler = null;
  }
  if (lane.filterPickerOverlayEl) {
    lane.filterPickerOverlayEl.remove();
    lane.filterPickerOverlayEl = null;
  }
}

function dismissLaneFilterAssignOverlay(lane, event) {
  const target = event.target;
  if (!(target instanceof Node)) return;
  const picker = lane.filterPickerOverlayEl;
  if (picker && picker.contains(target)) return;
  const assign = lane.filtersChipsEl.querySelector(".lane-filter-chip--assign");
  if (assign instanceof HTMLElement && assign.contains(target)) return;
  closeLaneFilterAssignPicker(lane);
}

function positionLaneFilterAssignOverlay(lane) {
  const picker = lane.filterPickerOverlayEl;
  const anchor = lane.filtersChipsEl.querySelector(".lane-filter-chip--assign");
  if (!(picker instanceof HTMLElement) || !(anchor instanceof HTMLElement))
    return;
  const margin = 8;
  const anchorRect = anchor.getBoundingClientRect();
  const laneRect = lane.element.getBoundingClientRect();
  const width = Math.max(
    anchorRect.width,
    Math.min(laneRect.width, window.innerWidth - margin * 2),
  );
  const left = Math.max(
    margin,
    Math.min(laneRect.left, window.innerWidth - width - margin),
  );
  picker.style.left = left + "px";
  picker.style.top = anchorRect.bottom + margin + "px";
  picker.style.width = width + "px";
  const pickerRect = picker.getBoundingClientRect();
  if (pickerRect.bottom <= window.innerHeight - margin) return;
  picker.style.top =
    Math.max(margin, anchorRect.top - pickerRect.height - margin) + "px";
}

function laneFilterPickerRoot(lane) {
  return lane.filterPickerOverlayEl || lane.filtersChipsEl;
}

function laneFilterAssignChip(lane, filters) {
  const removals = [...lane.selectedFilterRemovals].sort();
  const pending = [...lane.filterPickerPendingAssignments].sort();
  const removing = removals.length > 0;
  const catalogReady = Boolean(laneFilterInventory(lane));
  const availableOpenTaskCount = laneFilterAvailableOpenTaskCount(lane, filters);
  const availableFilters = laneAvailableTaskFilters(lane, filters);
  const button = document.createElement("button");
  button.type = "button";
  button.className = "lane-filter-chip lane-filter-chip--assign";
  button.classList.toggle(
    "lane-filter-chip--assign-open",
    removing || lane.filterPickerOpen,
  );
  const label = document.createElement("span");
  label.className = "lane-filter-chip-label";
  label.textContent = removing
    ? "unassign"
    : lane.filterPickerOpen
      ? pending.length
        ? "done"
        : "done"
      : "+ assign";
  button.append(label);
  if (removing || catalogReady) {
    const count = removing ? removals.length : availableOpenTaskCount;
    const countEl = document.createElement("span");
    countEl.className = "lane-filter-chip-count";
    countEl.textContent = String(count);
    button.append(countEl);
  }
  button.title = removing
    ? "Remove selected lane filters"
    : "Assign lane filter; " + availableOpenTaskCount + " unassigned open tasks";
  button.disabled =
    !removing &&
    !lane.filterPickerOpen &&
    catalogReady &&
    !availableFilters.length &&
    !laneFilterCanCreate(lane);
  button.addEventListener("click", () => {
    if (removing) {
      mutateLaneTaskFilters(lane, (current) =>
        current.filter((item) => !removals.includes(item)),
      );
      lane.selectedFilterRemovals.clear();
      return;
    }
    if (lane.filterPickerOpen) closeLaneFilterAssignPicker(lane);
    else openLaneFilterAssignPicker(lane);
  });
  return button;
}

function laneFilterCanCreate(lane) {
  const inventory = laneFilterInventory(lane);
  if (!inventory) return false;
  const catalog = inventory.catalog || {};
  return (catalog.approvedStems || []).length > 0;
}

function laneFilterPickerAssignedFilters(lane, assignedFilters) {
  return uniqueStringList([
    ...(assignedFilters || []),
    ...lane.filterPickerPendingAssignments,
  ]);
}

function openLaneFilterAssignPicker(lane) {
  lane.filterPickerOpen = true;
  lane.filterPickerQuery = "";
  lane.filterPickerPendingAssignments.clear();
  renderLaneFiltersPane(lane);
  setTimeout(() => {
    const input = laneFilterPickerRoot(lane).querySelector(".lane-filter-search");
    if (input instanceof HTMLInputElement) input.focus({ preventScroll: true });
  }, 0);
}

function closeLaneFilterAssignPicker(lane) {
  const pending = [...lane.filterPickerPendingAssignments];
  lane.filterPickerPendingAssignments.clear();
  lane.filterPickerOpen = false;
  lane.filterPickerQuery = "";
  renderLaneFiltersPane(lane);
  if (pending.length) {
    mutateLaneTaskFilters(lane, (current) => [...current, ...pending]);
  }
}

function renderLaneFilterPicker(lane, assignedFilters) {
  const picker = document.createElement("div");
  picker.className = "lane-filter-picker";
  const input = document.createElement("input");
  input.type = "search";
  input.className = "lane-filter-search";
  input.placeholder = "Filter or create lane filter";
  input.value = lane.filterPickerQuery || "";
  const results = document.createElement("div");
  results.className = "lane-filter-results";
  const footer = document.createElement("div");
  footer.className = "lane-filter-picker-footer";
  footer.textContent = laneFilterPickerFooterText(lane);
  const syncResults = () => {
    const scrollTop = results.scrollTop;
    lane.filterPickerQuery = input.value;
    results.replaceChildren(
      ...laneFilterPickerActionNodes(
        lane,
        assignedFilters,
        input.value,
        syncResults,
      ),
    );
    results.scrollTop = scrollTop;
  };
  input.addEventListener("input", syncResults);
  input.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closeLaneFilterAssignPicker(lane);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const action = laneFilterPickerActions(lane, assignedFilters, input.value)[0];
      if (action) toggleLaneFilterPendingAssignment(lane, action.filter, syncResults);
    }
  });
  syncResults();
  picker.append(input, results, footer);
  return picker;
}

function toggleLaneFilterPendingAssignment(lane, filter, syncResults) {
  if (lane.filterPickerPendingAssignments.has(filter))
    lane.filterPickerPendingAssignments.delete(filter);
  else lane.filterPickerPendingAssignments.add(filter);
  syncResults();
}

function laneFilterPickerActionNodes(lane, assignedFilters, query, syncResults) {
  const actions = laneFilterPickerActions(lane, assignedFilters, query);
  if (!actions.length) {
    const empty = document.createElement("div");
    empty.className = "lane-filter-picker-empty";
    empty.textContent = "No valid lane filter";
    return [empty];
  }
  return actions.map((action) => {
    const selected = lane.filterPickerPendingAssignments.has(action.filter);
    const button = document.createElement("button");
    button.type = "button";
    button.className = "lane-filter-picker-action";
    button.classList.toggle("lane-filter-picker-action--selected", selected);
    button.innerHTML =
      '<span class="lane-filter-picker-action-label"></span>' +
      '<span class="lane-filter-picker-action-detail"></span>';
    button.querySelector(".lane-filter-picker-action-label").textContent =
      action.label;
    button.querySelector(".lane-filter-picker-action-detail").textContent =
      action.detail;
    button.addEventListener("click", () =>
      toggleLaneFilterPendingAssignment(lane, action.filter, syncResults),
    );
    return button;
  });
}

function laneFilterPickerActions(lane, assignedFilters, query) {
  const inventory = laneFilterInventory(lane);
  if (!inventory) return [];
  const assigned = new Set(assignedFilters);
  const normalized = String(query || "").trim();
  const queryLower = normalized.toLowerCase();
  const existing = (inventory.filters || [])
    .filter((filter) => filter.name && !assigned.has(filter.name))
    .filter(
      (filter) => !queryLower || filter.name.toLowerCase().includes(queryLower),
    )
    .map((filter) => ({
      kind: "existing",
      filter: filter.name,
      label: filter.name,
      detail: filter.openTaskCount + " tasks · existing",
    }));
  const catalog = inventory.catalog || {};
  const stems = (catalog.approvedStems || [])
    .filter((stem) => !assigned.has(stem))
    .filter((stem) => !queryLower || stem.toLowerCase().includes(queryLower))
    .map((stem) => ({
      kind: "stem",
      filter: stem,
      label: stem,
      detail: laneTaskFilterOpenCount(lane, stem) + " tasks · stem",
    }));
  const create = laneFilterCreateAction(lane, assigned, normalized);
  const actions = [...existing, ...stems].sort(compareLaneFilterPickerActions);
  return create ? [...actions, create] : actions;
}

function compareLaneFilterPickerActions(left, right) {
  return (
    String(left.label || "").localeCompare(String(right.label || "")) ||
    String(left.kind || "").localeCompare(String(right.kind || "")) ||
    String(left.filter || "").localeCompare(String(right.filter || ""))
  );
}

function laneFilterCreateAction(lane, assigned, query) {
  if (!query || assigned.has(query)) return null;
  const inventory = laneFilterInventory(lane);
  if (!inventory) return null;
  if ((inventory.filters || []).some((filter) => filter.name === query))
    return null;
  const catalog = inventory.catalog || {};
  const delimiter = catalog.filterDelimiter || ".";
  const segments = query.split(delimiter);
  const pattern = new RegExp("^" + (catalog.segmentPattern || "[0-9a-z_]+") + "$");
  const valid =
    (catalog.approvedStems || []).includes(segments[0]) &&
    segments.every((segment) => pattern.test(segment));
  if (!valid) return null;
  return {
    kind: "create",
    filter: query,
    label: query,
    detail: "new empty assignment",
  };
}

function laneFilterPickerFooterText(lane) {
  const inventory = laneFilterInventory(lane);
  if (!inventory) return "Lane filter catalog loading";
  const catalog = inventory.catalog || {};
  const stems = (catalog.approvedStems || []).join(", ");
  const examples = (catalog.filterExamples || []).slice(0, 3).join(", ");
  return (
    "Lane filter stems: " +
    stems +
    " · " +
    (catalog.segmentRuleLabel || "") +
    (examples ? " · examples: " + examples : "")
  );
}

function mutateLaneTaskFilters(lane, updateFilters) {
  const host = laneGroupHost(lane);
  updateTaskDrainForLane(host, {
    taskFilters: uniqueStringList(updateFilters(laneAssignedTaskFilters(host))),
    replaceTaskFilters: true,
  });
}

// ---- metrics pane -------------------------------------------------------------------------

let laneMetricsLitIslandPromise = null;
let laneMetricsLitIslandRenderer = null;
const laneMetricsLitIslandModulePath = "/static/app.metrics-lit.js";

function renderLaneMetricsPane(lane) {
  if (!lane.metricsGridEl) return;
  const model = laneMetricsRenderModel(lane);
  lane.metricsSummaryEl.textContent = model.status;
  if (laneMetricsLitIslandEnabled()) {
    if (laneMetricsLitIslandRenderer) {
      laneMetricsLitIslandRenderer(lane.metricsGridEl, model);
      return;
    }
    renderLaneMetricsVanilla(lane.metricsGridEl, model);
    loadLaneMetricsLitIsland().then(
      (renderer) => {
        if (!lane.metricsGridEl) return;
        renderer(lane.metricsGridEl, laneMetricsRenderModel(lane));
      },
      (error) => reportLaneMetricsLitIslandError(error),
    );
    return;
  }
  renderLaneMetricsVanilla(lane.metricsGridEl, model);
}

function renderLaneMetricsVanilla(grid, model) {
  const cells = model.cells.map((cell) => laneMetricCell(cell.label, cell.value));
  grid.replaceChildren(
    ...cells,
    laneMetricSparklineCell(model),
  );
}

function laneMetricsRenderModel(lane) {
  const metrics = lane.laneMetrics || {};
  const sparkline = Array.isArray(metrics.sparkline)
    ? metrics.sparkline.map((value) => Number(value) || 0)
    : [];
  return {
    status: lane.serverReachable ? "live" : "offline",
    cells: [
      { label: "drained", value: String(metrics.drained || 0) },
      { label: "acked", value: String(metrics.acked || 0) },
      { label: "sends", value: String(metrics.sends || 0) },
      { label: "tool calls", value: String(metrics.toolCalls || 0) },
      { label: "uptime", value: laneMetricDuration(metrics.uptimeSeconds || 0) },
    ],
    sparkline,
    activityTotal: sparkline.reduce((sum, value) => sum + value, 0),
  };
}

function laneMetricsLitIslandEnabled() {
  if (typeof window === "undefined") return false;
  if (/** @type {any} */ (window).__spiceForceLitMetricsIsland === true)
    return true;
  try {
    const params = new URLSearchParams(window.location.search || "");
    if (params.get("litMetrics") === "1") return true;
  } catch (error) {
    // Fall through to storage; malformed synthetic locations should not break metrics.
  }
  const storage = browserStorage();
  return storage ? storage.getItem("spice.serve.litMetrics") === "1" : false;
}

function loadLaneMetricsLitIsland() {
  if (laneMetricsLitIslandPromise) return laneMetricsLitIslandPromise;
  const loader =
    /** @type {any} */ (window).__spiceLitMetricsModuleLoader ||
    (() => import(laneMetricsLitIslandModulePath));
  laneMetricsLitIslandPromise = Promise.resolve()
    .then(() => loader())
    .then((module) => {
      const renderer = module && module.renderLaneMetricsLitIsland;
      if (typeof renderer !== "function")
        throw new Error("Lit metrics island did not export a renderer");
      laneMetricsLitIslandRenderer = renderer;
      return renderer;
    });
  return laneMetricsLitIslandPromise;
}

function reportLaneMetricsLitIslandError(error) {
  const status = /** @type {any} */ (window).setGlobalTransientStatus;
  if (typeof status === "function")
    status("Lit metrics island failed: " + String(error));
  throw error;
}

function laneMetricCell(label, value) {
  const cell = document.createElement("span");
  cell.className = "lane-metric-cell";
  const valueEl = document.createElement("span");
  valueEl.className = "lane-metric-value";
  valueEl.textContent = value;
  const labelEl = document.createElement("span");
  labelEl.className = "lane-metric-label";
  labelEl.textContent = label;
  cell.append(valueEl, labelEl);
  return cell;
}

function laneMetricSparklineCell(model) {
  const cell = laneMetricCell("activity", model.activityTotal + " messages");
  cell.classList.add("lane-metric-cell--wide");
  const wrap = document.createElement("div");
  wrap.className = "lane-metric-sparkline";
  const max = Math.max(1, ...model.sparkline);
  for (const value of model.sparkline) {
    const bar = document.createElement("span");
    bar.className = "lane-metric-sparkline-bar";
    bar.style.setProperty(
      "--lane-metric-sparkline-level",
      String(Math.max(1, Math.ceil((value / max) * 8))),
    );
    wrap.append(bar);
  }
  cell.append(wrap);
  return cell;
}

const metricSecondsPerMinute = 60;
const metricSecondsPerHour = 3600;
const metricSecondsPerDay = 86400;

function laneMetricDuration(seconds) {
  if (seconds < metricSecondsPerMinute) return seconds + "s";
  if (seconds < metricSecondsPerHour)
    return Math.round(seconds / metricSecondsPerMinute) + "m";
  if (seconds < metricSecondsPerDay)
    return Math.round(seconds / metricSecondsPerHour) + "h";
  return Math.round(seconds / metricSecondsPerDay) + "d";
}

// ---- info pane -----------------------------------------------------------------------------

function renderLaneInfoPane(lane) {
  if (!lane.infoGridEl) return;
  const host = laneGroupHost(lane);
  const members = laneGroupMemberLanes(host);
  const cells = [];
  if (members.length > 1) {
    for (const member of members) {
      const heading = document.createElement("span");
      heading.className = "lane-info-member-heading";
      heading.textContent = laneMemberTargetLabel(member);
      cells.push(heading);
      appendLaneInfoRows(cells, member.laneInfo);
    }
  } else {
    appendLaneInfoRows(cells, host.laneInfo);
  }
  lane.infoSummaryEl.textContent = members.length > 1 ? "group" : "copy";
  lane.infoGridEl.replaceChildren(...cells);
}

function appendLaneInfoRows(cells, info) {
  for (const row of (info || {}).summaryRows || []) {
    cells.push(laneInfoCell(row.key, row.value, Boolean(row.span)));
  }
}

function laneInfoCell(label, value, span) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = span
    ? "lane-info-cell lane-info-cell--wide"
    : "lane-info-cell";
  button.title = "Copy " + label + ": " + value;
  button.innerHTML =
    '<span class="lane-info-key"></span><span class="lane-info-value"></span>' +
    '<span class="lane-info-copy" aria-hidden="true">⧉</span>';
  button.querySelector(".lane-info-key").textContent = label;
  button.querySelector(".lane-info-value").textContent = value;
  button.addEventListener("click", () => {
    const copyEl = button.querySelector(".lane-info-copy");
    writeClipboardText(value).then((copied) => {
      copyEl.textContent = copied ? "copied" : "copy failed";
      setTimeout(() => {
        copyEl.textContent = "⧉";
      }, 1100);
    });
  });
  return button;
}
