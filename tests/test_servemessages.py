"""Serve transcript resolution contracts."""

from __future__ import annotations

import gzip
import json
import subprocess
from collections import Counter
from pathlib import Path

from spice.agent.driver import (
    CLAUDE_DRIVER,
    CODEX_DRIVER,
    SPICE_AGENT_DRIVER_ENV,
    dashed_uuid,
)
from spice.config.edit import set_scope_section
from spice.config.layers import WORKTREE_SOURCE
from spice.serve import messages as message_reader
from spice.serve.messages import (
    RolloutCursor,
    assistant_messages_for_thread_id,
    read_assistant_messages,
    resolve_thread_transcript,
)
from spice.transcript import reader as transcript_reader

THREAD = "11111111222233334444555555555555"
TIMESTAMP = "2026-06-20T04:45:00.000000Z"
# Enough append reads over the unfinished record to catch a cursor that walks
# forward once per read rather than staying on the last complete boundary.
QUIET_APPEND_READS = 3


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
    set_scope_section(repo, WORKTREE_SOURCE, "agent", {"driver": "codex"})
    _write_claude_transcript(tmp_path, monkeypatch, "hello claude")

    read = assistant_messages_for_thread_id(THREAD, repo_root=repo)

    assert read.error is None
    assert read.transcript is not None
    assert read.transcript.owner_driver is CLAUDE_DRIVER
    assert [item.text for item in read.items] == ["hello claude"]


def test_resolve_thread_transcript_returns_claude_owner(tmp_path, monkeypatch):
    repo = _repo(tmp_path / "repo")
    set_scope_section(repo, WORKTREE_SOURCE, "agent", {"driver": "claude"})
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


def test_append_only_read_delivers_a_record_the_writer_finished_after_the_seed(
    tmp_path,
) -> None:
    """A record split across two flushes is delivered once, not skipped.

    Serve seeds its live cursor from the same reverse read that draws the
    history window, and a live writer can be caught mid-record. Seeding at
    end-of-file would put the resume offset inside that half-written line, so
    the append pass would decode only its suffix and the completed record would
    never reach the lane.
    """
    transcript = tmp_path / "rollout.jsonl"
    _append_codex_message(transcript, TIMESTAMP, "before")
    line = _codex_message_line("2026-06-20T04:46:00.000000Z", "after")
    flushed = line.index('"payload"')
    _append_text(transcript, line[:flushed])
    cursor = RolloutCursor()

    initial = read_assistant_messages(
        transcript, limit=5, cursor=cursor, driver=CODEX_DRIVER
    )
    _append_text(transcript, line[flushed:])
    delta = read_assistant_messages(
        transcript,
        limit=5,
        append_only=True,
        cursor=cursor,
        driver=CODEX_DRIVER,
    )

    assert [item.display_text for item in initial] == ["before"]
    assert [item.display_text for item in delta] == ["after"]
    assert [
        item.display_text
        for item in read_assistant_messages(transcript, limit=5, driver=CODEX_DRIVER)
    ] == ["after", "before"]


def test_append_only_reads_over_a_partial_tail_still_deliver_the_record(
    tmp_path,
) -> None:
    """Reads taken while the writer is still mid-record do not skip past it.

    A lane reads far more often than a writer flushes, so the half-written
    record is normally observed several times before it is complete. Each of
    those reads sees a file that has not grown since the window was drawn, and
    treating that as 'caught up to the end' would move the cursor over the
    prefix the writer is still finishing.
    """
    transcript = tmp_path / "rollout.jsonl"
    _append_codex_message(transcript, TIMESTAMP, "before")
    line = _codex_message_line("2026-06-20T04:46:00.000000Z", "after")
    flushed = line.index('"payload"')
    _append_text(transcript, line[:flushed])
    cursor = RolloutCursor()
    read_assistant_messages(transcript, limit=5, cursor=cursor, driver=CODEX_DRIVER)

    quiet = [
        read_assistant_messages(
            transcript,
            limit=5,
            append_only=True,
            cursor=cursor,
            driver=CODEX_DRIVER,
        )
        for _ in range(QUIET_APPEND_READS)
    ]
    _append_text(transcript, line[flushed:])
    delta = read_assistant_messages(
        transcript,
        limit=5,
        append_only=True,
        cursor=cursor,
        driver=CODEX_DRIVER,
    )

    assert quiet == [[] for _ in range(QUIET_APPEND_READS)]
    assert [item.display_text for item in delta] == ["after"]


