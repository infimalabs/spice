// Stretch-aligned cards report their assigned grid cell as rect height, which
// would freeze a fresh card at the span-1 default forever. scrollHeight sees
// the natural content height through the stretch (plus the border, which
// scrollHeight excludes), so a card measures natural when fresh, grown, or
// already settled into its grid cell - a fixed point either way.
const messageCardBorderAllowancePx = 2;
const messagePackGridTrackCount = 12;
const messagePackMaxEffectiveColumns = 6;
const messagePackEffectiveColumnCounts = [6, 4, 3, 2, 1];
const messagePackLayoutVersion = 5;
const messagePackLegalTrackSpans = [2, 3, 4, 6, 12];
const messagePackSlackPenalty = 3;
const messagePackTallFinalMinHeightPx = 280;
const messagePackHashInitial = 2166136261;
const messagePackHashMultiplier = 16777619;

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
  const height = node.matches("article.image-only[data-message-key]")
    ? node.getBoundingClientRect().height
    : node.matches("article[data-message-key]")
      ? node.scrollHeight + messageCardBorderAllowancePx
      : node.getBoundingClientRect().height;
  if (previousSpan)
    node.style.setProperty("--message-pack-row-span", previousSpan);
  return height;
}

function messagePackReservedSpan(node, naturalSpan, rowGap, rowStride) {
  const type = messagePackReservationType(node);
  if (!type || typeof mosaicReservationRows !== "function") return naturalSpan;
  const rows = mosaicReservationRows(
    type,
    messagePackRootFontSizePx(),
    rowGap,
    rowStride,
  );
  return Number.isFinite(rows) ? Math.max(naturalSpan, rows) : naturalSpan;
}

function messagePackReservationType(node) {
  if (!node || !node.dataset) return "";
  return node.dataset.mosaicReservationType || "";
}

function messagePackRootFontSizePx() {
  if (typeof mosaicRootFontSizePx === "function") return mosaicRootFontSizePx();
  const parsed = Number.parseFloat(
    getComputedStyle(document.documentElement).fontSize,
  );
  return Number.isFinite(parsed) ? parsed : 16;
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

function messagePackColumnCount(lane, host, style) {
  if (messagePackViewportForcesSingleColumn()) return 1;
  const inner = messagePackInnerWidth(host, style);
  if (!Number.isFinite(inner) || inner <= 0) return 1;
  const gap = cssPixelValue(style.columnGap);
  const minWidth = cssLengthValue(
    style.getPropertyValue("--message-card-min-width"),
    style,
  );
  const floorWidth = minWidth > 0 ? minWidth : inner;
  for (const count of messagePackEffectiveColumnCounts) {
    if (count > messagePackMaxEffectiveColumns) continue;
    if (messagePackEffectiveColumnWidth(inner, gap, count) + 0.5 >= floorWidth)
      return count;
  }
  return 1;
}

function messagePackEffectiveColumnWidth(inner, gap, count) {
  const columns = Math.max(1, count);
  return (inner - gap * (columns - 1)) / columns;
}

// A lone visible lane's own width already answers the "should this lane use
// the full swimlanes width" question honestly: .lane{flex:1 1 0} in
// .swimlanes{display:flex} already stretches it to fill all available
// space when it is the only flex item, so host.clientWidth here (and in
// mosaic-geometry's mosaicContainerWidthPx for the live render path)
// already reflects the correct width with no packing-time override needed.
function messagePackInnerWidth(host, style) {
  return (
    host.clientWidth -
    cssPixelValue(style.paddingLeft) -
    cssPixelValue(style.paddingRight)
  );
}

function messagePackViewportForcesSingleColumn() {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(max-width: 720px)").matches
  );
}

function setMessagePackColumnSpan(node, columnSpan) {
  const value = columnSpan > 1 ? "span " + columnSpan : "";
  if (node.style.gridColumnEnd !== value) node.style.gridColumnEnd = value;
  if (columnSpan > 1) {
    const spanValue = String(columnSpan);
    node.dataset.messagePackColumnSpan = spanValue;
    node.style.setProperty("--message-pack-column-span", spanValue);
  } else {
    delete node.dataset.messagePackColumnSpan;
    node.style.removeProperty("--message-pack-column-span");
  }
}

function setMessagePackColumnCount(host, columnCount) {
  const value = String(Math.max(1, columnCount));
  if (host.style.getPropertyValue("--message-pack-column-count") !== value) {
    host.style.setProperty("--message-pack-column-count", value);
    return true;
  }
  return false;
}

