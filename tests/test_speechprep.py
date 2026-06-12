"""Serve speech text preparation runs in the browser audio script."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
AUDIO_JS = PROJECT_ROOT / "spice" / "serve" / "static" / "app.audio.js"


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
            "speech_utterances": [
                "ACK [thread](spice-session://abc) at 2026-06-12T13:05Z",
                "ACK [thread](spice-session://abc) at 2026-06-12T13:05Z",
            ]
        }
    )

    assert result == ["ACK thread at June 12, 2026 at 1:05 PM UTC"]


def test_speak_mode_speaks_ack_utterances_and_final_messages():
    ack_item = {"kind": "assistant", "speech_utterances": ["ACK finished."]}
    final_item = {"kind": "final", "display_text": "Final answer ready."}

    assert _automatic_speech_utterances("speak", ack_item) == ["ACK finished."]
    assert _automatic_speech_utterances("speak", final_item) == ["Final answer ready."]


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


def test_speech_session_updates_page_title_and_media_metadata():
    assert _speech_session_title_states() == {
        "activeTitle": "spice - Matilda",
        "activeMediaTitle": "spice - Matilda",
        "activeMediaArtist": "spice",
        "idleTitle": "spice",
        "idleMediaTitle": "spice",
    }


def test_manual_speech_playback_aborts_active_entry_remaining_utterances():
    assert _manual_speech_interrupt_requests() == ["old first", "manual"]


def test_normal_speech_queue_preserves_entries_while_audio_is_active():
    assert _normal_speech_queue_requests() == ["first", "second"]


def _prepare_speech(text: str) -> str:
    return _node_call("prepareSpeechText", text)


def _speech_utterances_for_item(item: dict[str, object]) -> list[str]:
    return _node_call("speechUtterancesForItem", item)


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


def _speech_session_title_states() -> dict[str, str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
class FakeMediaMetadata {
  constructor(value) {
    Object.assign(this, value);
  }
}
const context = {
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
};
vm.runInContext("currentSpeech = null; syncSpeechButtons();", context);
process.stdout.write(JSON.stringify({
  ...active,
  idleTitle: context.document.title,
  idleMediaTitle: context.navigator.mediaSession.metadata.title,
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
            "node speech session title failed:\n"
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
  context.toggleMessageSpeech(lane, "manual-key", ["manual"]);
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
