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
  return taskFilterEffectiveAssignedNames(inventory, assignedFilters);
}

function laneFilterAvailableOpenTaskCount(lane, assignedFilters) {
  return availableTaskFilterOpenTaskCount(
    laneFilterInventory(lane),
    assignedFilters,
  );
}

function laneAvailableTaskFilters(lane, assignedFilters) {
  return availableTaskFilterNames(laneFilterInventory(lane), assignedFilters);
}

function laneTaskFilterOpenCount(lane, filter) {
  return taskFilterOpenCount(laneFilterInventory(lane), filter);
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
const laneMetricSeriesMetrics = [
  ["activity", "activity"],
  ["sends", "sends"],
  ["acks", "acks"],
  ["burndown", "burndown"],
  ["distribution", "distribution"],
  ["stuck", "stuck"],
];
const laneMetricSeriesLenses = [
  ["lineage", "lineage"],
  ["perSession", "session"],
  ["teamHistorical", "team"],
];
const laneMetricSeriesRanges = [
  ["3600", "1h"],
  ["21600", "6h"],
  ["86400", "24h"],
];

function renderLaneMetricsPane(lane) {
  if (!lane.metricsGridEl) return;
  syncLaneMetricSeriesControlHandler(lane);
  const model = laneMetricsRenderModel(lane);
  lane.metricsSummaryEl.textContent = model.status;
  if (laneMetricsLitIslandEnabled()) {
    if (laneMetricsLitIslandRenderer) {
      laneMetricsLitIslandRenderer(lane.metricsGridEl, model);
      requestLaneMetricSeries(lane, model);
      return;
    }
    renderLaneMetricsVanilla(lane.metricsGridEl, model);
    requestLaneMetricSeries(lane, model);
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
  requestLaneMetricSeries(lane, model);
}

function renderLaneMetricsVanilla(grid, model) {
  const nodes = model.cells.map((cell) => {
    const slot = "cell:" + cell.label;
    return laneMetricCell(
      cell.label,
      cell.value,
      laneMetricGridSlot(grid, slot),
      slot,
    );
  });
  nodes.push(
    laneMetricSparklineCell(model, laneMetricGridSlot(grid, "activity")),
    laneMetricSeriesControls(model, laneMetricGridSlot(grid, "series-controls")),
    laneMetricSeriesChartCell(model, laneMetricGridSlot(grid, "series-chart")),
  );
  syncLaneMetricElementChildren(grid, nodes);
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
    series: lane.metricSeries || { points: [] },
    seriesControls: laneMetricSeriesControlsModel(lane),
  };
}

function laneMetricSeriesControlsModel(lane) {
  return {
    metric: lane.metricSeriesMetric || "activity",
    lens: lane.metricSeriesLens || "lineage",
    rangeSeconds: String(lane.metricSeriesRangeSeconds || 3600),
    metrics: laneMetricSeriesMetrics,
    lenses: laneMetricSeriesLenses,
    ranges: laneMetricSeriesRanges,
  };
}

function syncLaneMetricSeriesControlHandler(lane) {
  const grid = lane.metricsGridEl;
  if (!grid || grid.__spiceMetricSeriesControlHandler) return;
  if (typeof grid.addEventListener !== "function") return;
  const handler = (event) => {
    const detail = (event && event.detail) || {};
    if (detail.metric) lane.metricSeriesMetric = String(detail.metric);
    if (detail.lens) lane.metricSeriesLens = String(detail.lens);
    if (detail.rangeSeconds)
      lane.metricSeriesRangeSeconds = Math.max(60, Number(detail.rangeSeconds) || 3600);
    lane.metricSeries = null;
    lane.metricSeriesRequestKey = "";
    lane.metricSeriesPendingKey = "";
    lane.metricSeriesQueryEnd = 0;
    renderLaneMetricsPane(lane);
  };
  grid.addEventListener("spice-metric-series-change", handler);
  grid.__spiceMetricSeriesControlHandler = handler;
}

function requestLaneMetricSeries(lane, model) {
  if (typeof liveBusRequest !== "function") return;
  const query = laneMetricSeriesQuery(lane, model);
  if (!query) return;
  const key = JSON.stringify(query);
  if (lane.metricSeriesRequestKey === key || lane.metricSeriesPendingKey === key)
    return;
  lane.metricSeriesPendingKey = key;
  liveBusRequest("metrics.series", { query }).then(
    (message) => {
      if (lane.metricSeriesPendingKey !== key) return;
      lane.metricSeriesPendingKey = "";
      lane.metricSeriesRequestKey = key;
      lane.metricSeries = (message && message.result) || { points: [] };
      renderLaneMetricsPane(lane);
    },
    () => {
      if (lane.metricSeriesPendingKey !== key) return;
      lane.metricSeriesPendingKey = "";
      reportLaneMetricSeriesError();
    },
  );
}

function laneMetricSeriesQuery(lane, model) {
  const controls = model.seriesControls || laneMetricSeriesControlsModel(lane);
  const rangeSeconds = Math.max(60, Number(controls.rangeSeconds) || 3600);
  const end = lane.metricSeriesQueryEnd || Math.floor(Date.now() / 1000);
  lane.metricSeriesQueryEnd = end;
  const query = {
    metric: controls.metric || "activity",
    lens: controls.lens || "lineage",
    start: Math.max(0, end - rangeSeconds),
    end,
    bucketSeconds: laneMetricSeriesBucketSeconds(rangeSeconds),
  };
  if (query.lens === "teamHistorical") {
    if (!lane.teamId) return null;
    query.teamId = lane.teamId;
    return query;
  }
  const agentId = laneMetricSeriesAgentId(lane);
  if (!agentId) return null;
  query.agentId = agentId;
  return query;
}

function laneMetricSeriesAgentId(lane) {
  if (!lane || !lane.targetId) return "";
  if (typeof laneTeamAgentId === "function") return laneTeamAgentId(lane);
  return "";
}

function laneMetricSeriesBucketSeconds(rangeSeconds) {
  return Math.max(60, Math.ceil(rangeSeconds / 24 / 60) * 60);
}

function reportLaneMetricSeriesError() {
  const status =
    typeof window !== "undefined"
      ? /** @type {any} */ (window).setGlobalTransientError
      : null;
  if (typeof status === "function") status("Metric series request failed");
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
  const status = /** @type {any} */ (window).setGlobalTransientError;
  if (typeof status === "function")
    status("Lit metrics island failed: " + String(error));
  throw error;
}

function laneMetricGridSlot(grid, slot) {
  for (const child of grid.children || []) {
    if (child.__spiceLaneMetricSlot === slot) return child;
  }
  return null;
}

function markLaneMetricSlot(element, slot) {
  element.__spiceLaneMetricSlot = slot;
  return element;
}

function syncLaneMetricElementChildren(element, nodes) {
  const children = Array.from(element.children || []);
  if (
    children.length === nodes.length &&
    nodes.every((node, index) => children[index] === node)
  )
    return;
  element.replaceChildren(...nodes);
}

function laneMetricElementWithClass(parent, className, tagName = "span") {
  let element = parent.querySelector("." + className);
  if (!element) {
    element = document.createElement(tagName);
    element.className = className;
  }
  return element;
}

function laneMetricCell(label, value, existing = null, slot = "") {
  const cell = existing || document.createElement("span");
  cell.className = "lane-metric-cell";
  if (slot) markLaneMetricSlot(cell, slot);
  const valueEl = laneMetricElementWithClass(cell, "lane-metric-value");
  valueEl.textContent = value;
  const labelEl = laneMetricElementWithClass(cell, "lane-metric-label");
  labelEl.textContent = label;
  syncLaneMetricElementChildren(cell, [valueEl, labelEl]);
  return cell;
}

function laneMetricSparklineCell(model, existing = null) {
  const cell = laneMetricCell(
    "activity",
    model.activityTotal + " messages",
    existing,
    "activity",
  );
  cell.classList.add("lane-metric-cell--wide");
  let wrap = cell.querySelector(".lane-metric-sparkline");
  if (!wrap) {
    wrap = document.createElement("div");
    wrap.className = "lane-metric-sparkline";
  }
  wrap.className = "lane-metric-sparkline";
  const max = Math.max(1, ...model.sparkline);
  const bars = [];
  for (const value of model.sparkline) {
    const bar = document.createElement("span");
    bar.className = "lane-metric-sparkline-bar";
    bar.style.setProperty(
      "--lane-metric-sparkline-level",
      String(Math.max(1, Math.ceil((value / max) * 8))),
    );
    bars.push(bar);
  }
  wrap.replaceChildren(...bars);
  const valueEl = laneMetricElementWithClass(cell, "lane-metric-value");
  const labelEl = laneMetricElementWithClass(cell, "lane-metric-label");
  syncLaneMetricElementChildren(cell, [valueEl, labelEl, wrap]);
  return cell;
}

function laneMetricSeriesControls(model, existing = null) {
  const controls = model.seriesControls || {};
  const cell = existing || document.createElement("span");
  markLaneMetricSlot(cell, "series-controls");
  cell.className = "lane-metric-series-controls lane-metric-cell--wide";
  syncLaneMetricElementChildren(
    cell,
    [
      laneMetricSeriesSelect("metric", controls.metric, controls.metrics || [], cell),
      laneMetricSeriesSelect("lens", controls.lens, controls.lenses || [], cell),
      laneMetricSeriesSelect(
        "rangeSeconds",
        controls.rangeSeconds,
        controls.ranges || [],
        cell,
      ),
    ],
  );
  return cell;
}

function laneMetricSeriesSelect(name, selectedValue, options, container = null) {
  let select = laneMetricSeriesSelectForName(container, name);
  if (!select) {
    select = document.createElement("select");
    select.__spiceLaneMetricSeriesSelect = name;
    select.setAttribute("aria-label", "Metric " + name);
    if (typeof select.addEventListener === "function")
      select.addEventListener("change", () => {
        select.dispatchEvent(
          new CustomEvent("spice-metric-series-change", {
            bubbles: true,
            detail: { [name]: select.value },
          }),
        );
      });
  }
  select.className = "lane-metric-series-select";
  syncLaneMetricSeriesSelectOptions(select, selectedValue, options);
  return select;
}

function laneMetricSeriesSelectForName(container, name) {
  if (!container) return null;
  for (const child of container.children || []) {
    if (child.__spiceLaneMetricSeriesSelect === name) return child;
  }
  return null;
}

function syncLaneMetricSeriesSelectOptions(select, selectedValue, options) {
  const selected = String(selectedValue || "");
  const optionNodes = [];
  const existingByValue = laneMetricSeriesOptionsByValue(select);
  for (const [value, label] of options) {
    const optionValue = String(value);
    const option = existingByValue.get(optionValue) || document.createElement("option");
    option.value = value;
    option.textContent = label;
    option.selected = optionValue === selected;
    optionNodes.push(option);
  }
  syncLaneMetricElementChildren(select, optionNodes);
  select.value = selected;
}

function laneMetricSeriesOptionsByValue(select) {
  const options = new Map();
  for (const option of select.children || []) options.set(String(option.value), option);
  return options;
}

function laneMetricSeriesChartCell(model, existing = null) {
  const cell = existing || document.createElement("span");
  markLaneMetricSlot(cell, "series-chart");
  cell.className = "lane-metric-series-chart lane-metric-cell--wide";
  const points = Array.isArray((model.series || {}).points)
    ? model.series.points
    : [];
  if (!points.length) {
    let empty = cell.querySelector(".lane-metric-series-empty");
    if (!empty) empty = document.createElement("span");
    empty.className = "lane-metric-series-empty";
    empty.textContent = "no series";
    syncLaneMetricElementChildren(cell, [empty]);
    return cell;
  }
  const svg = laneMetricSeriesSvg(points);
  syncLaneMetricElementChildren(cell, [svg]);
  return cell;
}

function laneMetricSeriesSvg(points) {
  const svg = createSvgElement("svg");
  svg.setAttribute("class", "lane-metric-series-svg");
  svg.setAttribute("viewBox", "0 0 120 36");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "Metric series");
  const values = points.map((point) => Math.max(0, Number(point.value) || 0));
  const max = points.some((point) => typeof point.share !== "undefined")
    ? 1
    : Math.max(1, ...values);
  const width = 120;
  const height = 36;
  const buckets = laneMetricSeriesBuckets(points);
  const bucketIndex = new Map(buckets.map((bucket, index) => [bucket, index]));
  const step = buckets.length > 1 ? width / (buckets.length - 1) : width;
  for (const [seriesIndex, group] of laneMetricSeriesPointGroups(points).entries()) {
    const groupEl = createSvgElement("g");
    groupEl.setAttribute("class", "lane-metric-series-group");
    if (group.agentId) groupEl.setAttribute("data-agent-id", group.agentId);
    groupEl.style.setProperty(
      "--lane-metric-series-color",
      laneMetricSeriesColor(seriesIndex),
    );
    const title = createSvgElement("title");
    title.textContent = group.agentId || "series";
    groupEl.append(title);
    const coords = group.points.map((point) => {
      const bucket = laneMetricSeriesPointBucket(point);
      const index = bucketIndex.has(bucket) ? bucketIndex.get(bucket) || 0 : 0;
      const x = buckets.length > 1 ? index * step : width / 2;
      const value = Math.max(0, Number(point.value) || 0);
      const y = height - (value / max) * (height - 4) - 2;
      return [x, y, point];
    });
    const polyline = createSvgElement("polyline");
    polyline.setAttribute(
      "points",
      coords.map(([x, y]) => x.toFixed(1) + "," + y.toFixed(1)).join(" "),
    );
    polyline.setAttribute("class", "lane-metric-series-line");
    groupEl.append(polyline);
    for (const [x, y, point] of coords) {
      const dot = createSvgElement("circle");
      dot.setAttribute("cx", x.toFixed(1));
      dot.setAttribute("cy", y.toFixed(1));
      dot.setAttribute("r", "1.8");
      dot.setAttribute("class", "lane-metric-series-dot");
      if (point.agentId) dot.setAttribute("data-agent-id", String(point.agentId));
      groupEl.append(dot);
    }
    svg.append(groupEl);
  }
  return svg;
}

function laneMetricSeriesPointGroups(points) {
  const groups = new Map();
  for (const point of points) {
    const agentId = String(point.agentId || "");
    const key = agentId || "series";
    if (!groups.has(key)) groups.set(key, { agentId, points: [] });
    groups.get(key).points.push(point);
  }
  return Array.from(groups.values());
}

function laneMetricSeriesBuckets(points) {
  const buckets = new Set();
  for (const point of points) buckets.add(laneMetricSeriesPointBucket(point));
  return Array.from(buckets).sort((left, right) => left - right);
}

function laneMetricSeriesPointBucket(point) {
  return Number.isFinite(Number(point.bucketStart)) ? Number(point.bucketStart) : 0;
}

function laneMetricSeriesColor(index) {
  const colors = ["#1677ff", "#d9480f", "#2f9e44", "#ae3ec9", "#0ca678", "#f08c00"];
  return colors[index % colors.length];
}

function createSvgElement(tagName) {
  if (typeof document.createElementNS === "function")
    return document.createElementNS("http://www.w3.org/2000/svg", tagName);
  return document.createElement(tagName);
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
