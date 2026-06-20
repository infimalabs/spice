"""Serve speech text preparation runs in the browser audio script."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
AUDIO_JS = PROJECT_ROOT / "spice" / "serve" / "static" / "app.audio.js"
STREAM_JS = PROJECT_ROOT / "spice" / "serve" / "static" / "app.stream.js"


def test_speech_text_preparation_strips_markdown_link_targets():
    assert _prepare_speech("See [docs](https://example.test/path?q=1).") == "See docs."


def test_speech_text_preparation_speaks_git_hash_prefix():
    assert _prepare_speech("Merged a21c3c1 and f647d55b7ec9.") == (
        "Merged commit a 2 1 c 3 c 1 and commit f 6 4 7 d 5 5."
    )


def test_speech_text_preparation_keeps_existing_commit_label():
    assert _prepare_speech("See commit a21c3c1 and sha f647d55.") == (
        "See commit a 2 1 c 3 c 1 and sha f 6 4 7 d 5 5."
    )


def test_speech_text_preparation_speaks_utc_datetimes():
    assert _prepare_speech("Started 2026-06-12T01:20:30.123Z.") == (
        "Started June 12, 2026 at 1:20:30 AM UTC."
    )


def test_speech_item_utterances_use_prepared_text_once():
    result = _speech_utterances_for_item(
        {
            "ack_utterances": [
                "ACK [thread](spice-session://abc) at 2026-06-12T13:05Z",
                "ACK [thread](spice-session://abc) at 2026-06-12T13:05Z",
            ]
        }
    )

    assert result == ["ACK thread at June 12, 2026 at 1:05 PM UTC"]


def test_speak_mode_speaks_ack_utterances_and_final_messages():
    ack_item = {
        "kind": "assistant",
        "ack_utterances": ["ACK first paragraph.\n\nACK last paragraph."],
    }
    final_item = {
        "kind": "final",
        "display_text": "First final paragraph.\n\nMiddle final paragraph.\n\nLast final paragraph.",
    }
    final_ack_item = {
        "kind": "final",
        "ack_utterances": ["ACK body should not override final excerpt."],
        "display_text": "First final ACK paragraph.\n\nHidden middle.\n\nLast final ACK paragraph.",
    }

    assert _automatic_speech_utterances("speak", ack_item) == [
        "ACK first paragraph. ACK last paragraph."
    ]
    assert _automatic_speech_utterances("speak", final_item) == [
        "First final paragraph.",
        "Last final paragraph.",
    ]
    assert _automatic_speech_utterances("speak", final_ack_item) == [
        "First final ACK paragraph.",
        "Last final ACK paragraph.",
    ]


def test_quiet_and_narrate_keep_their_speech_contracts():
    final_item = {"kind": "final", "display_text": "Final answer ready."}
    assistant_item = {
        "kind": "assistant",
        "display_text": "First paragraph.\n\nMiddle paragraph.\n\nLast paragraph.",
    }

    assert _automatic_speech_utterances("quiet", final_item) == []
    assert _automatic_speech_utterances("narrate", assistant_item) == [
        "First paragraph.",
        "Last paragraph.",
    ]


def test_manual_speech_playback_reads_all_display_paragraphs():
    assistant_item = {
        "kind": "assistant",
        "ack_utterances": ["ACK body should not override manual playback."],
        "display_text": "First paragraph.\n\nMiddle paragraph.\n\nLast paragraph.",
    }
    final_item = {
        "kind": "final",
        "display_text": "First final paragraph.\n\nMiddle final paragraph.\n\nLast final paragraph.",
    }

    assert _message_speech_utterances(assistant_item) == [
        "First paragraph.",
        "Middle paragraph.",
        "Last paragraph.",
    ]
    assert _message_speech_utterances(final_item) == [
        "First final paragraph.",
        "Middle final paragraph.",
        "Last final paragraph.",
    ]


def test_speech_session_updates_page_title_and_media_metadata():
    assert _speech_session_title_states("Ops Console") == {
        "activeTitle": "Ops Console - Matilda",
        "activeMediaTitle": "Ops Console - Matilda",
        "activeMediaArtist": "Ops Console",
        "activePlaybackState": "playing",
        "idleTitle": "Ops Console",
        "idleMediaTitle": "Ops Console",
        "idlePlaybackState": "none",
    }


def test_speech_session_title_falls_back_without_branding_global():
    assert _speech_session_title_states()["idleTitle"] == "spice"


def test_narration_mode_holds_media_session_playing_for_speak_lanes():
    assert _narration_media_session_states() == {
        "speakOnlyPlaybackState": "none",
        "narrationPlaybackState": "playing",
        "closedNarrationPlaybackState": "none",
        "actions": ["play", "pause", "stop"],
    }


def test_manual_speech_playback_aborts_active_entry_remaining_utterances():
    assert _manual_speech_interrupt_requests() == ["old first", "manual"]


def test_normal_speech_queue_preserves_entries_while_audio_is_active():
    assert _normal_speech_queue_requests() == ["first", "second"]


def test_speech_queue_preserves_couple_pending_entries_across_agents():
    assert _speech_queue_requests_for_entries(
        [
            {"lane": "a", "key": "active-key", "text": "active"},
            {"lane": "b", "key": "bravo-key", "text": "bravo"},
            {"lane": "a", "key": "alpha-key", "text": "alpha"},
        ]
    ) == ["active", "bravo", "alpha"]


def test_natural_clip_end_pause_preserves_final_tail():
    assert _natural_clip_end_pause_requests() == [
        "First final paragraph.",
        "Last final paragraph.",
    ]


def test_speech_queue_clears_global_pending_backlog_when_behind():
    assert _speech_queue_requests_for_entries(
        [
            {"lane": "a", "key": "active-key", "text": "active"},
            {"lane": "b", "key": "bravo-key", "text": "bravo"},
            {"lane": "a", "key": "alpha-key", "text": "alpha"},
            {"lane": "b", "key": "current-key", "text": "current"},
        ]
    ) == ["active", "current"]


def test_stop_clears_pending_queue_across_lanes():
    assert _stop_clears_pending_across_lanes() == ["active"]


def test_external_pause_clears_pending_queue():
    assert _external_pause_clears_pending() == ["active"]


def test_external_pause_during_narration_preserves_speak_queue():
    assert _external_pause_during_narration_preserves_pending() == ["active", "bravo"]


def test_speech_burst_never_overlaps_audio():
    assert _burst_max_concurrent_audio() == 1


def test_initial_payload_speech_keeps_startup_ack_from_becoming_silent_baseline():
    assert _initial_payload_speech_keys() == ["startup-race-ack", "fresh-ack"]


def test_automatic_speech_tracks_latest_played_timestamp_per_agent():
    assert _automatic_speech_cursor_requests() == [
        "latest",
        "other agent older",
        "newer",
    ]


def _prepare_speech(text: str) -> str:
    return _node_call("prepareSpeechText", text)


def _speech_utterances_for_item(item: dict[str, object]) -> list[str]:
    return _node_call("speechUtterancesForItem", item)


def _message_speech_utterances(item: dict[str, object]) -> list[str]:
    return _node_call("messageSpeechUtterances", item)


def _automatic_speech_utterances(mode: str, item: dict[str, object]) -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const mode = process.argv[2];
const item = JSON.parse(process.argv[3]);
const context = {
  laneEffectiveSpeechMode: () => mode,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);
process.stdout.write(
  JSON.stringify(context.automaticSpeechUtterances({ speechMode: mode }, item)),
);
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS), mode, json.dumps(item)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node automatic speech failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _speech_session_title_states(brand: str | None = None) -> dict[str, str]:
    branding = (
        ""
        if brand is None
        else f"  spiceServeBranding: {json.dumps({'name': brand})},\n"
    )
    script = (
        """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
