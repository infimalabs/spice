"""Session phase and message analysis surfaces."""

from __future__ import annotations

import json

from spice.cli.parser import build_parser

EXIT_OK = 0
PHASE_EXAMPLES = 1
MAX_TEXT = 80


def test_session_phases_segments_contiguous_working_families(tmp_path, capsys):
    transcript = tmp_path / "analysis.jsonl"
    _write_analysis_transcript(transcript)
    args = build_parser().parse_args(
        [
            "session",
            "phases",
            str(transcript),
            "--examples",
            str(PHASE_EXAMPLES),
            "--max-text",
            str(MAX_TEXT),
        ]
    )

    result = args.func(args)

    output = capsys.readouterr().out
    assert result == EXIT_OK
    assert "phase=1 family=execution primary=execution turns=2" in output
    assert "phase=2 family=implementation primary=implementation turns=3" in output
    assert "turn=turn-exec-1 archetype=execution" in output
    assert "turn=turn-patch-1 archetype=implementation" in output


def test_session_messages_filter_by_side_phase_kind_and_flavor(tmp_path, capsys):
    transcript = tmp_path / "analysis.jsonl"
    _write_analysis_transcript(transcript)
    user_args = build_parser().parse_args(
        [
            "session",
            "messages",
            str(transcript),
            "--side",
            "user",
            "--flavor",
            "constraint_like",
            "--oldest-first",
            "--max-text",
            str(MAX_TEXT),
        ]
    )
    assistant_args = build_parser().parse_args(
        [
            "session",
            "messages",
            str(transcript),
            "--side",
            "assistant",
            "--phase-kind",
            "final_answer",
            "--flavor",
            "final_answer",
            "--oldest-first",
            "--max-text",
            str(MAX_TEXT),
        ]
    )

    user_result = user_args.func(user_args)
    user_output = capsys.readouterr().out
    assistant_result = assistant_args.func(assistant_args)
    assistant_output = capsys.readouterr().out

    assert user_result == EXIT_OK
    assert assistant_result == EXIT_OK
    assert "side=user phase=prompt flavor=constraint_like" in user_output
    assert "cues=must,only" in user_output
    assert "must only patch code 1" in user_output
    assert "side=assistant phase=final_answer flavor=final_answer" in assistant_output
    assert "final answer 1" in assistant_output


def test_session_analysis_parser_exposes_phases_and_messages_flags(tmp_path):
    transcript = tmp_path / "analysis.jsonl"
    phases = build_parser().parse_args(
        [
            "session",
            "phases",
            str(transcript),
            "--contains",
            "patch",
            "--tool",
            "apply_patch",
            "--limit",
            "2",
            "--examples",
            str(PHASE_EXAMPLES),
        ]
    )
    messages = build_parser().parse_args(
        [
            "session",
            "messages",
            str(transcript),
            "--side",
            "assistant",
            "--phase-kind",
            "commentary",
            "--flavor",
            "question_like",
            "--oldest-first",
        ]
    )

    assert phases.session_action == "phases"
    assert phases.contains == "patch"
    assert phases.tools == ["apply_patch"]
    assert phases.limit == 2
    assert phases.examples == PHASE_EXAMPLES
    assert messages.session_action == "messages"
    assert messages.side == ["assistant"]
    assert messages.phase_kinds == ["commentary"]
    assert messages.flavors == ["question_like"]
    assert messages.oldest_first is True


def _write_analysis_transcript(path) -> None:
    events = [
        *_turn_events(
            "turn-exec-1",
            start_second=0,
            user_text="please run command 1",
            tool_name="exec_command",
            final_text="command final 1",
        ),
        *_turn_events(
            "turn-exec-2",
            start_second=10,
            user_text="please run command 2",
            tool_name="exec_command",
            final_text="command final 2",
        ),
        *_turn_events(
            "turn-patch-1",
            start_second=20,
            user_text="must only patch code 1",
            tool_name="apply_patch",
            final_text="final answer 1",
        ),
        *_turn_events(
            "turn-patch-2",
            start_second=30,
            user_text="must only patch code 2",
            tool_name="apply_patch",
            final_text="final answer 2",
        ),
        *_turn_events(
            "turn-patch-3",
            start_second=40,
            user_text="must only patch code 3",
            tool_name="apply_patch",
            final_text="final answer 3",
        ),
    ]
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )


def _turn_events(
    turn_id: str,
    *,
    start_second: int,
    user_text: str,
    tool_name: str,
    final_text: str,
) -> list[dict[str, object]]:
    return [
        {
            "timestamp": _ts(start_second),
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": turn_id},
        },
        {
            "timestamp": _ts(start_second + 1),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": user_text}],
            },
        },
        {
            "timestamp": _ts(start_second + 2),
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": tool_name,
                "arguments": _tool_arguments(tool_name),
            },
        },
        {
            "timestamp": _ts(start_second + 3),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{"text": final_text}],
            },
        },
        {
            "timestamp": _ts(start_second + 4),
            "type": "event_msg",
            "payload": {"type": "task_complete", "last_agent_message": final_text},
        },
    ]


def _tool_arguments(tool_name: str) -> str:
    if tool_name != "apply_patch":
        return "{}"
    return json.dumps(
        {
            "input": (
                "*** Begin Patch\n"
                "*** Update File: spice/sessions/analysis.py\n"
                "@@\n"
                "+changed\n"
                "*** End Patch\n"
            )
        }
    )


def _ts(second: int) -> str:
    return f"2026-01-01T00:00:{second:02d}Z"
