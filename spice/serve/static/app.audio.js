// Speech: one global sequential queue across lanes. Speak mode plays explicit
// ACK utterances, with final-answer bodies summarized to edge paragraphs;
// narrate summarizes visible message bodies to edges. Manual play reads the
// visible body. Markdown images are described rather than read. The transcript
// remains the record — playback is best-effort ear candy and never blocks the
// stream.

const speechPlayIconSvg =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5v14l11-7Z" fill="currentColor"/></svg>';
const speechStopIconSvg =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor"/></svg>';

const speechQueue = [];
let speechBusy = false;
let currentSpeech = null;
// Bumped by every hard reset (stop, manual play, external pause). Each queued
// entry records the epoch it was enqueued under; the drain abandons any entry
// whose epoch is stale, so a single stop clears the whole pipeline regardless
// of lane.
let speechEpoch = 0;
// Single-owner playback: at most one audio element may sound at a time. The
// active element is hard-stopped before any new clip starts, and the token
// lets a late-resolving play() that has since been superseded stop itself.
let activePlaybackAudio = null;
let playbackGeneration = 0;
// Elements we pause deliberately (supersession or stop). Their 'pause' event is
// a controlled settle; an unmarked 'pause' is an external stop (OS/media key)
// and clears the entire queue.
const intentionallyPaused = new WeakSet();
const defaultDocumentTitle =
  String(
    (typeof spiceServeBranding === "object" &&
      spiceServeBranding &&
      spiceServeBranding.name) ||
      "spice",
  ).trim() || "spice";
const speechQueueBacklogClearThreshold = 2;
const hoursPerHalfDay = 12;
const gitHashContextChars = 16;
let speechMediaSessionHandlersInstalled = false;

function queueSpeechForMessages(lane, messages) {
  const host = laneGroupHost(lane);
  if (laneEffectiveSpeechMode(host) === "quiet") return;
  for (const item of [...messages].reverse()) {
    if (!item.key) continue;
    if (isPresenceMessage(item)) continue;
    if (lane.spokenMessageKeys.has(item.key)) continue;
    const timestamp = automaticSpeechMessageTimestamp(item);
    if (messageIsBehindAutomaticSpeechCursor(host, item, timestamp)) continue;
    const texts = automaticSpeechUtterances(host, item);
    if (!texts.length) continue;
    lane.spokenMessageKeys.add(item.key);
    recordAutomaticSpeechCursor(host, item, timestamp);
    enqueueSpeech(lane, item.key, texts);
  }
}

function automaticSpeechMessageTimestamp(item) {
  const timestamp = Date.parse(item.timestamp || "");
  return Number.isFinite(timestamp) ? timestamp : null;
}

function messageIsBehindAutomaticSpeechCursor(lane, item, timestamp) {
  if (timestamp === null) return false;
  const agentKey = automaticSpeechAgentKey(lane, item);
  if (!agentKey) return false;
  const cursors = automaticSpeechCursorMap(lane);
  const latest = cursors.get(agentKey);
  return Number.isFinite(latest) && timestamp < latest;
}

function recordAutomaticSpeechCursor(lane, item, timestamp) {
  if (timestamp === null) return;
  const agentKey = automaticSpeechAgentKey(lane, item);
  if (!agentKey) return;
  const cursors = automaticSpeechCursorMap(lane);
  const latest = cursors.get(agentKey);
  if (!Number.isFinite(latest) || timestamp > latest) cursors.set(agentKey, timestamp);
}

function automaticSpeechCursorMap(lane) {
  if (!lane.latestSpokenMessageAtByAgent)
    lane.latestSpokenMessageAtByAgent = new Map();
  return lane.latestSpokenMessageAtByAgent;
}

function automaticSpeechAgentKey(lane, item) {
  return (
    item.threadId ||
    item.producerTargetId ||
    lane.targetThreadId ||
    lane.activeThreadId ||
    lane.targetId ||
    ""
  );
}

function primeSpeechBoundary(lane) {
  for (const item of lane.knownMessages) lane.spokenMessageKeys.add(item.key);
  lane.speechPrimed = true;
}

// Automatic speech speaks edges, not essays: final answers and narrated bodies
// read the first and last visible paragraphs. Non-final ACK messages use the
// whole extracted ACK body; final ACK messages follow final-answer excerpting.
function automaticSpeechUtterances(lane, item) {
  const mode = laneEffectiveSpeechMode(lane);
  if (mode === "quiet") return [];
  if (item.kind === "final") {
    return speechUtterancesForItem(item, {
      includeDisplayBody: true,
      includeAckUtterances: false,
    });
  }
  if (itemHasAckUtterances(item)) return speechUtterancesForItem(item);
  if (mode === "narrate") {
    return speechUtterancesForItem(item, { includeDisplayBody: true });
  }
  return speechUtterancesForItem(item);
}