function setMessagePackTrackMin(host, value) {
  if (host.style.getPropertyValue("--message-pack-track-min") !== value) {
    host.style.setProperty("--message-pack-track-min", value);
    return true;
  }
  return false;
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
  segmentSeed,
  columnCount,
  columnRows,
  slotSpan,
  placementStep,
  reflowing,
) {
  const pinned = messagePackPinnedColumn(
    node,
    columnCount,
    slotSpan,
    placementStep,
    reflowing,
  );
  if (pinned >= 0) return pinned;
  const column = messagePackBestColumn(
    segmentIndex,
    segmentSeed,
    columnCount,
    columnRows,
    slotSpan,
    placementStep,
  );
  node.dataset.messagePackSegment = segmentKey;
  node.dataset.messagePackColumn = String(column);
  return column;
}

function messagePackPinnedColumn(
  node,
  columnCount,
  slotSpan,
  placementStep,
  reflowing,
) {
  const pinned = Number.parseInt(node.dataset.messagePackColumn || "", 10);
  const step = Math.max(1, placementStep);
  if (
    !reflowing &&
    Number.isInteger(pinned) &&
    pinned >= 0 &&
    pinned + slotSpan <= columnCount &&
    pinned % step === 0
  )
    return pinned;
  return -1;
}

function messagePackBestColumn(
  segmentIndex,
  segmentSeed,
  columnCount,
  columnRows,
  slotSpan,
  placementStep,
) {
  const rightToLeft = messagePackPlacementRightToLeft(
    segmentIndex,
    columnCount,
  );
  const preferredColumn = messagePackPlacementPreferredColumn(
    segmentIndex,
    segmentSeed,
    columnCount,
    slotSpan,
    placementStep,
  );
  const step = Math.max(1, placementStep);
  let best = { column: 0, row: Infinity, score: Infinity };
  for (
    let candidate = 0;
    candidate <= Math.max(0, columnCount - slotSpan);
    candidate += step
  ) {
    const candidateMetrics = messagePackPlacementMetrics(
      columnRows,
      candidate,
      slotSpan,
    );
    const current = { column: candidate, ...candidateMetrics };
    if (messagePackPlacementBeats(current, best, preferredColumn, rightToLeft))
      best = current;
  }
  return best.column;
}

function messagePackPlacementPreferredColumn(
  segmentIndex,
  segmentSeed,
  columnCount,
  slotSpan,
  placementStep,
) {
  const step = Math.max(1, placementStep);
  const candidateCount = messagePackPlacementCandidateCount(
    columnCount,
    slotSpan,
    step,
  );
  if (candidateCount <= 1) return 0;
  const sequenceIndex = Math.floor(Math.max(0, segmentIndex) / step);
  const preferredIndex = (sequenceIndex + segmentSeed) % candidateCount;
  return preferredIndex * step;
}

function messagePackPlacementCandidateCount(columnCount, slotSpan, step) {
  return Math.floor(Math.max(0, columnCount - slotSpan) / step) + 1;
}

function messagePackPlacementRightToLeft(segmentIndex, columnCount) {
  if (columnCount <= 1) return false;
  return Math.floor(segmentIndex / columnCount) % 2 === 1;
}

function messagePackPlacementBeats(
  candidate,
  current,
  preferredColumn,
  rightToLeft,
) {
  if (candidate.score < current.score) return true;
  if (candidate.score > current.score) return false;
  if (candidate.row < current.row) return true;
  if (candidate.row > current.row) return false;
  const candidateDistance = Math.abs(candidate.column - preferredColumn);
  const currentDistance = Math.abs(current.column - preferredColumn);
  if (candidateDistance < currentDistance) return true;
  if (candidateDistance > currentDistance) return false;
  if (rightToLeft) return candidate.column > current.column;
  return candidate.column < current.column;
}

function messagePackPlacementMetrics(columnRows, column, slotSpan) {
  const row = maxMessagePackRows(columnRows, column, slotSpan);
  let slack = 0;
  for (
    let index = column;
    index < Math.min(columnRows.length, column + slotSpan);
    index += 1
  ) {
    slack += row - columnRows[index];
  }
  return {
    row,
    score: row + slack * messagePackSlackPenalty,
  };
}

function messagePackColumnSpan(columnCount) {
  return Math.max(
    1,
    Math.floor(messagePackGridTrackCount / Math.max(1, columnCount)),
  );
}

function messagePackTrackSpan(node, minimumTrackSpan, itemHeight = 0) {
  const floor = Math.max(1, minimumTrackSpan);
  if (node.classList.contains("final"))
    return messagePackPreferredTrackSpan(
      floor,
      itemHeight > messagePackTallFinalMinHeightPx ? 6 : 4,
    );
  if (
    node.classList.contains("media-rich") ||
    node.classList.contains("image-only")
  )
    return messagePackPreferredTrackSpan(floor, 4);
  if (node.classList.contains("acked"))
    return messagePackPreferredTrackSpan(floor, 3);
  return floor;
}