def test_append_only_cursor_restarts_on_a_larger_rotated_transcript(tmp_path) -> None:
    transcript = tmp_path / "rollout.jsonl"
    _append_codex_message(transcript, TIMESTAMP, "old")
    cursor = RolloutCursor()
    initial = read_assistant_messages(
        transcript,
        limit=5,
        cursor=cursor,
        driver=CODEX_DRIVER,
    )
    old_offset = cursor.offset
    old_identity = cursor.file_identity

    transcript.rename(tmp_path / "rotated-rollout.jsonl")
    replacement = ["new-0-long", "new-1-long", "new-2-long"]
    for index, text in enumerate(replacement, start=1):
        _append_codex_message(
            transcript,
            f"2026-06-20T04:46:0{index}.000000Z",
            text,
        )
    replacement_size = transcript_reader.transcript_size(transcript)
    assert replacement_size is not None
    assert replacement_size > old_offset

    resumed = read_assistant_messages(
        transcript,
        limit=5,
        append_only=True,
        cursor=cursor,
        driver=CODEX_DRIVER,
    )

    assert [item.display_text for item in initial] == ["old"]
    assert [item.display_text for item in resumed] == list(reversed(replacement))
    assert [item.display_text for item in cursor.window or []] == list(
        reversed(replacement)
    )
    assert cursor.file_identity != old_identity
    assert cursor.offset == replacement_size


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
                    "image_url": "data:image/png;base64,aW1n",
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


def test_append_only_read_with_after_reports_cross_boundary_image_pair_removal(
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
    after = cursor.last_key
    _append_codex_payload(
        transcript,
        "2026-06-20T04:46:00.000000Z",
        {
            "type": "function_call_output",
            "output": [
                {
                    "type": "input_image",
                    "image_url": "data:image/png;base64,aW1n",
                }
            ],
        },
    )
    expected = read_assistant_messages(
        transcript, limit=5, driver=CODEX_DRIVER, worktree_id="wt"
    )

    def fail_window(*_args, **_kwargs):
        raise AssertionError("append-only after-cursor growth must not rescan")

    monkeypatch.setattr(message_reader, "_read_window", fail_window)

    delta = read_assistant_messages(
        transcript,
        limit=5,
        after=after,
        append_only=True,
        cursor=cursor,
        driver=CODEX_DRIVER,
        worktree_id="wt",
    )

    assert [item.source_kind for item in initial] == ["view_image_call"]
    assert [item.source_kind for item in delta] == ["tool_output_image"]
    assert cursor.removed_keys == [initial[0].key]
    assert [item.key for item in cursor.window or []] == [item.key for item in expected]


def test_gzip_reader_preserves_timestamp_offset_cursors_and_paging(tmp_path) -> None:
    transcript = tmp_path / "rollout.jsonl.gz"
    first = {
        "timestamp": TIMESTAMP,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "first ☃"}],
        },
    }
    second_timestamp = "2026-06-20T04:46:00.000000Z"
    second = {
        "timestamp": second_timestamp,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "second"}],
        },
    }
    first_raw = (
        json.dumps(first, separators=(",", ":"), ensure_ascii=False) + "\n"
    ).encode()
    malformed = b"{malformed\n"
    second_offset = len(first_raw) + len(malformed)
    with gzip.open(transcript, "wb") as handle:
        handle.write(first_raw)
        handle.write(malformed)
        handle.write((json.dumps(second, separators=(",", ":")) + "\n").encode())

    items = read_assistant_messages(transcript, limit=5, driver=CODEX_DRIVER)

    assert [item.display_text for item in items] == ["second", "first ☃"]
    assert [item.key for item in items] == [
        f"{second_timestamp}#{second_offset}",
        f"{TIMESTAMP}#0",
    ]
    assert [
        item.display_text
        for item in read_assistant_messages(
            transcript,
            limit=5,
            before=items[0].key,
            driver=CODEX_DRIVER,
        )
    ] == ["first ☃"]
    assert [
        item.display_text
        for item in read_assistant_messages(
            transcript,
            limit=5,
            after=items[1].key,
            driver=CODEX_DRIVER,
        )
    ] == ["second"]


