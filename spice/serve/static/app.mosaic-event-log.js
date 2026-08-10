// Mosaic event-log replay: layout is a pure function of the recorded event
// sequence, so replaying a log reproduces every position exactly.
// This is the shared, DOM-free replay format used by node fixtures and browser
// smokes: record measured candidates and stream events, then replay them
// through the same pure lattice functions that the live path uses.

const MOSAIC_EVENT_LOG_VERSION = 1;
const MOSAIC_GEOMETRY_REPLAY_REASONS = Object.freeze([
  "initial",
  "topology",
  "fonts-ready",
]);
const MOSAIC_GEOMETRY_REPLAY_SOURCES = Object.freeze([
  "stream-render",
  "resize-observer",
  "deferred-reveal",
  "fonts-ready",
  "image-resolution",
]);

function mosaicRequireExactKeys(value, keys, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(label + " must be an object");
  }
  const actual = Object.keys(value).sort();
  const expected = keys.slice().sort();
  if (
    actual.length !== expected.length ||
    actual.some((key, index) => key !== expected[index])
  ) {
    throw new Error(label + " fields must be exactly " + expected.join(", "));
  }
}

function mosaicRequireReplayEnum(value, accepted, label) {
  if (!accepted.includes(value)) {
    throw new Error(label + " is unknown: " + String(value));
  }
  return value;
}

function mosaicGeometryReplaySource(value) {
  return mosaicRequireReplayEnum(
    value,
    MOSAIC_GEOMETRY_REPLAY_SOURCES,
    "geometry replay source",
  );
}

function mosaicNormalizeGeometrySnapshot(snapshot) {
  mosaicRequireExactKeys(
    snapshot,
    ["width", "baseSpan", "trackCount", "M"],
    "geometry replay snapshot",
  );
  const { width, baseSpan, trackCount, M } = snapshot;
  if (!Number.isFinite(width) || width < 0) {
    throw new Error("geometry replay snapshot width must be finite and nonnegative");
  }
  if (!Number.isInteger(baseSpan) || baseSpan <= 0) {
    throw new Error("geometry replay snapshot baseSpan must be a positive integer");
  }
  if (!Number.isInteger(trackCount) || trackCount <= 0) {
    throw new Error("geometry replay snapshot trackCount must be a positive integer");
  }
  if (baseSpan > trackCount) {
    throw new Error("geometry replay snapshot baseSpan exceeds trackCount");
  }
  if (!Number.isFinite(M) || M <= 0) {
    throw new Error("geometry replay snapshot M must be finite and positive");
  }
  return Object.freeze({ width, baseSpan, trackCount, M });
}

function mosaicNormalizeGeometryReplay(value) {
  mosaicRequireExactKeys(
    value,
    ["kind", "reason", "source", "previous", "next"],
    "geometry replay provenance",
  );
  if (value.kind !== "geometry") {
    throw new Error("geometry replay provenance kind must be geometry");
  }
  const reason = mosaicRequireReplayEnum(
    value.reason,
    MOSAIC_GEOMETRY_REPLAY_REASONS,
    "geometry replay reason",
  );
  const source = mosaicGeometryReplaySource(value.source);
  const previous =
    value.previous === null
      ? null
      : mosaicNormalizeGeometrySnapshot(value.previous);
  const next = mosaicNormalizeGeometrySnapshot(value.next);
  if (reason === "initial" && previous !== null) {
    throw new Error("initial geometry replay must not have a previous snapshot");
  }
  if (reason !== "initial" && previous === null) {
    throw new Error(reason + " geometry replay requires a previous snapshot");
  }
  if (reason === "fonts-ready" && source !== "fonts-ready") {
    throw new Error("fonts-ready geometry replay requires the fonts-ready source");
  }
  return Object.freeze({ kind: "geometry", reason, source, previous, next });
}

function mosaicCloneCandidates(candidates) {
  return (candidates || []).map((candidate) => ({
    span: candidate.span,
    n: candidate.n,
  }));
}

function mosaicCreateEventLog(trackCount, freezeDepth) {
  return {
    version: MOSAIC_EVENT_LOG_VERSION,
    trackCount: mosaicTrackCount(trackCount),
    freezeDepth,
    events: [],
  };
}

function mosaicNormalizeEventLogEvent(event) {
  if (event.type === "insert") {
    return {
      type: "insert",
      key: String(event.key),
      creationIndex: event.creationIndex,
      candidates: mosaicCloneCandidates(event.candidates),
    };
  }
  if (event.type === "content-resize") {
    return {
      type: "content-resize",
      key: String(event.key),
      creationIndex: event.creationIndex,
      n: event.n,
    };
  }
  if (event.type === "remove") {
    return {
      type: "remove",
      keys: (event.keys || []).map((key) => String(key)),
    };
  }
  if (event.type === "backfill") {
    return {
      type: "backfill",
      trackCount: mosaicTrackCount(event.trackCount),
      cards: (event.cards || []).map((card) => ({
        key: String(card.key),
        creationIndex: card.creationIndex,
        candidates: mosaicCloneCandidates(card.candidates),
      })),
    };
  }
  if (event.type === "full-replay") {
    const normalized = {
      type: "full-replay",
      cause: String(event.cause || "unspecified"),
      fallbackReason: String(event.fallbackReason || ""),
      olderBarrierKey: String(event.olderBarrierKey || ""),
      newerBarrierKey: String(event.newerBarrierKey || ""),
      trackCount: mosaicTrackCount(event.trackCount),
      cards: (event.cards || []).map((card) => ({
        key: String(card.key),
        creationIndex: card.creationIndex,
        candidates: mosaicCloneCandidates(card.candidates),
      })),
    };
    if (Object.prototype.hasOwnProperty.call(event, "geometryReplay")) {
      normalized.geometryReplay = mosaicNormalizeGeometryReplay(
        event.geometryReplay,
      );
      const snapshots = [
        normalized.geometryReplay.previous,
        normalized.geometryReplay.next,
      ].filter(Boolean);
      if (
        snapshots.some(
          (snapshot) => snapshot.trackCount !== normalized.trackCount,
        )
      ) {
        throw new Error(
          "geometry replay snapshot trackCount conflicts with full replay",
        );
      }
    }
    return normalized;
  }
  if (event.type === "epoch-replay") {
    return {
      type: "epoch-replay",
      cause: String(event.cause || "unspecified"),
      olderBarrierKey: String(event.olderBarrierKey || ""),
      newerBarrierKey: String(event.newerBarrierKey || ""),
      trackCount: mosaicTrackCount(event.trackCount),
      replacedKeys: (event.replacedKeys || []).map((key) => String(key)),
      cards: (event.cards || []).map((card) => ({
        key: String(card.key),
        creationIndex: card.creationIndex,
        candidates: mosaicCloneCandidates(card.candidates),
      })),
    };
  }
  throw new Error("unknown mosaic event-log event type: " + event.type);
}

function mosaicRecordEventLogEvent(log, event) {
  if (!log) return log;
  log.events.push(mosaicNormalizeEventLogEvent(event));
  return log;
}

function mosaicEventLogLayout(cards) {
  return cards.map((card) => ({
    key: card.key,
    creationIndex: card.creationIndex,
    t: card.t,
    b: card.b,
    n: card.n,
    span: card.span,
    frozen: Boolean(card.frozen),
  }));
}

function mosaicReplayEventLogInsert(cards, event, trackCount, freezeDepth) {
  const rowFloor = mosaicDeriveRowFloor(cards, trackCount);
  const placement = mosaicInsert(cards, rowFloor, event.candidates, trackCount);
  const card = {
    ...placement.card,
    key: event.key,
    creationIndex: event.creationIndex,
    frozen: false,
  };
  return mosaicRecomputeFrozen(cards.concat([card]), freezeDepth);
}

function mosaicReplayEventLogContentResize(cards, event, trackCount, freezeDepth) {
  const card = cards.find(
    (candidate) =>
      candidate.key === event.key ||
      candidate.creationIndex === event.creationIndex,
  );
  if (!card) return cards;
  if (card.frozen) {
    return mosaicResolveFrozenResize(cards, card.creationIndex, event.n);
  }
  const resized = cards.map((candidate) =>
    candidate.creationIndex === card.creationIndex
      ? { ...candidate, n: event.n }
      : candidate,
  );
  return mosaicWetReplay(resized, trackCount, freezeDepth);
}

function mosaicEventLogDownwardRowFloor(cards, trackCount) {
  const tracks = mosaicTrackCount(trackCount);
  const bottomByTrack = new Array(tracks).fill(0);
  for (const card of cards) {
    const start = Math.max(0, card.t);
    const end = Math.min(tracks, card.t + card.span);
    for (let track = start; track < end; track += 1) {
      if (card.b < bottomByTrack[track]) bottomByTrack[track] = card.b;
    }
  }
  const anchor = Math.max(...bottomByTrack);
  return {
    anchor,
    rowFloor: bottomByTrack.map((bottom) => anchor - bottom),
  };
}

function mosaicReplayEventLogBackfill(cards, event, freezeDepth) {
  const trackCount = mosaicTrackCount(event.trackCount);
  const downward = mosaicEventLogDownwardRowFloor(cards, trackCount);
  let rowFloor = downward.rowFloor;
  const placedCards = [];
  for (const entry of event.cards || []) {
    const placement = mosaicInsert([], rowFloor, entry.candidates, trackCount);
    rowFloor = placement.rowFloor;
    placedCards.push({
      ...placement.card,
      key: entry.key,
      creationIndex: entry.creationIndex,
      b: downward.anchor - placement.card.b - placement.card.n,
      frozen: true,
    });
  }
  return mosaicRecomputeFrozen(cards.concat(placedCards), freezeDepth);
}

function mosaicReplayEventLogFullReplay(cards, event, freezeDepth) {
  const trackCount = mosaicTrackCount(event.trackCount);
  const replayInput = event.cards.map((entry) => {
    const previous = cards.find((card) => card.key === entry.key);
    const firstCandidate = entry.candidates[0] || { span: trackCount, n: 1 };
    return {
      key: entry.key,
      creationIndex: entry.creationIndex,
      frozen: false,
      candidates: mosaicCloneCandidates(entry.candidates),
      t: previous ? previous.t : 0,
      b: previous ? previous.b : 0,
      n: previous ? previous.n : 0,
      span: previous ? previous.span : firstCandidate.span,
    };
  });
  return mosaicFullReplay(replayInput, trackCount, freezeDepth).map(
    ({ candidates, ...card }) => card,
  );
}

function mosaicReplayEventLogEpochReplay(cards, event, freezeDepth) {
  const replayed = mosaicEpochReplay(
    cards,
    event.replacedKeys,
    event.cards,
    event.trackCount,
    freezeDepth,
  );
  return replayed.map(({ candidates, ...card }) => card);
}

function mosaicReplayEventLog(log) {
  if (!log || log.version !== MOSAIC_EVENT_LOG_VERSION) {
    throw new Error("unsupported mosaic event-log version");
  }
  let trackCount = mosaicTrackCount(log.trackCount);
  const fallbackFreezeDepth =
    typeof MOSAIC_FREEZE_DEPTH === "number" ? MOSAIC_FREEZE_DEPTH : 2;
  const freezeDepth = Number.isFinite(log.freezeDepth)
    ? log.freezeDepth
    : fallbackFreezeDepth;
  let cards = [];
  for (const event of log.events || []) {
    if (event.type === "insert") {
      cards = mosaicReplayEventLogInsert(cards, event, trackCount, freezeDepth);
    } else if (event.type === "content-resize") {
      cards = mosaicReplayEventLogContentResize(
        cards,
        event,
        trackCount,
        freezeDepth,
      );
    } else if (event.type === "remove") {
      const removed = new Set(event.keys || []);
      cards = cards.filter((card) => !removed.has(card.key));
    } else if (event.type === "backfill") {
      trackCount = mosaicTrackCount(event.trackCount);
      cards = mosaicReplayEventLogBackfill(cards, event, freezeDepth);
    } else if (event.type === "full-replay") {
      trackCount = mosaicTrackCount(event.trackCount);
      cards = mosaicReplayEventLogFullReplay(cards, event, freezeDepth);
    } else if (event.type === "epoch-replay") {
      trackCount = mosaicTrackCount(event.trackCount);
      cards = mosaicReplayEventLogEpochReplay(cards, event, freezeDepth);
    } else {
      throw new Error("unknown mosaic event-log event type: " + event.type);
    }
  }
  return {
    cards,
    layout: mosaicEventLogLayout(cards),
    rowFloor: mosaicDeriveRowFloor(cards, trackCount),
    trackCount,
    freezeDepth,
  };
}