function messagePackPreferredTrackSpan(minimumTrackSpan, preferredTrackSpan) {
  const target = Math.max(minimumTrackSpan, preferredTrackSpan);
  for (const span of messagePackLegalTrackSpans) {
    if (span >= target) return span;
  }
  return messagePackGridTrackCount;
}

function messagePackSegmentSeed(segmentKey) {
  if (!segmentKey || segmentKey === "top") return 0;
  let hash = messagePackHashInitial;
  for (let index = 0; index < segmentKey.length; index += 1) {
    hash ^= segmentKey.charCodeAt(index);
    hash = Math.imul(hash, messagePackHashMultiplier) >>> 0;
  }
  return hash;
}

function messagePackBarrierSegmentSeed(node, segmentKey) {
  if (node.dataset.historyTargetId) return 0;
  return messagePackSegmentSeed(segmentKey);
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
  if (state.version !== messagePackLayoutVersion) return true;
  if (state.columnCount !== columnCount) return true;
  const previous = state.itemKeys || [];
  if (!messagePackKnownSequencePreserved(previous, itemKeys)) return true;
  return messagePackPinnedLayoutCollapsed(host, columnCount);
}

function messagePackKnownSequencePreserved(previous, current) {
  if (!previous.length) return !current.length;
  const previousSet = new Set(previous);
  let bestCurrentStart = 0;
  let bestLength = 0;
  for (
    let previousStart = 0;
    previousStart < previous.length;
    previousStart += 1
  ) {
    for (
      let currentStart = 0;
      currentStart < current.length;
      currentStart += 1
    ) {
      let length = 0;
      while (
        previousStart + length < previous.length &&
        currentStart + length < current.length &&
        previous[previousStart + length] === current[currentStart + length]
      ) {
        length += 1;
      }
      if (length > bestLength) {
        bestCurrentStart = currentStart;
        bestLength = length;
      }
    }
  }
  if (bestLength <= 0) return false;
  const before = current.slice(0, bestCurrentStart);
  const after = current.slice(bestCurrentStart + bestLength);
  return ![...before, ...after].some((key) => previousSet.has(key));
}

function messagePackPinnedLayoutCollapsed(host, columnCount) {
  if (columnCount <= 1) return false;
  const minimumTrackSpan = messagePackColumnSpan(columnCount);
  const step = Math.max(1, minimumTrackSpan);
  const possibleStarts = new Set();
  const pinnedStarts = new Set();
  let pinnedCount = 0;
  let packableCount = 0;
  for (const node of host.children) {
    if (!isMessagePackItem(node) || isMessagePackBarrier(node)) continue;
    packableCount += 1;
    const slotSpan = messagePackTrackSpan(node, minimumTrackSpan, 0);
    for (
      let candidate = 0;
      candidate <= Math.max(0, messagePackGridTrackCount - slotSpan);
      candidate += step
    ) {
      possibleStarts.add(candidate);
    }
    const pinned = Number.parseInt(node.dataset.messagePackColumn || "", 10);
    if (
      Number.isInteger(pinned) &&
      pinned >= 0 &&
      pinned + slotSpan <= messagePackGridTrackCount &&
      pinned % step === 0
    ) {
      pinnedCount += 1;
      pinnedStarts.add(pinned);
    }
  }
  const expectedStarts = Math.min(columnCount, possibleStarts.size, packableCount);
  if (expectedStarts <= 1 || pinnedCount < expectedStarts) return false;
  const coverageFloor = Math.min(
    expectedStarts,
    Math.max(2, Math.ceil(expectedStarts * 0.75)),
  );
  return pinnedStarts.size < coverageFloor;
}

function clearMessagePackPlacement(node) {
  delete node.dataset.messagePackColumn;
  delete node.dataset.messagePackSegment;
  delete node.dataset.messagePackColumnSpan;
  node.style.removeProperty("--message-pack-column-span");
}

function commitMessagePackLayoutState(lane, columnCount, itemKeys) {
  lane.messagePackLayoutState = {
    columnCount,
    itemKeys: itemKeys.slice(),
    version: messagePackLayoutVersion,
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
  setMessagePackColumnSpan(node, columnSpan);
  const rowValue = String(rowStart);
  if (node.style.gridRowStart !== rowValue) node.style.gridRowStart = rowValue;
}

function setMessagePackPosition(
  node,
  columnStart,
  rowStart,
  columnSpan = 1,
) {
  if (node.style.gridColumnStart !== columnStart)
    node.style.gridColumnStart = columnStart;
  setMessagePackColumnSpan(node, columnSpan);
  const rowValue = String(rowStart);
  if (node.style.gridRowStart !== rowValue) node.style.gridRowStart = rowValue;
}