function speechUtterancesForItem(item, options = {}) {
  if (item.image_only) return [];
  const includeDisplayBody = Boolean(options.includeDisplayBody);
  const includeFullDisplayBody = Boolean(options.includeFullDisplayBody);
  const includeAckUtterances = options.includeAckUtterances !== false;
  const utterances = [];

  if (includeAckUtterances) {
    for (const utterance of item.ack_utterances || []) {
      appendSpeechUtterance(utterances, utterance);
    }
  }
  if (includeDisplayBody || includeFullDisplayBody) {
    const displayBody = item.display_text || item.text;
    const paragraphs = includeFullDisplayBody
      ? speechParagraphs(displayBody)
      : edgeSpeechParagraphs(displayBody);
    for (const utterance of paragraphs) {
      appendSpeechUtterance(utterances, utterance);
    }
  }
  return utterances;
}

function itemHasAckUtterances(item) {
  for (const utterance of item.ack_utterances || []) {
    if (stripSpeechText(utterance)) return true;
  }
  return false;
}

function appendSpeechUtterance(utterances, raw) {
  const text = stripSpeechText(raw);
  if (text && !utterances.includes(text)) utterances.push(text);
}

function speechParagraphs(raw) {
  const normalized = String(raw || "").replace(/\r\n?/g, "\n");
  const paragraphs = [];
  for (const paragraph of normalized.split(/\n[ \t]*\n/)) {
    const stripped = paragraph
      .split("\n")
      .filter((line) => !/^\s*::[a-z][a-z0-9-]*\{.*\}\s*$/.test(line))
      .join("\n")
      .trim();
    if (stripped) paragraphs.push(stripped);
  }
  return paragraphs;
}

function edgeSpeechParagraphs(raw) {
  const paragraphs = speechParagraphs(raw);
  if (paragraphs.length <= 1) return paragraphs;
  const first = paragraphs[0];
  const last = paragraphs[paragraphs.length - 1];
  return first === last ? [first] : [first, last];
}

const markdownImagePattern = /!\[([^\]]*)\]\([^)]*\)/g;
const markdownLinkPattern = /\[([^\]]+)\]\((?:\\.|[^)])+\)/g;
const gitHashPattern = /\b[0-9a-f]{7,40}\b/gi;
const utcDateTimePattern =
  /\b(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})(?::(\d{2})(?:\.\d{1,6})?)?(?:Z|\+00:00)\b/g;
const utcMonthNames = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];

function stripSpeechText(raw) {
  return prepareSpeechText(raw);
}

function prepareSpeechText(raw) {
  return String(raw || "")
    .replace(markdownImagePattern, (match, alt) =>
      alt && alt.trim() ? "Image: " + alt.trim() + "." : "An image.",
    )
    .replace(markdownLinkPattern, (match, label) => label.trim())
    .replace(utcDateTimePattern, (
      match,
      year,
      month,
      day,
      hour,
      minute,
      second,
    ) => speakUtcDateTime(year, month, day, hour, minute, second || ""))
    .replace(gitHashPattern, (match, offset, source) =>
      speakGitHash(match, gitHashNeedsCommitLabel(source, offset)),
    )
    .replace(/\s+/g, " ")
    .trim();
}

function gitHashNeedsCommitLabel(source, offset) {
  const prefix = source
    .slice(Math.max(0, offset - gitHashContextChars), offset)
    .toLowerCase();
  return !/\b(commit|sha)\s*$/.test(prefix);
}

function speakGitHash(raw, includeLabel = true) {
  const prefix = String(raw).slice(0, 7).toLowerCase();
  const spoken = prefix.split("").join(" ");
  return includeLabel ? "commit " + spoken : spoken;
}

function speakUtcDateTime(year, month, day, hour, minute, second) {
  const hour24 = Number(hour);
  const hour12 = hour24 % hoursPerHalfDay || hoursPerHalfDay;
  const seconds = second ? ":" + String(Number(second)).padStart(2, "0") : "";
  const meridiem = hour24 < hoursPerHalfDay ? "AM" : "PM";
  return (
    utcMonthNames[Number(month) - 1] +
    " " +
    Number(day) +
    ", " +
    year +
    " at " +
    hour12 +
    ":" +
    minute +
    seconds +
    " " +
    meridiem +
    " UTC"
  );
}

