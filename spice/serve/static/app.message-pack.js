// Stretch-aligned cards report their assigned grid cell as rect height, which
// would freeze a fresh card at the span-1 default forever. scrollHeight sees
// the natural content height through the stretch (plus the border, which
// scrollHeight excludes), so a card measures natural when fresh or grown and
// its settled band cell otherwise - a fixed point either way.
const messageCardBorderAllowancePx = 2;

function messagePackItemHeight(node, naturalHeightMode = false) {
  const rectHeight = node.getBoundingClientRect().height;
  if (!node.matches("article[data-message-key]")) return rectHeight;
  if (naturalHeightMode) return naturalMessagePackItemHeight(node);
  const naturalHeight = node.scrollHeight + messageCardBorderAllowancePx;
  return Math.max(rectHeight, naturalHeight);
}

function naturalMessagePackItemHeight(node) {
  const previousSpan = node.style.getPropertyValue("--message-pack-row-span");
  if (previousSpan) node.style.removeProperty("--message-pack-row-span");
  const height = node.scrollHeight + messageCardBorderAllowancePx;
  if (previousSpan)
    node.style.setProperty("--message-pack-row-span", previousSpan);
  return height;
}

// Segments are keyed by the identity of the barrier that opens them, never by
// ordinal position: history backfill or a newly synthesized time rule must not
// shift the key of untouched downstream segments and cascade re-pins while the
// operator is reading.
function messagePackBarrierKey(node) {
  return (
    node.dataset.messageKey ||
    node.dataset.messageTs ||
    node.dataset.historyTargetId ||
    node.className
  );
}

// Derive the track count from geometry, never from computed
// grid-template-columns: the stream uses repeat(auto-fit, ...), which
// collapses empty tracks - once every card sits in column 1, the computed
// track list shrinks to one entry and a computed-style reading locks the
// packer into a single ever-growing column. The auto-fit formula is
// deterministic from the host width, so recompute it directly.
function messagePackColumnCount(lane, host, style) {
  if (!laneIsFusedHost(lane)) return messagePackRegularColumnCount(host, style);
  const geometryColumns = messagePackGeometryColumnCount(host, style, "min");
  return Math.max(
    1,
    Math.min(geometryColumns, laneGroupMemberLanes(lane).length),
  );
}

function messagePackRegularColumnCount(host, style) {
  return messagePackGeometryColumnCount(host, style, "max");
}

function messagePackGeometryColumnCount(host, style, widthMode = "min") {
  if (messagePackViewportForcesSingleColumn()) return 1;
  const inner = messagePackInnerWidth(host, style);
  if (!Number.isFinite(inner) || inner <= 0) return 1;
  const gap = cssPixelValue(style.columnGap);
  const minWidth = cssLengthValue(
    style.getPropertyValue("--message-card-min-width"),
    style,
  );
  const maxWidth = cssLengthValue(
    style.getPropertyValue("--message-card-max-width"),
    style,
  );
  const targetWidth = widthMode === "max" && maxWidth > 0 ? maxWidth : minWidth;
  const trackTarget = Math.min(inner, targetWidth > 0 ? targetWidth : inner);
  if (trackTarget <= 0) return 1;
  return Math.max(1, Math.floor((inner + gap) / (trackTarget + gap)));
}

function messagePackInnerWidth(host, style) {
  return (
    host.clientWidth -
    cssPixelValue(style.paddingLeft) -
    cssPixelValue(style.paddingRight)
  );
}

function messagePackTrackWidth(host, style, columnCount) {
  const columns = Math.max(1, columnCount);
  const inner = messagePackInnerWidth(host, style);
  if (!Number.isFinite(inner) || inner <= 0) return 0;
  const gap = cssPixelValue(style.columnGap);
  return (inner - gap * (columns - 1)) / columns;
}

function messagePackViewportForcesSingleColumn() {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(max-width: 720px)").matches
  );
}

function fusedMessagePackColumn(node, columnCount) {
  const slot = Number.parseInt(node.dataset.accentSlot || "", 10);
  if (!Number.isFinite(slot) || slot < 0) return 0;
  return slot % Math.max(1, columnCount);
}

function setMessagePackColumnSpan(node, columnSpan) {
  const value = columnSpan > 1 ? "span " + columnSpan : "";
  if (node.style.gridColumnEnd !== value) node.style.gridColumnEnd = value;
  if (columnSpan > 1) node.dataset.messagePackColumnSpan = String(columnSpan);
  else delete node.dataset.messagePackColumnSpan;
}

function setMessagePackColumnCount(host, columnCount) {
  const value = String(Math.max(1, columnCount));
  if (host.style.getPropertyValue("--message-pack-column-count") !== value)
    host.style.setProperty("--message-pack-column-count", value);
}

function cssLengthValue(value, style) {
  const text = String(value || "").trim();
  const parsed = Number.parseFloat(text);
  if (!Number.isFinite(parsed)) return 0;
  if (text.endsWith("rem")) {
    const rootSize = Number.parseFloat(
      getComputedStyle(document.documentElement).fontSize,
    );
    return parsed * (Number.isFinite(rootSize) ? rootSize : 16);
  }
  if (text.endsWith("em") && !text.endsWith("rem")) {
    const fontSize = cssPixelValue(style.fontSize) || 16;
    return parsed * fontSize;
  }
  return parsed;
}