def test_sparse_reverse_chunks_project_each_accessed_record_once(
    tmp_path, monkeypatch
) -> None:
    transcript = tmp_path / "rollout.jsonl"
    _append_codex_message(transcript, TIMESTAMP, "first")
    _append_codex_payload(
        transcript,
        "2026-06-20T04:45:00.250000Z",
        {
            "type": "function_call",
            "name": "exec_command",
            "arguments": json.dumps({"cmd": "echo hi"}),
            "call_id": "cross-chunk-call",
        },
    )
    _append_codex_payload(
        transcript,
        "2026-06-20T04:45:00.500000Z",
        {"type": "ignored-long-line", "blob": "x" * 900},
    )
    for index in range(12):
        _append_codex_payload(
            transcript,
            f"2026-06-20T04:45:{index + 1:02d}.000000Z",
            {"type": f"ignored-{index}", "value": index},
        )
    _append_codex_payload(
        transcript,
        "2026-06-20T04:45:59.000000Z",
        {
            "type": "function_call_output",
            "output": "done",
            "call_id": "cross-chunk-call",
        },
    )
    _append_codex_message(
        transcript,
        "2026-06-20T04:46:00.000000Z",
        "last",
    )
    expected = read_assistant_messages(transcript, limit=2, driver=CODEX_DRIVER)
    original_parse = transcript_reader._parse_json_object
    original_projection = message_reader._build_message
    parses: Counter[str] = Counter()
    projections: Counter[int] = Counter()

    def count_parse(raw: str):
        parses[raw] += 1
        return original_parse(raw)

    def count_projection(*args, **kwargs):
        projections[args[0].at.offset] += 1
        return original_projection(*args, **kwargs)

    monkeypatch.setattr(message_reader, "REVERSE_WINDOW_BYTES", 256)
    monkeypatch.setattr(transcript_reader, "_parse_json_object", count_parse)
    monkeypatch.setattr(message_reader, "_build_message", count_projection)

    items = read_assistant_messages(transcript, limit=2, driver=CODEX_DRIVER)

    assert [item.to_payload() for item in items] == [
        item.to_payload() for item in expected
    ]
    assert [
        item.display_text for item in items if not item.kind.startswith("presence:")
    ] == ["last", "first"]
    assert [item.preview for item in items if item.kind.startswith("presence:")] == [
        "exec command: echo hi -> done"
    ]
    assert parses
    assert set(parses.values()) == {1}
    assert projections
    assert set(projections.values()) == {1}


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
    _append_text(path, _codex_message_line(timestamp, text))


def _append_codex_payload(
    path: Path, timestamp: str, payload: dict[str, object]
) -> None:
    _append_text(path, _codex_payload_line(timestamp, payload))


def _codex_message_line(timestamp: str, text: str) -> str:
    return _codex_payload_line(
        timestamp,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )


def _codex_payload_line(timestamp: str, payload: dict[str, object]) -> str:
    return (
        json.dumps(
            {"timestamp": timestamp, "type": "response_item", "payload": payload},
            separators=(",", ":"),
        )
        + "\n"
    )


def _append_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(text)


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
