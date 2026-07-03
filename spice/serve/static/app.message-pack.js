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
  const geometryColumns = messagePackGeometryColumnCount(host, style);
  if (!laneIsFusedHost(lane)) return geometryColumns;
  return Math.max(
    1,
    Math.min(geometryColumns, laneGroupMemberLanes(lane).length),
  );
}

function messagePackGeometryColumnCount(host, style) {
  if (messagePackViewportForcesSingleColumn()) return 1;
  const inner =
    host.clientWidth -
    cssPixelValue(style.paddingLeft) -
    cssPixelValue(style.paddingRight);
  if (!Number.isFinite(inner) || inner <= 0) return 1;
  const gap = cssPixelValue(style.columnGap);
  const minWidth = cssLengthValue(
    style.getPropertyValue("--message-card-min-width"),
    style,
  );
  const trackMin = Math.min(inner, minWidth > 0 ? minWidth : inner);
  if (trackMin <= 0) return 1;
  return Math.max(1, Math.floor((inner + gap) / (trackMin + gap)));
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

const messagePackTallSpanColumns = 2;
const messagePackTallSpanBandThreshold = 4;

function messagePackColumnSpan(
  node,
  columnCount,
  fusedHost,
  naturalSpan,
  bandRows,
) {
  if (fusedHost || columnCount < messagePackTallSpanColumns) return 1;
  if (!node.matches("article[data-message-key]")) return 1;
  if (node.classList.contains("image-only")) return 1;
  if (isMessagePackBarrier(node)) return 1;
  const previous = Number.parseInt(node.dataset.messagePackColumnSpan || "", 10);
  if (
    Number.isInteger(previous) &&
    previous > 1 &&
    previous <= columnCount
  )
    return Math.min(previous, messagePackTallSpanColumns);
  if (naturalSpan >= bandRows * messagePackTallSpanBandThreshold)
    return messagePackTallSpanColumns;
  return 1;
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

function stickyMessagePackColumn(node, segmentKey, columnRows, bandRows) {
  const pinned = Number(node.dataset.messagePackColumn);
  if (
    node.dataset.messagePackSegment === segmentKey &&
    Number.isInteger(pinned) &&
    pinned >= 0 &&
    pinned < columnRows.length
  )
    return pinned;
  const column = leftmostFeasiblePackColumn(columnRows, bandRows);
  node.dataset.messagePackSegment = segmentKey;
  node.dataset.messagePackColumn = String(column);
  return column;
}

function stickyMessagePackColumnSpanStart(
  node,
  segmentKey,
  columnRows,
  columnSpan,
  bandRows,
) {
  const pinned = Number(node.dataset.messagePackColumn);
  const pinnedSpan = Number.parseInt(
    node.dataset.messagePackColumnSpan || "",
    10,
  );
  if (
    node.dataset.messagePackSegment === segmentKey &&
    Number.isInteger(pinned) &&
    pinnedSpan === columnSpan &&
    pinned >= 0 &&
    pinned + columnSpan <= columnRows.length
  )
    return pinned;
  const column = leftmostFeasiblePackColumnSpanStart(
    columnRows,
    columnSpan,
    bandRows,
  );
  node.dataset.messagePackSegment = segmentKey;
  node.dataset.messagePackColumn = String(column);
  return column;
}

// One band of tolerance turns shortest-column masonry into row-major filling:
// a level fills left to right, then the next level starts, so chronological
// insertion keeps each visual band a contiguous time slice. The lowest column
// is always feasible, so the scan always returns.
const messagePackBandRowsDefault = 6;

function messagePackBandRows(style) {
  const parsed = Number.parseInt(
    style.getPropertyValue("--message-pack-band"),
    10,
  );
  return parsed > 0 ? parsed : messagePackBandRowsDefault;
}

function leftmostFeasiblePackColumn(columnRows, bandRows) {
  // Strictly less than one band: a column that already took a full band is
  // spent for this level, so equal-height cards fan left-to-right instead of
  // stacking pairs into the leftmost column.
  const lowest = Math.min(...columnRows);
  for (let index = 0; index < columnRows.length; index += 1) {
    if (columnRows[index] < lowest + bandRows) return index;
  }
  return columnRows.indexOf(lowest);
}

function leftmostFeasiblePackColumnSpanStart(columnRows, columnSpan, bandRows) {
  const starts = Math.max(1, columnRows.length - columnSpan + 1);
  const lowest = Math.min(...columnRows);
  let fallbackColumn = 0;
  let fallbackRow = Infinity;
  let bestColumn = null;
  let bestRow = Infinity;
  for (let column = 0; column < starts; column += 1) {
    const row = maxMessagePackRows(columnRows, column, columnSpan);
    if (row < fallbackRow) {
      fallbackColumn = column;
      fallbackRow = row;
    }
    const leftColumnAtSameRow = columnRows
      .slice(0, column)
      .some((value) => value === row);
    if (leftColumnAtSameRow) continue;
    if (row < bestRow) {
      bestColumn = column;
      bestRow = row;
    }
    if (row < lowest + bandRows) return column;
  }
  return bestColumn ?? fallbackColumn;
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
