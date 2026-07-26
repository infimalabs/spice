"""Terminal proof that every transcript consumer projects one shared plane."""

from __future__ import annotations

import base64
import dataclasses
import json
from contextlib import contextmanager
from pathlib import Path
from typing import cast

from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER
from spice.serve.images import rollout_image_from_offset
from spice.serve.messages import RolloutCursor, read_assistant_messages
from spice.sessions.briefing import build_briefing_payload
from spice.tasks import effort
from spice.transcript import reader as transcript_reader
from spice.transcript.events import ContextUsage, Image, TranscriptEvent
from spice.transcript.reader import TranscriptEventReader, TranscriptReadStats
from tests.test_maillaunchparity import (
    assembled_ack_presentations,
    assembled_launch_narratives,
    replayed_launch_narratives,
    served_ack_presentations,
)
from tests.test_servemessagecrossing import (
    resumed_stream_envelopes,
    tail_window_envelopes,
)
from tests.test_transcriptparity import (
    CorpusCase,
    ParityOutput,
    assert_parity,
    split_pass_events,
    typed_events,
    whole_pass_events,
)
from tests.test_watchdogparity import replayed_judgments, supervised_judgments

THREAD = "019f9f00-1111-7222-8333-444455556666"
TURN = "019f9f00-aaaa-7bbb-8ccc-ddddeeeeffff"
TIMESTAMP_PREFIX = "2026-07-26T07:00:"
ACK_KEY = "1jN5Xq7C"
ACK_TEXT = f"ACK {ACK_KEY}: fresh two-driver convergence"
PNG_BYTES = b"\x89PNG\r\n\x1a\nconvergence"
PNG_DATA = base64.b64encode(PNG_BYTES).decode()
PNG_URL = f"data:image/png;base64,{PNG_DATA}"


def test_fresh_both_driver_fixtures_cross_every_shared_projection(
    tmp_path: Path, monkeypatch
) -> None:
    cases = _fresh_cases(tmp_path, monkeypatch)

    assert_parity(
        whole_pass_events,
        split_pass_events,
        corpus=cases,
        labels=("forward", "bounded"),
    )
    assert_parity(
        tail_window_envelopes,
        resumed_stream_envelopes,
        corpus=cases,
        labels=("reverse-window", "live-cursor"),
    )
    assert_parity(
        _whole_turns,
        _split_turns,
        corpus=cases,
        labels=("forensic whole", "forensic split"),
    )
    assert_parity(
        served_ack_presentations,
        assembled_ack_presentations,
        corpus=cases,
        labels=("ACK envelope", "ACK span"),
    )
    assert_parity(
        supervised_judgments,
        replayed_judgments,
        corpus=cases,
        labels=("watchdog stdout", "watchdog replay"),
    )
    assert_parity(
        replayed_launch_narratives,
        assembled_launch_narratives,
        corpus=cases,
        labels=("launch events", "launch spans"),
    )

    for case in cases:
        events = typed_events(case)
        effort_events = effort._read_transcript_events(case.path)
        assert effort_events == tuple(
            dataclasses.replace(
                event,
                at=dataclasses.replace(event.at, source_actor=None),
            )
            for event in events
        )
        assert any(isinstance(event, ContextUsage) for event in events)
        image = next(
            event
            for event in events
            if isinstance(event, Image)
            and event.role == "assistant"
            and event.payload_index is not None
        )
        assert image.at.offset is not None
        assert rollout_image_from_offset(
            case.path,
            offset=image.at.offset,
            item_index=image.payload_index,
            driver=case.driver,
        ) == (PNG_BYTES, "image/png")


def test_serve_and_briefing_physical_access_counts_are_observable_and_bounded(
    tmp_path: Path, monkeypatch
) -> None:
    case = _fresh_cases(tmp_path, monkeypatch)[0]
    source = case.path.read_bytes()
    source_lines = source.count(b"\n")
    cursor = RolloutCursor()
    opened: list[Path] = []
    parsed: list[str] = []
    reads: list[tuple[str, TranscriptReadStats]] = []
    open_binary = transcript_reader._open_binary
    parse_json = transcript_reader._parse_json_object
    read_events = TranscriptEventReader.read

    @contextmanager
    def track_open(path):
        opened.append(path)
        with open_binary(path) as handle:
            yield handle

    def track_parse(raw):
        parsed.append(raw)
        return parse_json(raw)

    def track_read(self, mode, **kwargs):
        result = read_events(self, mode, **kwargs)
        reads.append((mode, result.stats))
        return result

    monkeypatch.setattr(transcript_reader, "_open_binary", track_open)
    monkeypatch.setattr(transcript_reader, "_parse_json_object", track_parse)
    monkeypatch.setattr(TranscriptEventReader, "read", track_read)

    initial = read_assistant_messages(
        case.path,
        append_only=True,
        cursor=cursor,
        driver=case.driver,
    )
    first_page = (tuple(opened), tuple(parsed), tuple(reads))
    opened.clear()
    parsed.clear()
    reads.clear()
    delta = read_assistant_messages(
        case.path,
        append_only=True,
        cursor=cursor,
        driver=case.driver,
    )
    unchanged_page = (tuple(opened), tuple(parsed), tuple(reads))
    opened.clear()
    parsed.clear()
    reads.clear()
    payload = build_briefing_payload([case.path])
    briefing = (tuple(opened), tuple(parsed), tuple(reads))

    assert initial
    assert len(first_page[0]) == 1
    assert len(first_page[1]) == source_lines
    assert [mode for mode, _stats in first_page[2]] == ["reverse"]
    first_stats = first_page[2][0][1]
    assert (
        first_stats.file_opens,
        first_stats.bytes_read,
        first_stats.lines_parsed,
    ) == (1, len(source), source_lines)
    assert delta == []
    assert len(unchanged_page[0]) == 1
    assert unchanged_page[1:] == ((), ())
    assert payload.files == (case.path,)
    assert len(briefing[0]) == 2
    assert len(briefing[1]) == 2 * source_lines
    assert [mode for mode, _stats in briefing[2]] == ["reverse", "since"]
    assert sum(stats.file_opens for _mode, stats in briefing[2]) == 2
    assert sum(stats.bytes_read for _mode, stats in briefing[2]) == 2 * len(source)
    assert sum(stats.lines_parsed for _mode, stats in briefing[2]) == 2 * source_lines


