"""Compaction-bounded session slice views."""

import argparse
import json

from spice.cli.parser import build_parser
from spice.sessions import records
from spice.sessions.cli import _print_slices
from spice.sessions.slices import build_compaction_slices

PARSER_MAX_TEXT = 40


def test_compaction_slice_ids_are_derived_across_boundaries(tmp_path):
    transcript = _slice_fixture(tmp_path)

    rows = build_compaction_slices(
        records.collect_turns([transcript]), records.collect_compactions([transcript])
    )

    assert [row.slice_id for row in rows] == ["compaction-1", "compaction-2"]
    assert rows[0].start_ts == "2026-01-01T00:00:00.000Z"
    assert rows[0].end_ts == "2026-01-01T00:00:04.000Z"
    assert rows[0].turn_ids == ["turn-a"]
    assert rows[1].start_ts == "2026-01-01T00:00:04.000Z"
    assert rows[1].end_ts == "2026-01-01T00:00:09.000Z"
    assert rows[1].turn_ids == ["turn-b"]
    assert rows[1].patch_count == 1
    assert rows[1].crossing_turn_files == ["cli.py"]


def test_session_slices_summary_honors_limit_and_slice_id(tmp_path, capsys):
    transcript = _slice_fixture(tmp_path)

    _print_slices(_slice_args(limit=1), [transcript])

    limited = capsys.readouterr().out
    assert limited.startswith(
        "slice=compaction-2 2026-01-01T00:00:04.000Z -> 2026-01-01T00:00:09.000Z"
    )
    assert "slice=compaction-1" not in limited

    _print_slices(_slice_args(slice_id=["compaction-1"]), [transcript])

    selected = capsys.readouterr().out
    assert "slice=compaction-1" in selected
    assert "slice=compaction-2" not in selected


def test_session_slices_full_view_prints_boundary_messages(tmp_path, capsys):
    transcript = _slice_fixture(tmp_path)

    _print_slices(_slice_args(slice_id=["compaction-2"], view="full"), [transcript])

    output = capsys.readouterr().out
    assert "  messages=\n" in output
    assert "    assistant_before: answer b" in output
    assert "    user_after: ask c" in output
    assert "answer a" not in output


def test_session_slices_parser_exposes_current_flag_surface(tmp_path):
    parser = build_parser()

    args = parser.parse_args(
        [
            "session",
            "slices",
            str(tmp_path / "session.jsonl"),
            "--limit",
            "3",
            "--slice-id",
            "compaction-1",
            "--slice-id",
            "compaction-2",
            "--view",
            "full",
            "--max-text",
            str(PARSER_MAX_TEXT),
        ]
    )

    assert args.session_action == "slices"
    assert args.limit == 3
    assert args.slice_id == ["compaction-1", "compaction-2"]
    assert args.view == "full"
    assert args.max_text == PARSER_MAX_TEXT


def _slice_fixture(tmp_path):
    transcript = tmp_path / "session.jsonl"
    _write_jsonl(
        transcript,
        [
            _event(
                "2026-01-01T00:00:00Z",
                "event_msg",
                {"type": "task_started", "turn_id": "turn-a"},
            ),
            _message("2026-01-01T00:00:01Z", "user", "ask a"),
            _message("2026-01-01T00:00:02Z", "assistant", "answer a"),
            _event("2026-01-01T00:00:03Z", "event_msg", {"type": "task_complete"}),
            _event("2026-01-01T00:00:04Z", "compacted", {}),
            _event(
                "2026-01-01T00:00:05Z",
                "event_msg",
                {"type": "task_started", "turn_id": "turn-b"},
            ),
            _message("2026-01-01T00:00:06Z", "user", "ask b"),
            _patch_call("2026-01-01T00:00:07Z", "spice/sessions/cli.py"),
            _message("2026-01-01T00:00:08Z", "assistant", "answer b"),
            _event("2026-01-01T00:00:08.500Z", "event_msg", {"type": "task_complete"}),
            _event("2026-01-01T00:00:09Z", "compacted", {}),
            _event(
                "2026-01-01T00:00:10Z",
                "event_msg",
                {"type": "task_started", "turn_id": "turn-c"},
            ),
            _message("2026-01-01T00:00:11Z", "user", "ask c"),
            _message("2026-01-01T00:00:12Z", "assistant", "answer c"),
        ],
    )
    return transcript


def _slice_args(**overrides):
    values = {"limit": 25, "slice_id": [], "view": "summary", "max_text": 180}
    values.update(overrides)
    return argparse.Namespace(**values)


def _event(timestamp: str, record_type: str, payload: dict):
    return {"timestamp": timestamp, "type": record_type, "payload": payload}


def _message(timestamp: str, role: str, text: str):
    return _event(
        timestamp,
        "response_item",
        {
            "type": "message",
            "role": role,
            "content": [{"type": "output_text", "text": text}],
        },
    )


def _patch_call(timestamp: str, path: str):
    patch = f"*** Begin Patch\n*** Update File: {path}\n@@\n unchanged\n*** End Patch\n"
    return _event(
        timestamp,
        "response_item",
        {
            "type": "function_call",
            "name": "apply_patch",
            "arguments": json.dumps({"input": patch}),
        },
    )


def _write_jsonl(path, events):
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )
