// Mosaic event-log replay (spec .spice/mosaic-packing-spec.md §10, §11d).
// This is the shared, DOM-free replay format used by node fixtures and browser
// smokes: record measured candidates and stream events, then replay them
// through the same pure lattice functions that the live path uses.

const MOSAIC_EVENT_LOG_VERSION = 1;

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
  if (event.type === "full-replay") {
    return {
      type: "full-replay",
      trackCount: mosaicTrackCount(event.trackCount),
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
    } else if (event.type === "full-replay") {
      trackCount = mosaicTrackCount(event.trackCount);
      cards = mosaicReplayEventLogFullReplay(cards, event, freezeDepth);
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
