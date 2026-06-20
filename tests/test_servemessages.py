"""Serve transcript resolution contracts."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from spice.agent.driver import (
    CLAUDE_DRIVER,
    CODEX_DRIVER,
    SPICE_AGENT_DRIVER_ENV,
    dashed_uuid,
)
from spice.config import update_section
from spice.serve.messages import (
    assistant_messages_for_thread_id,
    resolve_thread_transcript,
)

THREAD = "11111111222233334444555555555555"
TIMESTAMP = "2026-06-20T04:45:00.000000Z"


def test_resolve_thread_transcript_returns_codex_owner(tmp_path, monkeypatch):
    repo = _repo(tmp_path / "repo")
    transcript = _write_codex_transcript(tmp_path, monkeypatch, "hello codex")

    resolved = resolve_thread_transcript(THREAD, repo)

    assert resolved is not None
    assert resolved.thread_id == THREAD
    assert resolved.path == transcript.resolve()
    assert resolved.owner_driver is CODEX_DRIVER


def test_assistant_messages_use_claude_owner_when_configured_driver_misses(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path / "repo")
    update_section(repo, "agent", {"driver": "codex"})
    _write_claude_transcript(tmp_path, monkeypatch, "hello claude")

    read = assistant_messages_for_thread_id(THREAD, repo_root=repo)

    assert read.error is None
    assert read.transcript is not None
    assert read.transcript.owner_driver is CLAUDE_DRIVER
    assert [item.text for item in read.items] == ["hello claude"]


def test_resolve_thread_transcript_returns_claude_owner(tmp_path, monkeypatch):
    repo = _repo(tmp_path / "repo")
    update_section(repo, "agent", {"driver": "claude"})
    transcript = _write_claude_transcript(tmp_path, monkeypatch, "native claude")

    resolved = resolve_thread_transcript(THREAD, repo)

    assert resolved is not None
    assert resolved.thread_id == THREAD
    assert resolved.path == transcript.resolve()
    assert resolved.owner_driver is CLAUDE_DRIVER


def test_assistant_messages_report_missing_transcript(tmp_path, monkeypatch):
    repo = _repo(tmp_path / "repo")
    _isolate_driver_homes(tmp_path, monkeypatch)

    read = assistant_messages_for_thread_id(THREAD, repo_root=repo)

    assert read.items == []
    assert read.transcript is None
    assert read.error == f"Could not resolve transcript for {THREAD}"


def _repo(path: Path) -> Path:
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    return path


def _isolate_driver_homes(tmp_path, monkeypatch) -> None:
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setenv("CODEX_HOME", str(tmp_path / "codex"))
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "claude"))


def _write_codex_transcript(tmp_path, monkeypatch, text: str) -> Path:
    _isolate_driver_homes(tmp_path, monkeypatch)
    transcript = tmp_path / "codex" / "sessions" / f"rollout-{THREAD}.jsonl"
    transcript.parent.mkdir(parents=True)
    transcript.write_text(
        json.dumps(
            {
                "timestamp": TIMESTAMP,
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                },
            },
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    return transcript


def _write_claude_transcript(tmp_path, monkeypatch, text: str) -> Path:
    _isolate_driver_homes(tmp_path, monkeypatch)
    transcript = (
        tmp_path / "claude" / "projects" / "-tmp-spice" / f"{dashed_uuid(THREAD)}.jsonl"
    )
    transcript.parent.mkdir(parents=True)
    transcript.write_text(
        json.dumps(
            {
                "type": "assistant",
                "timestamp": TIMESTAMP,
                "message": {
                    "role": "assistant",
                    "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": text}],
                },
            },
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    return transcript