function enqueueSpeech(lane, messageKey, texts, targetLane = lane) {
  if (speechQueue.length >= speechQueueBacklogClearThreshold)
    speechQueue.length = 0;
  speechQueue.push({
    lane,
    targetLane,
    messageKey,
    texts,
    abortVersion: lane.speechAbortVersion,
    epoch: speechEpoch,
  });
  drainSpeechQueue();
}

function toggleMessageSpeech(lane, item, targetLane = lane) {
  const messageKey = item.key;
  const texts = messageSpeechUtterances(item);
  if (!messageKey || !texts.length) return;
  // A manual play (or stop) is a hard reset: clear the entire queue and halt
  // whatever is sounding, then — unless this was a toggle-off of the active
  // message — play only this one message, uninterrupted.
  const wasPlaying = Boolean(
    currentSpeech && currentSpeech.messageKey === messageKey,
  );
  stopAllSpeech();
  if (wasPlaying) return;
  enqueueSpeech(lane, messageKey, texts, targetLane);
}

function messageIsCurrentSpeech(lane, item) {
  return Boolean(
    item.key && currentSpeech && currentSpeech.messageKey === item.key,
  );
}

function messageSpeechUtterances(item) {
  return speechUtterancesForItem(item, {
    includeFullDisplayBody: true,
    includeAckUtterances: false,
  });
}

function applySpeechButtonState(button, playing) {
  button.innerHTML = playing ? speechStopIconSvg : speechPlayIconSvg;
  button.title = playing ? "Stop playback" : "Play message";
  button.classList.toggle("speech-button--playing", playing);
}

function syncSpeechButtons() {
  syncSpeechSessionMetadata();
  for (const button of document.querySelectorAll("[data-speech-for]")) {
    const speechButton = /** @type {HTMLButtonElement} */ (button);
    applySpeechButtonState(
      speechButton,
      Boolean(
        currentSpeech &&
          currentSpeech.messageKey === speechButton.dataset.speechFor,
      ),
    );
  }
  syncNowPlayingMessages();
}

function syncSpeechSessionMetadata() {
  const title = currentSpeech
    ? speechSessionTitle(currentSpeech)
    : defaultDocumentTitle;
  if (typeof document !== "undefined") document.title = title;
  const session = speechMediaSession();
  if (!session) return;
  ensureSpeechMediaSessionHandlers(session);
  try {
    session.playbackState = speechMediaSessionPlaybackState();
    if (typeof MediaMetadata !== "undefined")
      session.metadata = new MediaMetadata({
        title,
        artist: defaultDocumentTitle,
      });
  } catch {
    return;
  }
}

function syncNarrationMediaSession() {
  syncSpeechSessionMetadata();
}

function speechMediaSession() {
  if (typeof navigator === "undefined") return null;
  return navigator.mediaSession || null;
}

function ensureSpeechMediaSessionHandlers(session) {
  if (
    speechMediaSessionHandlersInstalled ||
    typeof session.setActionHandler !== "function"
  ) {
    return;
  }
  session.setActionHandler("play", () => drainSpeechQueue());
  session.setActionHandler("pause", () => stopAllSpeech());
  session.setActionHandler("stop", () => stopAllSpeech());
  speechMediaSessionHandlersInstalled = true;
}

function speechMediaSessionPlaybackState() {
  return currentSpeech || narrationMediaSessionActive() ? "playing" : "none";
}

function narrationMediaSessionActive() {
  if (typeof laneStates === "undefined") return false;
  for (const lane of laneStates.values()) {
    if (lane.closed) continue;
    if (laneEffectiveSpeechMode(lane) === "narrate") return true;
  }
  return false;
}

function speechSessionTitle(entry) {
  const lane = entry.targetLane || entry.lane || {};
  const name = lane.agentName || lane.branchName || "";
  return name ? defaultDocumentTitle + " - " + name : defaultDocumentTitle;
}

function syncNowPlayingMessages() {
  const messageKey = currentSpeech ? currentSpeech.messageKey : "";
  for (const article of document.querySelectorAll("article[data-message-key]")) {
    const messageArticle = /** @type {HTMLElement} */ (article);
    messageArticle.classList.toggle(
      "now-playing",
      Boolean(messageKey) && messageArticle.dataset.messageKey === messageKey,
    );
  }
}

function abortLaneSpeech(lane) {
  lane.speechAbortVersion += 1;
  for (let index = speechQueue.length - 1; index >= 0; index -= 1) {
    if (speechQueue[index].lane === lane) speechQueue.splice(index, 1);
  }
  if (currentSpeech && currentSpeech.lane === lane) stopCurrentSpeech();
}