class FakeMediaMetadata {
  constructor(value) {
    Object.assign(this, value);
  }
}
const context = {
"""
        + branding
        + """
  document: {
    title: "spice - Simultaneous Production, Integration, and Control Environment",
    querySelectorAll: () => [],
  },
  navigator: { mediaSession: { metadata: null } },
  MediaMetadata: FakeMediaMetadata,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);
vm.runInContext(
  "currentSpeech = {" +
    "lane: { agentName: 'Jamie', branchName: 'main' }," +
    "targetLane: { agentName: 'Matilda', branchName: 'main-1' }," +
    "messageKey: 'message-1'" +
  "}; syncSpeechButtons();",
  context,
);
const active = {
  activeTitle: context.document.title,
  activeMediaTitle: context.navigator.mediaSession.metadata.title,
  activeMediaArtist: context.navigator.mediaSession.metadata.artist,
  activePlaybackState: context.navigator.mediaSession.playbackState,
};
vm.runInContext("currentSpeech = null; syncSpeechButtons();", context);
process.stdout.write(JSON.stringify({
  ...active,
  idleTitle: context.document.title,
  idleMediaTitle: context.navigator.mediaSession.metadata.title,
  idlePlaybackState: context.navigator.mediaSession.playbackState,
}));
"""
    )
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node speech session title failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _narration_media_session_states() -> dict[str, object]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const actions = [];
class FakeMediaMetadata {
  constructor(value) {
    Object.assign(this, value);
  }
}
const context = {
  document: {
    title: "spice",
    querySelectorAll: () => [],
  },
  navigator: {
    mediaSession: {
      metadata: null,
      playbackState: "none",
      setActionHandler: (name) => actions.push(name),
    },
  },
  MediaMetadata: FakeMediaMetadata,
  laneStates: new Map(),
  laneEffectiveSpeechMode: (lane) => lane.speechMode,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);
const speakLane = { targetId: "speak", speechMode: "speak" };
const narrationLane = { targetId: "narrate", speechMode: "narrate" };
context.laneStates.set("speak", speakLane);
context.syncNarrationMediaSession();
const speakOnlyPlaybackState = context.navigator.mediaSession.playbackState;
context.laneStates.set("narrate", narrationLane);
context.syncNarrationMediaSession();
const narrationPlaybackState = context.navigator.mediaSession.playbackState;
narrationLane.closed = true;
context.syncNarrationMediaSession();
process.stdout.write(JSON.stringify({
  speakOnlyPlaybackState,
  narrationPlaybackState,
  closedNarrationPlaybackState: context.navigator.mediaSession.playbackState,
  actions,
}));
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node narration media session states failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _manual_speech_interrupt_requests() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const requests = [];
const audioInstances = [];
const failTimer = setTimeout(() => {
  console.error("manual speech was not requested");
  process.exit(1);
}, 1000);
let firstRequestedResolve;
let firstAudioResolve;
let manualRequestedResolve;
const firstRequested = new Promise((resolve) => {
  firstRequestedResolve = resolve;
});
const firstAudioReady = new Promise((resolve) => {
  firstAudioResolve = resolve;
});
const manualRequested = new Promise((resolve) => {
  manualRequestedResolve = resolve;
});

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.index = audioInstances.length;
    audioInstances.push(this);
    if (this.index === 0) firstAudioResolve();
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    if (this.index > 0) queueMicrotask(() => this.listeners.ended());
    return Promise.resolve();
  }
  pause() {
    if (this.listeners.pause) this.listeners.pause();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    const text = JSON.parse(options.body).text;
    requests.push(text);
    if (requests.length === 1) firstRequestedResolve();
    if (text === "manual") manualRequestedResolve();
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const lane = {
    targetId: "lane-a",
    speechAbortVersion: 0,
    spokenMessageKeys: new Set(),
  };
  context.enqueueSpeech(lane, "old", ["old first", "old second"]);
  await firstRequested;
  await firstAudioReady;
  context.toggleMessageSpeech(lane, { key: "manual-key", display_text: "manual" });
  await manualRequested;
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node manual speech interrupt failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _normal_speech_queue_requests() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const requests = [];
const audioInstances = [];
const failTimer = setTimeout(() => {
  console.error("queued speech was not preserved");
  process.exit(1);
}, 1000);
let firstRequestedResolve;
let firstAudioResolve;
let secondRequestedResolve;
const firstRequested = new Promise((resolve) => {
  firstRequestedResolve = resolve;
});
const firstAudioReady = new Promise((resolve) => {
  firstAudioResolve = resolve;
});
const secondRequested = new Promise((resolve) => {
  secondRequestedResolve = resolve;
});

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.index = audioInstances.length;
    audioInstances.push(this);
    if (this.index === 0) firstAudioResolve();
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    return Promise.resolve();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    const text = JSON.parse(options.body).text;
    requests.push(text);
    if (text === "first") firstRequestedResolve();
    if (text === "second") secondRequestedResolve();
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const lane = {
    targetId: "lane-a",
    speechAbortVersion: 0,
    spokenMessageKeys: new Set(),
  };
  context.enqueueSpeech(lane, "first-key", ["first"]);
  await firstRequested;
  await firstAudioReady;
  context.enqueueSpeech(lane, "second-key", ["second"]);
  await Promise.resolve();
  if (requests.length !== 1) {
    throw new Error("normal enqueue requested queued speech before active audio ended");
  }
  audioInstances[0].listeners.ended();
  await secondRequested;
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node normal speech queue failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _speech_queue_requests_for_entries(entries: list[dict[str, str]]) -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const entries = JSON.parse(process.argv[2]);
const requests = [];
const audioInstances = [];
const finalText = entries[entries.length - 1].text;
const failTimer = setTimeout(() => {
  console.error("queued speech did not finish");
  process.exit(1);
}, 1000);
let firstRequestedResolve;
let firstAudioResolve;
let finalRequestedResolve;
const firstRequested = new Promise((resolve) => {
  firstRequestedResolve = resolve;
});
const firstAudioReady = new Promise((resolve) => {
  firstAudioResolve = resolve;
});
const finalRequested = new Promise((resolve) => {
  finalRequestedResolve = resolve;
});

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.index = audioInstances.length;
    audioInstances.push(this);
    if (this.index === 0) firstAudioResolve();
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    if (this.index > 0) queueMicrotask(() => this.listeners.ended());
    return Promise.resolve();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    const text = JSON.parse(options.body).text;
    requests.push(text);
    if (requests.length === 1) firstRequestedResolve();
    if (text === finalText) finalRequestedResolve();
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const lanes = new Map();
  const laneFor = (id) => {
    if (!lanes.has(id)) {
      lanes.set(id, {
        targetId: "lane-" + id,
        speechAbortVersion: 0,
        spokenMessageKeys: new Set(),
      });
    }
    return lanes.get(id);
  };
  const [active, ...pending] = entries;
  context.enqueueSpeech(laneFor(active.lane), active.key, [active.text]);
  await firstRequested;
  await firstAudioReady;
  for (const entry of pending)
    context.enqueueSpeech(laneFor(entry.lane), entry.key, [entry.text]);
  await Promise.resolve();
  if (requests.length !== 1) {
    throw new Error("queued speech requested before active audio ended");
  }
  audioInstances[0].listeners.ended();
  await finalRequested;
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS), json.dumps(entries)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node speech queue backlog failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _natural_clip_end_pause_requests() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const requests = [];
const audioInstances = [];
const failTimer = setTimeout(() => {
  console.error("final tail was not requested");
  process.exit(1);
}, 1000);
let tailRequestedResolve;
const tailRequested = new Promise((resolve) => {
  tailRequestedResolve = resolve;
});

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.index = audioInstances.length;
    this.ended = false;
    audioInstances.push(this);
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    if (this.index === 0) {
      queueMicrotask(() => {
        this.ended = true;
        this.listeners.pause();
      });
    } else {
      queueMicrotask(() => this.listeners.ended());
    }
    return Promise.resolve();
  }
  pause() {
    if (this.listeners.pause) this.listeners.pause();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    const text = JSON.parse(options.body).text;
    requests.push(text);
    if (text === "Last final paragraph.") tailRequestedResolve();
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const lane = {
    targetId: "lane-a",
    speechAbortVersion: 0,
    spokenMessageKeys: new Set(),
  };
  context.enqueueSpeech(lane, "final-key", [
    "First final paragraph.",
    "Last final paragraph.",
  ]);
  await tailRequested;
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node natural pause final tail failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _initial_payload_speech_keys() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const context = {};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);
const lane = { speechPrimeStartedAt: Date.parse("2026-06-17T04:00:00.000Z") };
const messages = [
  { key: "stale-history", timestamp: "2026-06-17T03:59:54.999Z" },
  { key: "startup-race-ack", timestamp: "2026-06-17T03:59:55.000Z" },
  { key: "fresh-ack", timestamp: "2026-06-17T04:00:00.000Z" },
  { key: "invalid-time", timestamp: "not-a-date" },
];
process.stdout.write(JSON.stringify(
  context.initialPayloadSpeechMessages(lane, messages).map((item) => item.key),
));
"""
    result = subprocess.run(
        ["node", "-e", script, str(STREAM_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node initial payload speech failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _automatic_speech_cursor_requests() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const requests = [];

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.ended = false;
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    queueMicrotask(() => {
      this.ended = true;
      if (this.listeners.ended) this.listeners.ended();
    });
    return Promise.resolve();
  }
  pause() {
    if (this.listeners.pause) this.listeners.pause();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    requests.push(JSON.parse(options.body).text);
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  setTimeout,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const lane = {
    targetId: "lane-a",
    targetThreadId: "agent-a",
    speechAbortVersion: 0,
    spokenMessageKeys: new Set(),
    latestSpokenMessageAtByAgent: new Map(),
  };
  context.queueSpeechForMessages(lane, [
    {
      key: "latest",
      timestamp: "2026-06-17T04:00:10.000Z",
      threadId: "agent-a",
      ack_utterances: ["latest"],
    },
  ]);
  context.queueSpeechForMessages(lane, [
    {
      key: "older",
      timestamp: "2026-06-17T04:00:09.000Z",
      threadId: "agent-a",
      ack_utterances: ["older"],
    },
  ]);
  context.queueSpeechForMessages(lane, [
    {
      key: "other-agent",
      timestamp: "2026-06-17T04:00:09.000Z",
      threadId: "agent-b",
      ack_utterances: ["other agent older"],
    },
  ]);
  context.queueSpeechForMessages(lane, [
    {
      key: "newer",
      timestamp: "2026-06-17T04:00:11.000Z",
      threadId: "agent-a",
      ack_utterances: ["newer"],
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 40));
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    return _run_audio_script(script, "node automatic speech cursor failed")


def _stop_clears_pending_across_lanes() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const requests = [];
const audioInstances = [];
const failTimer = setTimeout(() => {
  console.error("active speech was not requested");
  process.exit(1);
}, 1000);
let firstRequestedResolve;
let firstAudioResolve;
const firstRequested = new Promise((resolve) => {
  firstRequestedResolve = resolve;
});
const firstAudioReady = new Promise((resolve) => {
  firstAudioResolve = resolve;
});

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.index = audioInstances.length;
    audioInstances.push(this);
    if (this.index === 0) firstAudioResolve();
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    if (this.index > 0) queueMicrotask(() => this.listeners.ended());
    return Promise.resolve();
  }
  pause() {
    if (this.listeners.pause) this.listeners.pause();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    const text = JSON.parse(options.body).text;
    requests.push(text);
    if (text === "active") firstRequestedResolve();
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  setTimeout,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const laneA = { targetId: "lane-a", speechAbortVersion: 0, spokenMessageKeys: new Set() };
  const laneB = { targetId: "lane-b", speechAbortVersion: 0, spokenMessageKeys: new Set() };
  context.enqueueSpeech(laneA, "active-key", ["active"]);
  await firstRequested;
  await firstAudioReady;
  context.enqueueSpeech(laneB, "bravo-key", ["bravo"]);
  await Promise.resolve();
  // Stop by toggling off the active message: the entire queue must clear, so
  // the cross-lane "bravo" entry never reaches playback.
  context.toggleMessageSpeech(laneA, { key: "active-key", display_text: "active" });
  await new Promise((resolve) => setTimeout(resolve, 30));
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    return _run_audio_script(script, "node stop-clears-queue failed")


def _external_pause_clears_pending() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const requests = [];
const audioInstances = [];
const failTimer = setTimeout(() => {
  console.error("active speech was not requested");
  process.exit(1);
}, 1000);
let firstRequestedResolve;
let firstAudioResolve;
const firstRequested = new Promise((resolve) => {
  firstRequestedResolve = resolve;
});
const firstAudioReady = new Promise((resolve) => {
  firstAudioResolve = resolve;
});

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.index = audioInstances.length;
    audioInstances.push(this);
    if (this.index === 0) firstAudioResolve();
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    if (this.index > 0) queueMicrotask(() => this.listeners.ended());
    return Promise.resolve();
  }
  pause() {
    if (this.listeners.pause) this.listeners.pause();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    const text = JSON.parse(options.body).text;
    requests.push(text);
    if (text === "active") firstRequestedResolve();
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  setTimeout,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const laneA = { targetId: "lane-a", speechAbortVersion: 0, spokenMessageKeys: new Set() };
  const laneB = { targetId: "lane-b", speechAbortVersion: 0, spokenMessageKeys: new Set() };
  context.enqueueSpeech(laneA, "active-key", ["active"]);
  await firstRequested;
  await firstAudioReady;
  context.enqueueSpeech(laneB, "bravo-key", ["bravo"]);
  await Promise.resolve();
  // An external pause (OS / media key) fires 'pause' on the active element
  // without our intentional marker: it must stop everything, so the queued
  // "bravo" never plays.
  audioInstances[0].listeners.pause();
  await new Promise((resolve) => setTimeout(resolve, 30));
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    return _run_audio_script(script, "node external-pause-clears-queue failed")


def _external_pause_during_narration_preserves_pending() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const requests = [];
const audioInstances = [];
const failTimer = setTimeout(() => {
  console.error("active speech was not requested");
  process.exit(1);
}, 1000);
let firstRequestedResolve;
let firstAudioResolve;
const firstRequested = new Promise((resolve) => {
  firstRequestedResolve = resolve;
});
const firstAudioReady = new Promise((resolve) => {
  firstAudioResolve = resolve;
});

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.index = audioInstances.length;
    audioInstances.push(this);
    if (this.index === 0) firstAudioResolve();
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    delete this.listeners[name];
  }
  play() {
    if (this.index > 0) queueMicrotask(() => this.listeners.ended());
    return Promise.resolve();
  }
  pause() {
    if (this.listeners.pause) this.listeners.pause();
  }
}

const narrationLane = {
  targetId: "narration",
  speechMode: "narrate",
};
const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    const text = JSON.parse(options.body).text;
    requests.push(text);
    if (text === "active") firstRequestedResolve();
    return { ok: true, arrayBuffer: async () => new ArrayBuffer(1) };
  },
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: (lane) => lane.speechMode,
  laneGroupHost: (lane) => lane,
  laneStates: new Map([["narration", narrationLane]]),
  queueMicrotask,
  setTimeout,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const laneA = { targetId: "lane-a", speechMode: "speak", speechAbortVersion: 0, spokenMessageKeys: new Set() };
  const laneB = { targetId: "lane-b", speechMode: "speak", speechAbortVersion: 0, spokenMessageKeys: new Set() };
  context.enqueueSpeech(laneA, "active-key", ["active"]);
  await firstRequested;
  await firstAudioReady;
  context.enqueueSpeech(laneB, "bravo-key", ["bravo"]);
  await Promise.resolve();
  // With any open lane in narration mode, a raw audio pause from mobile
  // lock/backgrounding is recoverable: settle the clip but keep speak-only
  // entries queued behind the shared narration session.
  audioInstances[0].listeners.pause();
  await new Promise((resolve) => setTimeout(resolve, 30));
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    return _run_audio_script(
        script,
        "node narration-external-pause-preserves-queue failed",
    )


def _burst_max_concurrent_audio() -> int:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const audioInstances = [];
let active = 0;
let maxActive = 0;
const failTimer = setTimeout(() => {
  console.error("burst speech never settled");
  process.exit(1);
}, 1000);

class FakeAudio {
  constructor() {
    this.listeners = {};
    this.counted = false;
    audioInstances.push(this);
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  removeEventListener(name) {
    if (name === "pause" && this.counted) {
      active -= 1;
      this.counted = false;
    }
    delete this.listeners[name];
  }
  play() {
    active += 1;
    this.counted = true;
    if (active > maxActive) maxActive = active;
    queueMicrotask(() => {
      if (this.listeners.ended) this.listeners.ended();
    });
    return Promise.resolve();
  }
  pause() {
    if (this.listeners.pause) this.listeners.pause();
  }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: {
    createObjectURL: () => "blob:audio",
    revokeObjectURL: () => {},
  },
  document: { querySelectorAll: () => [] },
  fetch: async () => ({ ok: true, arrayBuffer: async () => new ArrayBuffer(1) }),
  isPresenceMessage: () => false,
  laneEffectiveSpeechMode: () => "speak",
  laneGroupHost: (lane) => lane,
  queueMicrotask,
  setTimeout,
  targetApi: (targetId, suffix) => targetId + suffix,
};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);

(async () => {
  const lanes = new Map();
  const laneFor = (id) => {
    if (!lanes.has(id))
      lanes.set(id, { targetId: "lane-" + id, speechAbortVersion: 0, spokenMessageKeys: new Set() });
    return lanes.get(id);
  };
  // Burst many ACK/final-style utterances across lanes back to back.
  const burst = [["a", "one"], ["b", "two"], ["a", "three"], ["b", "four"], ["a", "five"]];
  for (let i = 0; i < burst.length; i += 1)
    context.enqueueSpeech(laneFor(burst[i][0]), "key-" + i, [burst[i][1]]);
  await new Promise((resolve) => setTimeout(resolve, 50));
  clearTimeout(failTimer);
  process.stdout.write(JSON.stringify(maxActive));
})().catch((error) => {
  clearTimeout(failTimer);
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            "node burst overlap check failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _run_audio_script(script: str, failure: str) -> list[str]:
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            f"{failure}:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _node_call(function_name: str, value: object):
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const functionName = process.argv[2];
const value = JSON.parse(process.argv[3]);
const context = {};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);
process.stdout.write(JSON.stringify(context[functionName](value)));
"""
    result = subprocess.run(
        ["node", "-e", script, str(AUDIO_JS), function_name, json.dumps(value)],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(
            f"node speech prep failed:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)