def _whole_turns(case: CorpusCase) -> tuple[ParityOutput, ...]:
    return _turn_outputs(case, typed_events(case))


def _split_turns(case: CorpusCase) -> tuple[ParityOutput, ...]:
    events = cast(
        tuple[TranscriptEvent, ...],
        tuple(output.value for output in split_pass_events(case)),
    )
    return _turn_outputs(case, events)


def _turn_outputs(
    case: CorpusCase, events: tuple[TranscriptEvent, ...]
) -> tuple[ParityOutput, ...]:
    from spice.sessions.records import collect_turns_from_events

    return tuple(
        ParityOutput(
            value={
                **dataclasses.asdict(turn),
                "source_file": Path(turn.source_file).name,
            }
        )
        for turn in collect_turns_from_events(case.path, events)
    )


def _fresh_cases(tmp_path: Path, monkeypatch) -> tuple[CorpusCase, CorpusCase]:
    codex_home = tmp_path / "codex"
    claude_home = tmp_path / "claude"
    monkeypatch.setenv("CODEX_HOME", str(codex_home))
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude_home))
    codex_path = (
        codex_home
        / "sessions"
        / "2026"
        / "07"
        / f"rollout-2026-07-26T07-00-00-{THREAD}.jsonl"
    )
    claude_path = claude_home / "projects" / "-repo" / f"{THREAD}.jsonl"
    _write_records(codex_path, _codex_records())
    _write_records(claude_path, _claude_records())
    return (
        CorpusCase("fresh-codex", codex_path, CODEX_DRIVER),
        CorpusCase("fresh-claude", claude_path, CLAUDE_DRIVER),
    )


def _write_records(path: Path, records: tuple[dict, ...]) -> None:
    path.parent.mkdir(parents=True)
    path.write_text(
        "".join(f"{json.dumps(record, separators=(',', ':'))}\n" for record in records),
        encoding="utf-8",
    )


def _codex_records() -> tuple[dict, ...]:
    return (
        _codex_event("00.000Z", {"type": "task_started", "turn_id": TURN}),
        _codex_event(
            "01.000Z",
            {"type": "user_message", "turn_id": TURN, "message": "prove convergence"},
        ),
        _codex_response(
            "02.000Z",
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": ACK_TEXT}],
            },
        ),
        _codex_response(
            "03.000Z",
            {
                "type": "function_call",
                "name": "exec_command",
                "arguments": '{"cmd":"spice task status"}',
                "call_id": "call-fresh",
            },
        ),
        _codex_response(
            "04.000Z",
            {
                "type": "function_call_output",
                "call_id": "call-fresh",
                "output": "clean",
            },
        ),
        _codex_response(
            "05.000Z",
            {
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{"type": "input_image", "image_url": PNG_URL}],
            },
        ),
        _codex_event(
            "06.000Z",
            {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 10,
                        "cached_input_tokens": 2,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 3,
                        "reasoning_output_tokens": 1,
                        "total_tokens": 13,
                    },
                    "model_context_window": 258_400,
                },
            },
        ),
        _codex_event("07.000Z", {"type": "task_complete", "turn_id": TURN}),
        {"timestamp": _timestamp("08.000Z"), "type": "compacted", "payload": {}},
    )


def _claude_records() -> tuple[dict, ...]:
    return (
        {
            "timestamp": _timestamp("00.000Z"),
            "type": "user",
            "promptId": TURN,
            "cwd": "/repo",
            "message": {"content": "prove convergence"},
        },
        {
            "timestamp": _timestamp("01.000Z"),
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": ACK_TEXT},
                    {
                        "type": "tool_use",
                        "id": "call-fresh",
                        "name": "Bash",
                        "input": {"command": "spice task status"},
                    },
                ],
                "usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 2,
                    "cache_creation_input_tokens": 0,
                    "output_tokens": 3,
                },
            },
        },
        {
            "timestamp": _timestamp("02.000Z"),
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "call-fresh",
                        "content": "clean",
                    }
                ]
            },
        },
        {
            "timestamp": _timestamp("03.000Z"),
            "type": "assistant",
            "message": {
                "stop_reason": "end_turn",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": PNG_DATA,
                        },
                    }
                ],
            },
        },
        {"timestamp": _timestamp("04.000Z"), "type": "summary"},
    )


def _codex_response(seconds: str, payload: dict) -> dict:
    return {
        "timestamp": _timestamp(seconds),
        "type": "response_item",
        "payload": payload,
    }


def _codex_event(seconds: str, payload: dict) -> dict:
    return {
        "timestamp": _timestamp(seconds),
        "type": "event_msg",
        "payload": payload,
    }


def _timestamp(seconds: str) -> str:
    return f"{TIMESTAMP_PREFIX}{seconds}"