// Hard reset of the whole speech pipeline: drop every queued entry across all
// lanes, advance the epoch so any in-flight drain abandons its entry, and stop
// whatever is currently sounding. Stop and manual play both route through here.
function stopAllSpeech() {
  speechQueue.length = 0;
  speechEpoch += 1;
  stopCurrentSpeech();
}

function stopCurrentSpeech() {
  if (currentSpeech && currentSpeech.audio) {
    try {
      pauseIntentionally(currentSpeech.audio);
    } catch (error) {
      currentSpeech.audio = null;
    }
  }
  if (currentSpeech && currentSpeech.finish) currentSpeech.finish();
}

// Pause an element we own. The marker tells the element's 'pause' handler this
// stop was ours (a controlled settle), not an external OS/media-key pause.
function pauseIntentionally(audio) {
  intentionallyPaused.add(audio);
  audio.pause();
}

async function drainSpeechQueue() {
  if (speechBusy) return;
  speechBusy = true;
  try {
    while (speechQueue.length) {
      const entry = speechQueue.shift();
      if (entry.epoch !== speechEpoch) continue;
      if (entry.abortVersion !== entry.lane.speechAbortVersion) continue;
      currentSpeech = {
        lane: entry.lane,
        targetLane: entry.targetLane,
        messageKey: entry.messageKey,
      };
      syncSpeechButtons();
      for (const text of entry.texts) {
        if (entry.epoch !== speechEpoch) break;
        if (entry.abortVersion !== entry.lane.speechAbortVersion) break;
        await playSpeech(entry.targetLane, text);
      }
      currentSpeech = null;
      syncSpeechButtons();
    }
  } finally {
    speechBusy = false;
    currentSpeech = null;
    syncSpeechButtons();
  }
}

async function playSpeech(lane, text) {
  try {
    const response = await fetch(targetApi(lane.targetId, "/say"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text, rate: 1.0 }),
    });
    if (!response.ok) return;
    const buffer = await response.arrayBuffer();
    await playAudioBuffer(buffer);
  } catch (error) {
    return;
  }
}

function stopActivePlayback() {
  const audio = activePlaybackAudio;
  activePlaybackAudio = null;
  if (!audio) return;
  try {
    pauseIntentionally(audio);
  } catch (error) {
    // A failed pause still drops our reference; the clip is being discarded.
  }
}

function playAudioBuffer(buffer) {
  return new Promise((resolve) => {
    // Claim ownership before creating the clip: any in-flight clip is stopped,
    // and the bumped token supersedes any of its still-pending play() requests.
    const generation = (playbackGeneration += 1);
    stopActivePlayback();
    const blob = new Blob([buffer], { type: "audio/mp4" });
    const url = URL.createObjectURL(blob);
    const audio = new Audio(url);
    activePlaybackAudio = audio;
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      intentionallyPaused.delete(audio);
      audio.removeEventListener("ended", onEnd);
      audio.removeEventListener("error", onEnd);
      audio.removeEventListener("pause", onPause);
      URL.revokeObjectURL(url);
      if (activePlaybackAudio === audio) activePlaybackAudio = null;
      if (currentSpeech) {
        currentSpeech.audio = null;
        currentSpeech.finish = null;
      }
      resolve();
    };
    const onEnd = () => finish();
    const onPause = () => {
      // Without an active narration session, an external pause is a stop. With
      // narration selected, mobile lock/background pauses are treated as
      // recoverable interruptions so the shared speech queue keeps draining.
      // Pauses we initiated, and pause events some browsers emit at natural
      // clip end, are controlled settles and never cascade.
      const external = !audio.ended && !intentionallyPaused.has(audio);
      finish();
      if (external && !narrationMediaSessionActive()) stopAllSpeech();
    };
    if (currentSpeech) {
      currentSpeech.audio = audio;
      currentSpeech.finish = finish;
    }
    audio.addEventListener("ended", onEnd);
    audio.addEventListener("error", onEnd);
    audio.addEventListener("pause", onPause);
    audio.play().then(() => {
      // If a newer clip claimed ownership while play() was pending, this one
      // lost the race after starting: stop it so the two never overlap.
      if (generation !== playbackGeneration) stopOrphanedPlayback(audio);
    }, onEnd);
  });
}

function stopOrphanedPlayback(audio) {
  try {
    pauseIntentionally(audio);
  } catch (error) {
    // pause() fires the listener that settles this clip's promise; ignore.
  }
}
