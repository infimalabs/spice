"""Lane narration is gated to the lane's UI materialization instant.

Narration/speaking is a pure UI concern: a lane never auto-plays a message
older than the moment the browser materialized that lane (speechPrimeStartedAt),
regardless of server-side lane/team lifetimes. Old messages remain reachable via
manual play.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
AUDIO_JS = PROJECT_ROOT / "spice" / "serve" / "static" / "app.audio.js"


def test_automatic_speech_gates_messages_older_than_lane_materialization():
    # Narration never auto-plays anything older than the lane's UI
    # materialization instant; messages at or after it still play.
    assert set(_materialization_gated_speech_keys()) == {"at-materialization", "fresh"}


def test_message_materialization_gate_is_per_lane():
    decisions = _materialization_gate_decisions()
    assert decisions == {"early_lane": False, "late_lane": True}


def _materialization_gated_speech_keys() -> list[str]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const spoken = [];

class FakeAudio {
  constructor() { this.listeners = {}; }
  addEventListener(name, cb) { this.listeners[name] = cb; }
  removeEventListener(name) { delete this.listeners[name]; }
  play() {
    queueMicrotask(() => { if (this.listeners.ended) this.listeners.ended(); });
    return Promise.resolve();
  }
  pause() { if (this.listeners.pause) this.listeners.pause(); }
}

const context = {
  Blob: class {},
  Audio: FakeAudio,
  URL: { createObjectURL: () => "blob:audio", revokeObjectURL: () => {} },
  document: { querySelectorAll: () => [] },
  fetch: async (url, options) => {
    spoken.push(JSON.parse(options.body).text);
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
    speechPrimeStartedAt: Date.parse("2026-06-17T04:00:00.000Z"),
  };
  // Distinct agents per message so the per-agent automatic-speech cursor does
  // not cross-suppress; this isolates the materialization gate under test.
  context.queueSpeechForMessages(lane, [
    { key: "stale-history", timestamp: "2026-06-17T03:59:54.999Z",
      threadId: "agent-stale", ack_utterances: ["stale-history"] },
    { key: "startup-race", timestamp: "2026-06-17T03:59:59.999Z",
      threadId: "agent-race", ack_utterances: ["startup-race"] },
    { key: "at-materialization", timestamp: "2026-06-17T04:00:00.000Z",
      threadId: "agent-atmat", ack_utterances: ["at-materialization"] },
    { key: "fresh", timestamp: "2026-06-17T04:00:05.000Z",
      threadId: "agent-fresh", ack_utterances: ["fresh"] },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 40));
  process.stdout.write(JSON.stringify(spoken));
})().catch((error) => {
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
            "node materialization gate failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def _materialization_gate_decisions() -> dict[str, bool]:
    script = """
const fs = require("fs");
const vm = require("vm");
const path = process.argv[1];
const context = {};
vm.createContext(context);
vm.runInContext(fs.readFileSync(path, "utf8"), context);
const ts = Date.parse("2026-06-17T04:00:05.000Z");
process.stdout.write(JSON.stringify({
  early_lane: context.messageIsBeforeLaneMaterialization(
    { speechPrimeStartedAt: Date.parse("2026-06-17T04:00:00.000Z") }, ts),
  late_lane: context.messageIsBeforeLaneMaterialization(
    { speechPrimeStartedAt: Date.parse("2026-06-17T04:00:10.000Z") }, ts),
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
            "node materialization gate decisions failed:\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return json.loads(result.stdout)