function isMessagePackBarrier(node) {
  return node.matches(
    ".compaction-divider, .time-rule, [data-history-sentinel], article:has(.task-directive-stack)",
  );
}

function orderedMessagePackColumn(
  node,
  segmentKey,
  segmentIndex,
  columnCount,
  reflowing,
) {
  const pinned = Number.parseInt(node.dataset.messagePackColumn || "", 10);
  if (
    !reflowing &&
    Number.isInteger(pinned) &&
    pinned >= 0 &&
    pinned < columnCount
  )
    return pinned;
  const column = segmentIndex % Math.max(1, columnCount);
  node.dataset.messagePackSegment = segmentKey;
  node.dataset.messagePackColumn = String(column);
  return column;
}

const messagePackBandRowsDefault = 6;

function messagePackBandRows(host, style, columnCount, rowStride) {
  const parsed = Number.parseInt(
    style.getPropertyValue("--message-pack-band"),
    10,
  );
  const floor = parsed > 0 ? parsed : messagePackBandRowsDefault;
  if (columnCount <= 1 || !Number.isFinite(rowStride) || rowStride <= 0)
    return floor;
  const trackWidth = messagePackTrackWidth(host, style, columnCount);
  if (!Number.isFinite(trackWidth) || trackWidth <= 0) return floor;
  const targetPx = Math.max(72, Math.min(160, trackWidth * 0.25));
  return Math.max(floor, Math.round(targetPx / rowStride));
}

function maxMessagePackRows(columnRows, column, columnSpan) {
  let row = 0;
  for (
    let index = column;
    index < Math.min(columnRows.length, column + columnSpan);
    index += 1
  ) {
    row = Math.max(row, columnRows[index]);
  }
  return row;
}

function fillMessagePackRows(columnRows, column, columnSpan, row) {
  for (
    let index = column;
    index < Math.min(columnRows.length, column + columnSpan);
    index += 1
  ) {
    columnRows[index] = row;
  }
}

function messagePackItemKeys(host) {
  const keys = [];
  for (const node of host.children) {
    if (!isMessagePackItem(node)) continue;
    keys.push(messagePackItemIdentity(node));
  }
  return keys;
}

function messagePackItemIdentity(node) {
  if (node.matches("article[data-message-key]"))
    return "message:" + (node.dataset.messageKey || "");
  if (node.dataset.historyTargetId)
    return "history:" + node.dataset.historyTargetId;
  if (node.dataset.messageTs) return "time:" + node.dataset.messageTs;
  return "barrier:" + messagePackBarrierKey(node);
}

function messagePackShouldReflow(lane, host, columnCount, itemKeys) {
  const state = lane.messagePackLayoutState;
  if (!state) return true;
  if (state.columnCount !== columnCount) return true;
  if (state.hostWidth !== Math.round(host.clientWidth || 0)) return true;
  const previous = state.itemKeys || [];
  return !messagePackKnownSequencePreserved(previous, itemKeys);
}

function messagePackKnownSequencePreserved(previous, current) {
  if (!previous.length) return !current.length;
  const previousSet = new Set(previous);
  let previousIndex = 0;
  let knownCount = 0;
  let matchedCount = 0;
  for (const key of current) {
    if (!previousSet.has(key)) continue;
    knownCount += 1;
    while (previousIndex < previous.length && previous[previousIndex] !== key) {
      previousIndex += 1;
    }
    if (previousIndex >= previous.length) continue;
    matchedCount += 1;
    previousIndex += 1;
  }
  return knownCount > 0 && matchedCount === knownCount;
}

function clearMessagePackPlacement(node) {
  delete node.dataset.messagePackColumn;
  delete node.dataset.messagePackSegment;
  delete node.dataset.messagePackColumnSpan;
}

function commitMessagePackLayoutState(lane, host, columnCount, itemKeys) {
  lane.messagePackLayoutState = {
    columnCount,
    hostWidth: Math.round(host.clientWidth || 0),
    itemKeys: itemKeys.slice(),
  };
}

function setMessagePackProvisionalPosition(
  node,
  columnStart,
  rowStart,
  columnSpan = 1,
) {
  if (node.style.gridColumnStart !== columnStart)
    node.style.gridColumnStart = columnStart;
  const columnEnd = columnSpan > 1 ? "span " + columnSpan : "";
  if (node.style.gridColumnEnd !== columnEnd) node.style.gridColumnEnd = columnEnd;
  const rowValue = String(rowStart);
  if (node.style.gridRowStart !== rowValue) node.style.gridRowStart = rowValue;
}

function setMessagePackPosition(node, columnStart, rowStart, columnSpan = 1) {
  if (node.style.gridColumnStart !== columnStart)
    node.style.gridColumnStart = columnStart;
  setMessagePackColumnSpan(node, columnSpan);
  const rowValue = String(rowStart);
  if (node.style.gridRowStart !== rowValue) node.style.gridRowStart = rowValue;
}
