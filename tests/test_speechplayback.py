"""Serve speech playback: queue ordering, pause/stop, and the per-agent cursor.

These exercise the browser audio script's playback queue and automatic-speech
cursor behavior. Speech text preparation lives in test_speechprep.py; the
materialization gate lives in test_speechnarration.py.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
AUDIO_JS = PROJECT_ROOT / "spice" / "serve" / "static" / "app.audio.js"


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


def test_speech_queue_preserves_first_pending_when_coalescing_backlog():
    assert _speech_queue_requests_for_entries(
        [
            {"lane": "a", "key": "active-key", "text": "active"},
            {"lane": "b", "key": "bravo-key", "text": "bravo"},
            {"lane": "a", "key": "alpha-key", "text": "alpha"},
            {"lane": "b", "key": "current-key", "text": "current"},
        ]
    ) == ["active", "bravo", "current"]


def test_stop_clears_pending_queue_across_lanes():
    assert _stop_clears_pending_across_lanes() == ["active"]


def test_external_pause_clears_pending_queue():
    assert _external_pause_clears_pending() == ["active"]


def test_external_pause_during_narration_preserves_speak_queue():
    assert _external_pause_during_narration_preserves_pending() == ["active", "bravo"]


def test_speech_burst_never_overlaps_audio():
    assert _burst_max_concurrent_audio() == 1


def test_automatic_speech_tracks_latest_played_timestamp_per_agent():
    assert _automatic_speech_cursor_requests() == [
        "latest",
        "other agent older",
        "newer",
    ]


def test_automatic_speech_cursor_survives_page_reload_storage():
    assert _automatic_speech_persisted_cursor_requests() == ["latest", "newer"]


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


def _automatic_speech_persisted_cursor_requests() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const source = fs.readFileSync(path, "utf8");
const requests = [];
const storage = {
  values: new Map(),
  getItem(key) {
    return this.values.has(key) ? this.values.get(key) : null;
  },
  setItem(key, value) {
    this.values.set(key, String(value));
  },
};

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

async function runPage(messages) {
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
    browserStorage: () => storage,
    isPresenceMessage: () => false,
    laneEffectiveSpeechMode: () => "speak",
    laneGroupHost: (lane) => lane,
    queueMicrotask,
    setTimeout,
    targetApi: (targetId, suffix) => targetId + suffix,
  };
  vm.createContext(context);
  vm.runInContext(source, context);
  const lane = {
    targetId: "lane-a",
    targetThreadId: "agent-a",
    speechAbortVersion: 0,
    spokenMessageKeys: new Set(),
  };
  context.queueSpeechForMessages(lane, messages);
  await new Promise((resolve) => setTimeout(resolve, 40));
}

(async () => {
  await runPage([
    {
      key: "latest",
      timestamp: "2026-06-17T04:00:10.000Z",
      threadId: "agent-a",
      ack_utterances: ["latest"],
    },
  ]);
  await runPage([
    {
      key: "newer",
      timestamp: "2026-06-17T04:00:11.000Z",
      threadId: "agent-a",
      ack_utterances: ["newer"],
    },
    {
      key: "older",
      timestamp: "2026-06-17T04:00:09.000Z",
      threadId: "agent-a",
      ack_utterances: ["older"],
    },
  ]);
  process.stdout.write(JSON.stringify(requests));
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  process.exit(1);
});
"""
    return _run_audio_script(script, "node persisted automatic speech cursor failed")


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
