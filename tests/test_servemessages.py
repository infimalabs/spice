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
from spice.serve import messages as message_reader
from spice.serve.messages import (
    RolloutCursor,
    assistant_messages_for_thread_id,
    read_assistant_messages,
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


def test_append_only_read_uses_cursor_delta_and_matches_full_window(
    tmp_path, monkeypatch
):
    transcript = tmp_path / "rollout.jsonl"
    _append_codex_message(transcript, TIMESTAMP, "first")
    cursor = RolloutCursor()
    initial = read_assistant_messages(
        transcript, limit=5, cursor=cursor, driver=CODEX_DRIVER
    )
    old_offset = cursor.offset
    second = "2026-06-20T04:46:00.000000Z"
    _append_codex_message(transcript, second, "second")
    expected = read_assistant_messages(transcript, limit=5, driver=CODEX_DRIVER)

    def fail_window(*_args, **_kwargs):
        raise AssertionError("append-only growth must not rescan the full window")

    monkeypatch.setattr(message_reader, "_read_window", fail_window)

    delta = read_assistant_messages(
        transcript,
        limit=5,
        append_only=True,
        cursor=cursor,
        driver=CODEX_DRIVER,
    )

    assert [item.display_text for item in initial] == ["first"]
    assert [item.display_text for item in delta] == ["second"]
    assert [item.key for item in cursor.window or []] == [item.key for item in expected]
    assert cursor.offset > old_offset


def test_append_only_read_reports_cross_boundary_image_pair_removal(
    tmp_path, monkeypatch
):
    transcript = tmp_path / "rollout.jsonl"
    _append_codex_payload(
        transcript,
        TIMESTAMP,
        {
            "type": "function_call",
            "name": "view_image",
            "arguments": json.dumps({"path": "shot.png"}),
        },
    )
    cursor = RolloutCursor()
    initial = read_assistant_messages(
        transcript, limit=5, cursor=cursor, driver=CODEX_DRIVER, worktree_id="wt"
    )
    _append_codex_payload(
        transcript,
        "2026-06-20T04:46:00.000000Z",
        {
            "type": "function_call_output",
            "output": [
                {
                    "type": "input_image",
                    "image_url": {"url": "data:image/png;base64,aW1n"},
                }
            ],
        },
    )
    expected = read_assistant_messages(
        transcript, limit=5, driver=CODEX_DRIVER, worktree_id="wt"
    )

    def fail_window(*_args, **_kwargs):
        raise AssertionError("append-only image-pair growth must not rescan")

    monkeypatch.setattr(message_reader, "_read_window", fail_window)

    delta = read_assistant_messages(
        transcript,
        limit=5,
        append_only=True,
        cursor=cursor,
        driver=CODEX_DRIVER,
        worktree_id="wt",
    )

    assert [item.source_kind for item in initial] == ["view_image_call"]
    assert [item.source_kind for item in delta] == ["tool_output_image"]
    assert cursor.removed_keys == [initial[0].key]
    assert [item.key for item in cursor.window or []] == [item.key for item in expected]


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


def _append_codex_message(path: Path, timestamp: str, text: str) -> None:
    _append_codex_payload(
        path,
        timestamp,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )


def _append_codex_payload(
    path: Path, timestamp: str, payload: dict[str, object]
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(
            json.dumps(
                {"timestamp": timestamp, "type": "response_item", "payload": payload},
                separators=(",", ":"),
            )
            + "\n"
        )


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
