"""Session forensics: context metering and identity primitives."""

import argparse
import gzip
import json
import os
import shutil
import sqlite3
import subprocess
import time
from types import SimpleNamespace

import pytest

from spice.agent.driver import DRIVER
from spice.cli.parser import build_parser
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    deadletter_inbox_item,
    write_inbox_item,
)
from spice.sessions import briefing as briefing_module
from spice.sessions.briefing import render_briefing, render_sweep
from spice.sessions.cli import handle_session, render_thread_summary
from spice.sessions.meter import (
    ActiveContextSnapshot,
    active_context_percent,
    collect_context_meter,
    context_meter_instruction,
    context_pressure_level,
    context_pressure_should_warn,
)
from spice.sessions import learnings, records
from spice.sessions.util import first_text, normalize_timestamp
from spice.errors import SpiceError
from spice.tasks import alloc as task_alloc
from spice.tasks import config as task_config
from spice.tasks import create, identity as task_identity, ops
from spice.tasks import tw as task_tw
from spice.tasks.identity import (
    BASE,
    INCEPTED_RE,
    STAMP_WIDTH,
    decode,
    encode,
    encode_width,
    epoch_millis,
    key_for,
    mint_incepted,
    render_handle,
)

CODEX_HOME_ENV = "CODEX_HOME"  # env-policy: allow
THREAD_DASHED = "11111111-2222-3333-4444-555555555555"
THREAD_CANONICAL = "11111111222233334444555555555555"
BRIEFING_FILTER_MAX_LINES = 80
BRIEFING_FILTER_MAX_BYTES = 10_000
BRIEFING_PRUNE_MAX_LINES = 6
BRIEFING_PARSE_MAX_LINES = 10
BRIEFING_PARSE_MAX_BYTES = 1_000
GZIP_SESSION_TOTAL_TOKENS = 115
ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


def test_pressure_levels_at_documented_thresholds():
    assert context_pressure_level(74.9) == "green"
    assert context_pressure_level(75.0) == "yellow"
    assert context_pressure_level(85.0) == "orange"
    assert context_pressure_level(90.0) == "red"
    assert context_pressure_level(None) == "unknown"


def test_pressure_warns_from_yellow_up():
    assert context_pressure_should_warn("yellow") is True
    assert context_pressure_should_warn("orange") is True
    assert context_pressure_should_warn("red") is True
    assert context_pressure_should_warn("green") is False


QUARTER_PERCENT = 25.0


def test_active_context_percent_uses_window():
    snapshot = ActiveContextSnapshot(
        source_file="rollout.jsonl",
        ts="2026-01-01T00:00:00.000Z",
        input_tokens=40_000,
        cached_input_tokens=0,
        output_tokens=10_000,
        reasoning_output_tokens=0,
        total_tokens=50_000,
        model_context_window=200_000,
        cumulative_total_tokens=50_000,
    )
    assert active_context_percent(snapshot) == QUARTER_PERCENT


def test_normalize_timestamp_zulu_milliseconds():
    assert (
        normalize_timestamp("2026-01-01T00:00:00+00:00") == "2026-01-01T00:00:00.000Z"
    )


def test_first_text_reads_content_list():
    content = [{"type": "output_text", "text": "hello"}]
    assert first_text(content) == "hello"


def test_session_timeline_prints_turn_and_compaction(tmp_path, capsys):
    transcript = tmp_path / "session.jsonl"
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-a"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "build timeline"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"text": "ready to compact"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "last_agent_message": "timeline built",
            },
        },
        {"timestamp": "2026-01-01T00:00:04Z", "type": "compacted", "payload": {}},
        {
            "timestamp": "2026-01-01T00:00:05Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "after compaction"}],
            },
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )

    handle_session(_timeline_args(transcript, limit=10, max_text=80))

    output = capsys.readouterr().out
    assert "turn=turn-a" in output
    assert "user=build timeline" in output
    assert "compaction assistant_before=ready to compact" in output
    assert "user_after=after compaction" in output


def test_session_briefing_reads_direct_gzip_jsonl_path(tmp_path, capsys):
    transcript = tmp_path / "session.jsonl.gz"
    _write_gzip_jsonl(
        transcript,
        [
            {
                "timestamp": "2026-01-01T00:00:00Z",
                "type": "event_msg",
                "payload": {"type": "task_started", "turn_id": "turn-gzip"},
            },
            {
                "timestamp": "2026-01-01T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"text": "compressed request"}],
                },
            },
            {
                "timestamp": "2026-01-01T00:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "last_token_usage": {
                            "input_tokens": 100,
                            "cached_input_tokens": 25,
                            "output_tokens": 10,
                            "reasoning_output_tokens": 5,
                            "total_tokens": GZIP_SESSION_TOTAL_TOKENS,
                        },
                        "total_token_usage": {
                            "total_tokens": GZIP_SESSION_TOTAL_TOKENS
                        },
                        "model_context_window": 200000,
                    },
                },
            },
            {
                "timestamp": "2026-01-01T00:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "task_complete",
                    "last_agent_message": "compressed done",
                },
            },
        ],
    )

    args = build_parser().parse_args(
        [
            "session",
            "briefing",
            str(transcript),
            "--max-lines",
            "80",
            "--max-bytes",
            "10000",
        ]
    )
    handle_session(args)
    meter = collect_context_meter([transcript])

    output = capsys.readouterr().out
    assert "files=session.jsonl.gz turns=1" in output
    assert "Latest Ask\n  compressed request" in output
    assert "Latest Final\n  compressed done" in output
    assert meter.snapshot_count == 1
    assert meter.latest_snapshot is not None
    assert meter.latest_snapshot.total_tokens == GZIP_SESSION_TOTAL_TOKENS


def test_session_timeline_contains_keeps_turn_when_match_is_not_latest(
    tmp_path, capsys
):
    transcript = tmp_path / "session.jsonl"
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-a"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "needle setup"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "later request"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )

    handle_session(_timeline_args(transcript, contains="needle", limit=10, max_text=80))

    output = capsys.readouterr().out
    assert "turn=turn-a" in output
    assert "user=later request" in output


def _timeline_args(transcript, **overrides):
    values = {
        "session_action": "timeline",
        "start": None,
        "end": None,
        "contains": None,
        "turn_ids": None,
        "tools": None,
        "limit": 10,
        "max_text": 80,
    }
    values.update(overrides)
    values["files"] = [str(transcript)]
    return argparse.Namespace(**values)


def test_session_thread_resolves_state_db_and_summarizes_activity(
    tmp_path, monkeypatch
):
    codex_home = tmp_path / "codex"
    transcript = tmp_path / "rollout.jsonl"
    _write_thread_transcript(transcript)
    _write_state_db(codex_home, THREAD_DASHED, transcript)
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))

    summary = render_thread_summary(THREAD_CANONICAL)

    assert "Thread" in summary
    assert f"id={THREAD_CANONICAL}" in summary
    assert "driver=codex" in summary
    assert f"transcript={transcript.resolve()}" in summary
    assert "turns=1 compactions=0" in summary
    assert "latest_user=investigate thread" in summary
    assert "latest_assistant=thread done" in summary
    assert "latest_final=thread done" in summary
    assert "commands=1 patches=0 errors=0 web_searches=0" in summary


def test_session_thread_falls_back_to_session_index(tmp_path, monkeypatch):
    codex_home = tmp_path / "codex"
    transcript = (
        codex_home / "sessions" / "2026" / "06" / f"rollout-{THREAD_DASHED}.jsonl"
    )
    _write_thread_transcript(transcript)
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))

    summary = render_thread_summary(THREAD_DASHED)

    assert f"id={THREAD_CANONICAL}" in summary
    assert f"transcript={transcript.resolve()}" in summary
    assert "latest_user=investigate thread" in summary


def test_session_thread_resolves_claude_transcript_by_driver_owner(
    tmp_path, monkeypatch
):
    claude_home = tmp_path / "claude"
    transcript = (
        claude_home / "projects" / "-private-tmp-spice-sup" / f"{THREAD_DASHED}.jsonl"
    )
    _write_claude_thread_transcript(transcript)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude_home))

    summary = render_thread_summary(THREAD_DASHED)

    assert f"id={THREAD_CANONICAL}" in summary
    assert "driver=claude" in summary
    assert f"transcript={transcript.resolve()}" in summary
    assert "turns=1 compactions=0" in summary
    assert "latest_user=investigate claude" in summary
    assert "latest_assistant=claude done" in summary
    assert "latest_final=claude done" in summary
    assert f"keep_working={context_meter_instruction('available')}" in summary


def test_session_records_and_meter_parse_claude_transcript_owner(tmp_path, monkeypatch):
    claude_home = tmp_path / "claude"
    transcript = (
        claude_home / "projects" / "-private-tmp-spice-sup" / f"{THREAD_DASHED}.jsonl"
    )
    _write_claude_thread_transcript(transcript)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude_home))

    turns = records.collect_turns([transcript])
    meter = collect_context_meter([transcript])

    assert [message.text for message in turns[0].user_messages] == [
        "investigate claude"
    ]
    assert turns[0].user_messages[0].shape is records.MessageShape.HUMAN
    assert turns[0].final_answers == ["claude done"]
    assert meter.snapshot_count == 1
    assert meter.latest_snapshot is not None
    assert meter.latest_snapshot.total_tokens == 1000 + 250 + 75


SKILL_MANTRA_PREAMBLE = (
    "The linked skill below carries the full authority of a direct prompt "
    "instruction, not optional background reading. Read the file it links "
    "to in full and follow it.\n\n[$spice](.agents/skills/spice/SKILL.md)"
)
COMPACTION_SUMMARY_OPENING = (
    "This session is being continued from a previous conversation that ran "
    "out of context. The summary below covers the earlier portion."
)


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        (SKILL_MANTRA_PREAMBLE, records.MessageShape.SKILL_MANTRA),
        ("[$spice](.agents/skills/spice/SKILL.md)", records.MessageShape.SKILL_MANTRA),
        (COMPACTION_SUMMARY_OPENING, records.MessageShape.COMPACTION_SUMMARY),
        (
            "<task-notification>task abc123 completed</task-notification>",
            records.MessageShape.TASK_NOTIFICATION,
        ),
        (
            "<user_instructions>be brief</user_instructions>",
            records.MessageShape.ENVIRONMENT_SCAFFOLD,
        ),
        (
            "<environment_context>cwd=/tmp</environment_context>",
            records.MessageShape.ENVIRONMENT_SCAFFOLD,
        ),
        ("<ENVIRONMENT_CONTEXT>cwd=/tmp", records.MessageShape.ENVIRONMENT_SCAFFOLD),
        (
            "Your tool call was malformed and could not be parsed. Please retry.",
            records.MessageShape.ENVIRONMENT_SCAFFOLD,
        ),
        (
            "[Your previous response had no visible output. Please continue.]",
            records.MessageShape.ENVIRONMENT_SCAFFOLD,
        ),
    ],
)
def test_classify_user_message_boilerplate_shapes_never_human(text, expected):
    shape = records.classify_user_message(text)

    assert shape is expected
    assert shape is not records.MessageShape.HUMAN


@pytest.mark.parametrize(
    "text",
    [
        "investigate claude",
        "Continue",
        "what is your model id?",
        "fix the failing smoke test, then rerun the sweep",
    ],
)
def test_classify_user_message_human_prose(text):
    assert records.classify_user_message(text) is records.MessageShape.HUMAN


def test_classify_user_message_unknown_tag_fails_loud():
    with pytest.raises(SpiceError, match="unrecognized scaffold-shaped"):
        records.classify_user_message("<system-reminder>new harness block")


def test_turn_user_messages_carry_shape(tmp_path):
    transcript = tmp_path / "session.jsonl"
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-a"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": SKILL_MANTRA_PREAMBLE}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "drain the queue"}],
            },
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )

    turns = records.collect_turns([transcript])

    assert [message.shape for message in turns[0].user_messages] == [
        records.MessageShape.SKILL_MANTRA,
        records.MessageShape.HUMAN,
    ]


def test_collect_compactions_separates_summary_from_human_ask(tmp_path):
    transcript = tmp_path / "session.jsonl"
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"text": "about to compact"}],
            },
        },
        {"timestamp": "2026-01-01T00:00:01Z", "type": "compacted", "payload": {}},
        {
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": COMPACTION_SUMMARY_OPENING}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": SKILL_MANTRA_PREAMBLE}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "pick the work back up"}],
            },
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )

    compactions = records.collect_compactions([transcript])

    assert len(compactions) == 1
    assert compactions[0].last_assistant_before_text == "about to compact"
    assert compactions[0].summary_after_text == COMPACTION_SUMMARY_OPENING
    assert compactions[0].first_user_after_text == "pick the work back up"


def test_session_thread_reports_missing_driver_state(tmp_path, monkeypatch):
    codex_home = tmp_path / "codex"
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))
    # Pin the preferred driver so the assertion does not depend on the ambient
    # worktree's configured driver (a claude-configured worktree would surface
    # the claude resolver error first).
    monkeypatch.setenv("SPICE_AGENT_DRIVER", "codex")  # env-policy: allow

    with pytest.raises(SystemExit) as exc:
        render_thread_summary(THREAD_CANONICAL)

    assert f"Could not resolve thread {THREAD_CANONICAL}" in str(exc.value)
    assert "Missing codex state database" in str(exc.value)


def test_session_thread_parser_exposes_thread_id_argument():
    args = build_parser().parse_args(["session", "thread", THREAD_DASHED])

    assert args.session_action == "thread"
    assert args.thread_id == THREAD_DASHED


@pytest.fixture
def session_task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_git_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-session-learning")
    task_config.set_backend(str(backend))
    try:
        yield repo
    finally:
        task_config.set_backend(None)


def test_briefing_filters_turns_and_renders_git_posture(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "filtered.jsonl"
    _write_filter_transcript(transcript)
    monkeypatch.chdir(repo)

    briefing = render_briefing(
        [transcript],
        contains="needle",
        turn_ids=["turn-b"],
        tools=["apply_patch"],
        max_lines=BRIEFING_FILTER_MAX_LINES,
        max_bytes=BRIEFING_FILTER_MAX_BYTES,
    )

    assert "Filters" in briefing
    assert "contains=needle" in briefing
    assert "turn_ids=turn-b" in briefing
    assert "tools=apply_patch" in briefing
    assert "Latest Ask\n  needle request" in briefing
    assert "Working Set\n  spice/sessions/briefing.py touches=1" in briefing
    assert "Git\n  branch=main upstream=- ahead=- behind=-\n  dirty=clean" in briefing


def test_rehydration_ask_candidates_order_by_disposition_then_recency():
    candidates = [
        briefing_module.ask_candidate(
            "2026-01-01T00:00:04.000Z",
            "acked newest",
            disposition="acked",
        ),
        briefing_module.ask_candidate(
            "2026-01-01T00:00:01.000Z",
            "pending older",
            disposition="pending",
        ),
        briefing_module.ask_candidate(
            "2026-01-01T00:00:02.000Z",
            "pending newer",
            disposition="pending",
        ),
        briefing_module.ask_candidate(
            "2026-01-01T00:00:03.000Z",
            "refused latest",
            disposition="refused",
        ),
    ]

    ordered = briefing_module.sort_rehydration_candidates(candidates)

    assert [(candidate.rank_name, candidate.text) for candidate in ordered] == [
        (briefing_module.ASK_RANK_NAME, "pending newer"),
        (briefing_module.ASK_RANK_NAME, "pending older"),
        (briefing_module.ASK_RANK_NAME, "refused latest"),
        (briefing_module.ASK_RANK_NAME, "acked newest"),
    ]


def test_rehydration_file_candidates_order_by_last_touch_then_hotspot():
    older_hot = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:00.000Z",
        last_activity_ts="2026-01-01T00:00:05.000Z",
    )
    older_hot.touched_files["older_hot.py"] = 10
    newer_cool = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:10.000Z",
        last_activity_ts="2026-01-01T00:00:10.000Z",
    )
    newer_cool.touched_files["newer_cool.py"] = 1
    newer_hot = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:11.000Z",
        last_activity_ts="2026-01-01T00:00:10.000Z",
    )
    newer_hot.touched_files["newer_hot.py"] = 3

    ordered = briefing_module.sort_rehydration_candidates(
        briefing_module.collect_file_touch_candidates(
            [older_hot, newer_cool, newer_hot]
        )
    )

    assert [
        (candidate.rank_name, candidate.label, candidate.count) for candidate in ordered
    ] == [
        (briefing_module.FILE_RANK_NAME, "newer_hot.py", 3),
        (briefing_module.FILE_RANK_NAME, "newer_cool.py", 1),
        (briefing_module.FILE_RANK_NAME, "older_hot.py", 10),
    ]


def test_rehydration_command_candidates_order_by_failures_then_recency():
    older_failed = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:01.000Z",
        turn_id="older-failed",
        command_count=1,
        error_count=1,
    )
    newer_clean = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:03.000Z",
        turn_id="newer-clean",
        command_count=1,
        error_count=0,
    )
    newer_failed = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:02.000Z",
        turn_id="newer-failed",
        command_count=1,
        error_count=1,
    )

    ordered = briefing_module.sort_rehydration_candidates(
        briefing_module.collect_command_candidates(
            [older_failed, newer_clean, newer_failed]
        )
    )

    assert [(candidate.rank_name, candidate.label) for candidate in ordered] == [
        (briefing_module.COMMAND_RANK_NAME, "newer-failed"),
        (briefing_module.COMMAND_RANK_NAME, "older-failed"),
        (briefing_module.COMMAND_RANK_NAME, "newer-clean"),
    ]


def test_rehydration_recency_candidates_order_finals_commits_and_intents():
    older_turn = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:01.000Z",
        final_answers=["older final"],
    )
    newer_turn = records.TurnRecord(
        source_file="session.jsonl",
        start_ts="2026-01-01T00:00:02.000Z",
        final_answers=["newer final"],
    )
    commits = [
        records.CommitRecord(
            start_ts="2026-01-01T00:00:01.000Z",
            turn_id="older",
            source_file="session.jsonl",
            sha="1111111",
            line="commit 1111111 older",
            user=None,
        ),
        records.CommitRecord(
            start_ts="2026-01-01T00:00:02.000Z",
            turn_id="newer",
            source_file="session.jsonl",
            sha="2222222",
            line="commit 2222222 newer",
            user=None,
        ),
    ]
    compactions = [
        records.CompactionRecord(
            source_file="session.jsonl",
            ts="2026-01-01T00:00:01.000Z",
            first_user_after_text="older intent",
        ),
        records.CompactionRecord(
            source_file="session.jsonl",
            ts="2026-01-01T00:00:02.000Z",
            first_user_after_text="newer intent",
        ),
    ]

    finals = briefing_module.sort_rehydration_candidates(
        briefing_module.collect_final_candidates([older_turn, newer_turn])
    )
    ranked_commits = briefing_module.sort_rehydration_candidates(
        briefing_module.collect_commit_candidates(commits)
    )
    intents = briefing_module.sort_rehydration_candidates(
        briefing_module.collect_compaction_intent_candidates(compactions)
    )

    assert [(candidate.rank_name, candidate.text) for candidate in finals] == [
        (briefing_module.RECENCY_RANK_NAME, "newer final"),
        (briefing_module.RECENCY_RANK_NAME, "older final"),
    ]
    assert [(candidate.rank_name, candidate.label) for candidate in ranked_commits] == [
        (briefing_module.RECENCY_RANK_NAME, "2222222"),
        (briefing_module.RECENCY_RANK_NAME, "1111111"),
    ]
    assert [(candidate.rank_name, candidate.text) for candidate in intents] == [
        (briefing_module.RECENCY_RANK_NAME, "newer intent"),
        (briefing_module.RECENCY_RANK_NAME, "older intent"),
    ]


def test_task_plane_candidates_collect_lane_board_rows(tmp_path, monkeypatch):
    actor = ACTOR_A
    active = {
        "handle": "ACTIVE-1",
        "description": "Current implementation",
        "project": "session.briefing",
        "phase": "todo",
        "claim_by": actor,
        "claim_at": "2026-01-01T00:00:05.000Z",
        "acceptance": "render claimed work",
        "urgency": 1.0,
    }
    ready_high = {
        "handle": "READY-2",
        "description": "Higher ready item",
        "phase": "todo",
        "urgency": 12.5,
        "entry": "2026-01-01T00:00:02.000Z",
    }
    ready_low = {
        "handle": "READY-1",
        "description": "Lower ready item",
        "phase": "todo",
        "urgency": 7.0,
        "entry": "2026-01-01T00:00:03.000Z",
    }
    review = {
        "handle": "REVIEW-1",
        "description": "Review item",
        "phase": "review",
        "urgency": 9.0,
        "entry": "2026-01-01T00:00:04.000Z",
    }
    completed = {
        "handle": "DONE-1",
        "validation": "validated current behavior",
        "end": "2026-01-01T00:00:06.000Z",
    }
    oops = {
        "handle": "OOPS-1",
        "description": "Known tooling friction",
        "urgency": 2.0,
        "entry": "2026-01-01T00:00:01.000Z",
    }

    monkeypatch.setattr(briefing_module, "repo_root_from_cwd", lambda: tmp_path)
    monkeypatch.setattr(task_tw, "current_actor", lambda: actor)
    monkeypatch.setattr(task_identity, "render_handle", lambda row: row["handle"])
    monkeypatch.setattr(task_alloc, "is_hidden", lambda _row: False)
    monkeypatch.setattr(task_alloc, "visible_active_rows", lambda _actor: [active])
    monkeypatch.setattr(
        task_alloc,
        "visible_ready_rows",
        lambda _actor: [ready_low, ready_high, review],
    )

    def visible_rows(_actor, filters):
        if filters == ["status:pending", "phase:review"]:
            return [review]
        if filters == ["status:pending", "+BLOCKED"]:
            return [{"handle": "BLOCKED-1"}]
        if filters == ["status:completed"]:
            return [completed]
        return []

    monkeypatch.setattr(task_alloc, "visible_rows", visible_rows)
    monkeypatch.setattr(task_alloc, "oops_rows", lambda: [oops])

    ordered = briefing_module.sort_rehydration_candidates(
        briefing_module.collect_task_plane_candidates()
    )

    assert [(candidate.rank_name, candidate.text) for candidate in ordered] == [
        (
            briefing_module.TASK_PLANE_RANK_NAME,
            "claim ACTIVE-1 phase=todo project=session.briefing "
            "acceptance=render claimed work",
        ),
        (
            briefing_module.TASK_PLANE_RANK_NAME,
            "posture active=1 ready=2 review=1 blocked=1 oops=1",
        ),
        (
            briefing_module.TASK_PLANE_RANK_NAME,
            "ready READY-2 urgency=12.50 Higher ready item",
        ),
        (
            briefing_module.TASK_PLANE_RANK_NAME,
            "ready READY-1 urgency=7.00 Lower ready item",
        ),
        (
            briefing_module.TASK_PLANE_RANK_NAME,
            "review REVIEW-1 urgency=9.00 Review item",
        ),
        (
            briefing_module.TASK_PLANE_RANK_NAME,
            "completed DONE-1 validation=validated current behavior",
        ),
        (
            briefing_module.TASK_PLANE_RANK_NAME,
            "oops OOPS-1 Known tooling friction",
        ),
    ]


def test_briefing_renders_task_plane_section_with_explicit_overflow(monkeypatch):
    candidates = [
        briefing_module.RehydrationCandidate(
            kind="task_plane",
            timestamp=f"2026-01-01T00:00:{index:02d}.000Z",
            text=f"completed TASK-{index} validation=ok",
            rank_name=briefing_module.TASK_PLANE_RANK_NAME,
            rank_key=briefing_module.task_plane_rank_key(
                "completed", timestamp=f"2026-01-01T00:00:{index:02d}.000Z"
            ),
        )
        for index in range(briefing_module.TASK_PLANE_ROW_LIMIT + 2)
    ]
    monkeypatch.setattr(
        briefing_module, "collect_task_plane_candidates", lambda: candidates
    )

    briefing = render_briefing([], max_lines=200, max_bytes=20000)

    expected = ["Task Plane"]
    expected.extend(
        f"  completed TASK-{index} validation=ok"
        for index in range(
            briefing_module.TASK_PLANE_ROW_LIMIT + 1,
            1,
            -1,
        )
    )
    expected.append("  +2 more task-plane rows")
    assert _section_lines(briefing, "Task Plane") == expected


def test_briefing_learnings_use_active_stem_top_five(session_task_repo):
    repo = session_task_repo
    for index in range(6):
        learnings.confirm_learning_candidates(
            repo,
            "task",
            [
                learnings.LearningCandidate(
                    statement=f"Use durable session learning number {index}.",
                    source_task=f"TASK-{index}",
                    project_stem="task",
                )
            ],
            now=float(index),
        )
    learnings.confirm_learning_candidates(
        repo,
        "serve",
        [
            learnings.LearningCandidate(
                statement="Use unrelated serve learning only for serve tasks.",
                source_task="SERVE-1",
                project_stem="serve",
            )
        ],
        now=99.0,
    )
    active = create.add(
        "Read top task learnings",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["briefing renders the top five learnings"],
    )
    ops.claim(active)
    briefing = render_briefing([], max_lines=200, max_bytes=20000)

    assert _section_lines(briefing, "Learnings") == [
        "Learnings",
        "  stem=task",
        "  - Use durable session learning number 5. (confirmed=1, source=TASK-5)",
        "  - Use durable session learning number 4. (confirmed=1, source=TASK-4)",
        "  - Use durable session learning number 3. (confirmed=1, source=TASK-3)",
        "  - Use durable session learning number 2. (confirmed=1, source=TASK-2)",
        "  - Use durable session learning number 1. (confirmed=1, source=TASK-1)",
    ]


def test_briefing_surfaces_learning_from_prior_task_done(
    session_task_repo, tmp_path, monkeypatch
):
    codex_home = tmp_path / "codex-home"
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))
    monkeypatch.setattr(
        learnings,
        "evaluate_maxim",
        lambda *_args, **_kwargs: SimpleNamespace(agrees=True),
    )
    completed = create.add(
        "Distill session learning",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["task done stores a durable learning"],
    )
    ops.claim(completed)
    claimed = task_identity.resolve(completed)
    _write_learning_transcript(
        codex_home,
        thread_id=ACTOR_A,
        turn_id="turn-session-learning",
        timestamp=str(claimed["claim_at"]),
    )

    done_output = ops.done(completed, validation=["validated learning capture"])
    active = create.add(
        "Use session learning",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["briefing surfaces the active stem learning"],
    )
    ops.claim(active)
    transcript = tmp_path / "briefing.jsonl"
    _write_filter_transcript(transcript)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert "learnings: stored 1 accepted from 1 candidate(s)" in done_output
    assert _section_lines(briefing, "Learnings") == [
        "Learnings",
        "  stem=task",
        f"  - Use spice task next after phase boundaries (confirmed=1, source={completed})",
    ]


def test_briefing_reports_deadlettered_inbox_items(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    write_inbox_item(
        repo,
        "20260101T000000000001Z.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    deadletter_inbox_item(repo, "20260101T000000000001Z")
    monkeypatch.chdir(repo)

    briefing = render_briefing([], max_lines=200, max_bytes=20000)

    assert "Inbox\n  pending=0" in briefing
    assert "deadlettered=1" in briefing
    assert "source=inbox_deadletter" in briefing
    assert "requeue=spice agent requeue-deadletter <key>" in briefing
    assert "deadlettered_inbox key=20260101T000000000001Z" in briefing


def test_briefing_pending_inbox_ack_guidance_uses_open_response_copy(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    write_inbox_item(
        repo,
        "20260101T000000000002Z.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([], max_lines=200, max_bytes=20000)

    assert "Inbox\n  pending=1" in briefing
    assert (
        "Real-time N/ACK loop: put a plain-text ACK or reasoned NACK header "
        "near the start of each working assistant message"
    ) in briefing
    assert "ACK <key> [<key> ...]: <what changed or was captured>" in briefing
    assert "acknowledged keys clear once processed" in briefing
    assert "NACK <key>: <why this cannot be done>" in briefing
    assert "refused keys clear once processed" in briefing
    assert "Do not bury ACKs or NACKs mid-message" in briefing
    assert "understood" not in briefing


def test_agent_requeue_deadletter_command_restores_pending_item(
    tmp_path, monkeypatch, capsys
):
    repo = _init_git_repo(tmp_path / "repo")
    write_inbox_item(
        repo,
        "20260101T000000000002Z.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    deadletter_inbox_item(repo, "20260101T000000000002Z")
    monkeypatch.chdir(repo)
    args = build_parser().parse_args(
        ["agent", "requeue-deadletter", "20260101T000000000002Z"]
    )

    assert args.func(args) == 0

    output = capsys.readouterr().out
    assert "requeued_deadletter key=20260101T000000000002Z" in output
    assert [item.name for item in collect_inbox_items(repo)] == [
        "20260101T000000000002Z.txt"
    ]
    assert collect_deadlettered_inbox_items(repo) == []


def test_briefing_dirty_git_posture_includes_policy_pressure_and_ages(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "filtered.jsonl"
    _write_filter_transcript(transcript)
    oversize = repo / "oversize.py"
    magic = repo / "magic.py"
    oversize.write_text("def oversized():\n    return 1\n", encoding="utf-8")
    magic.write_text("def check(value):\n    return value > 99\n", encoding="utf-8")
    now = time.time()
    old_mtime = now - 120
    new_mtime = now - 5
    os.utime(oversize, (old_mtime, old_mtime))
    os.utime(magic, (new_mtime, new_mtime))
    monkeypatch.chdir(repo)

    monkeypatch.setattr(
        briefing_module.fileloc,
        "scan_loc_violations",
        lambda paths, **_kwargs: [
            briefing_module.fileloc.LocFinding(
                path="oversize.py",
                line_count=1601,
                byte_count=100,
                over_line_limit=True,
                over_byte_limit=False,
                line_limit=1500,
                byte_limit=120_000,
            )
        ],
    )
    monkeypatch.setattr(
        briefing_module,
        "_scan_dirty_complexity_pressure",
        lambda paths, **_kwargs: [
            briefing_module.DirtyComplexityRegression(
                path="oversize.py",
                function_name="oversized",
                metric="ccn",
                value=31,
                active_threshold=30,
                baseline_value=None,
            )
        ],
    )
    monkeypatch.setattr(
        briefing_module.magicnums,
        "detect_magic_regressions",
        lambda paths, **_kwargs: [
            briefing_module.magicnums.MagicFinding(
                path="magic.py",
                line=2,
                literal="99",
            )
        ],
    )

    briefing = render_briefing(
        [transcript],
        max_lines=BRIEFING_FILTER_MAX_LINES,
        max_bytes=BRIEFING_FILTER_MAX_BYTES,
    )

    assert "dirty=2 path(s)" in briefing
    assert (
        "pressure severity=high findings=3 files=2 scanned=2/2 "
        "file-loc=1 complexity=1 magic-numbers=1"
    ) in briefing
    assert "dirty_age=oldest=oversize.py:" in briefing
    assert "newest=magic.py:" in briefing
    assert "pressure_file=oversize.py [complexity-ccn,file-loc]" in briefing
    assert "pressure_file=magic.py [magic]" in briefing


def test_briefing_budget_prunes_with_explanation(tmp_path):
    transcript = tmp_path / "filtered.jsonl"
    _write_filter_transcript(transcript)

    briefing = render_briefing(
        [transcript],
        max_lines=BRIEFING_PRUNE_MAX_LINES,
        max_bytes=BRIEFING_FILTER_MAX_BYTES,
        explain_pruning=True,
    )

    assert len(briefing.splitlines()) == 6
    assert "Pruning original_lines=" in briefing


def test_session_briefing_parser_exposes_budget_and_filter_flags():
    args = build_parser().parse_args(
        [
            "session",
            "briefing",
            "--start",
            "2026-01-01T00:00:00Z",
            "--end",
            "2026-01-01T00:00:10Z",
            "--contains",
            "needle",
            "--turn-id",
            "turn-b",
            "--tool",
            "apply_patch",
            "--max-lines",
            str(BRIEFING_PARSE_MAX_LINES),
            "--max-bytes",
            str(BRIEFING_PARSE_MAX_BYTES),
            "--explain-pruning",
        ]
    )

    assert args.session_action == "briefing"
    assert args.contains == "needle"
    assert args.turn_ids == ["turn-b"]
    assert args.tools == ["apply_patch"]
    assert args.max_lines == BRIEFING_PARSE_MAX_LINES
    assert args.max_bytes == BRIEFING_PARSE_MAX_BYTES
    assert args.explain_pruning is True


def test_sweep_and_timeline_parser_share_filter_flags():
    parser = build_parser()
    sweep = parser.parse_args(
        [
            "session",
            "sweep",
            "--contains",
            "needle",
            "--turn-id",
            "turn-b",
            "--tool",
            "apply_patch",
        ]
    )
    timeline = parser.parse_args(
        [
            "session",
            "timeline",
            "--contains",
            "needle",
            "--turn-id",
            "turn-b",
            "--tool",
            "apply_patch",
        ]
    )

    assert sweep.contains == "needle"
    assert sweep.turn_ids == ["turn-b"]
    assert sweep.tools == ["apply_patch"]
    assert timeline.contains == "needle"
    assert timeline.turn_ids == ["turn-b"]
    assert timeline.tools == ["apply_patch"]


def test_briefing_default_horizon_is_count_bound(tmp_path):
    transcript = tmp_path / "horizon.jsonl"
    _write_horizon_transcript(
        transcript,
        asks=[
            ("2026-01-01T01:00:00Z", "before count horizon"),
            ("2026-01-01T07:00:00Z", "inside first count window"),
            ("2026-01-01T13:00:00Z", "inside second count window"),
            ("2026-01-01T19:00:00Z", "inside current count window"),
        ],
        compactions=[
            "2026-01-01T06:00:00Z",
            "2026-01-01T12:00:00Z",
            "2026-01-01T18:00:00Z",
        ],
    )

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert "files=horizon.jsonl turns=3" in briefing
    assert (
        "horizon_basis=compaction_count start=2026-01-01T06:00:00.000Z compactions=3/3"
    ) in briefing
    assert "Latest Ask\n  inside current count window" in briefing


def test_sweep_horizon_extends_to_wall_clock_floor(tmp_path):
    transcript = tmp_path / "horizon.jsonl"
    _write_horizon_transcript(
        transcript,
        asks=[
            ("2026-01-01T01:00:00Z", "floor request"),
            ("2026-01-01T10:05:00Z", "recent request one"),
            ("2026-01-01T10:15:00Z", "recent request two"),
            ("2026-01-01T10:30:00Z", "recent request three"),
        ],
        compactions=[
            "2026-01-01T00:00:00Z",
            "2026-01-01T10:00:00Z",
            "2026-01-01T10:10:00Z",
            "2026-01-01T10:20:00Z",
        ],
    )

    sweep = render_sweep([transcript], count=3)

    assert "Sweep\n  windows=4 files=1" in sweep
    assert (
        "horizon_basis=wall_clock_floor start=2026-01-01T00:00:00.000Z compactions=4/3"
    ) in sweep
    assert "Window 0 (from 2026-01-01T00:00:00.000Z)" in sweep
    assert "ask 2026-01-01T01:00:00.000Z floor request" in sweep


def test_sweep_horizon_caps_requested_count(tmp_path):
    transcript = tmp_path / "horizon.jsonl"
    _write_horizon_transcript(
        transcript,
        asks=[
            ("2026-01-01T01:30:00Z", "cap window one"),
            ("2026-01-01T02:30:00Z", "cap window two"),
            ("2026-01-01T03:30:00Z", "cap window three"),
            ("2026-01-01T04:30:00Z", "cap window four"),
            ("2026-01-01T05:30:00Z", "cap current window"),
        ],
        compactions=[
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "2026-01-01T02:00:00Z",
            "2026-01-01T03:00:00Z",
            "2026-01-01T04:00:00Z",
            "2026-01-01T05:00:00Z",
        ],
    )

    sweep = render_sweep([transcript], count=9)

    assert "Sweep\n  windows=5 files=1" in sweep
    assert (
        "horizon_basis=hard_cap start=2026-01-01T01:00:00.000Z compactions=5/9"
    ) in sweep
    assert "Window 4 (from 2026-01-01T05:00:00.000Z)" in sweep
    assert "ask 2026-01-01T05:30:00.000Z cap current window" in sweep


def test_mint_incepted_shape_and_collision_advance():
    first = mint_incepted(set())
    assert INCEPTED_RE.match(first) is not None
    assert len(first) == STAMP_WIDTH
    # A collision on the freshly minted stamp forces a distinct later stamp.
    second = mint_incepted({first})
    assert second != first


def test_incepted_alphabet_excludes_vowels():
    assert not set("AEIOUaeiou") & set(mint_incepted(set()))


def test_key_for_prefers_project_segment():
    assert key_for("serve.livebus", "anything at all") == "LIVEBUS"
    assert key_for(None, "fix the broken thing") == "FTBT"


def test_render_handle_is_key_dash_incepted():
    incepted = encode_width(1)
    row = {
        "incepted": incepted,
        "project": "task.alloc",
        "description": "allocate fairly",
    }
    assert render_handle(row) == f"ALLOC-{incepted}"


def test_codec_round_trips_and_pads_to_fixed_width():
    for value in (0, 1, BASE - 1, BASE, 1000, 1_700_000_000_000):
        assert decode(encode(value)) == value
        assert decode(encode_width(value)) == value
        assert len(encode_width(value)) == STAMP_WIDTH
    assert encode(0) == "0"
    assert encode(BASE - 1) == "z"
    assert encode(BASE) == "10"
    assert encode_width(BASE - 1) == "0" * (STAMP_WIDTH - 1) + "z"


def test_codec_fixed_width_preserves_numeric_order():
    values = [0, 1, BASE - 1, BASE, 1000, 1_700_000_000_000, BASE**STAMP_WIDTH - 1]
    encoded = [encode_width(value) for value in values]
    assert encoded == sorted(encoded)


def test_epoch_millis_counts_whole_milliseconds():
    from datetime import UTC, datetime

    assert epoch_millis(datetime(1970, 1, 1, tzinfo=UTC)) == 0


def _write_state_db(codex_home, thread_id, transcript) -> None:
    codex_home.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(codex_home / "state_5.sqlite") as connection:
        connection.execute("CREATE TABLE threads (id TEXT, rollout_path TEXT)")
        connection.execute(
            "INSERT INTO threads (id, rollout_path) VALUES (?, ?)",
            (thread_id, str(transcript)),
        )


def _write_thread_transcript(path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-thread"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "investigate thread"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{}",
            },
        },
        {
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"text": "working on thread"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "last_agent_message": "thread done",
            },
        },
    ]
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )


def _write_gzip_jsonl(path, events) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with gzip.open(path, "wt", encoding="utf-8") as handle:
        for event in events:
            handle.write(f"{json.dumps(event)}\n")


def _write_claude_thread_transcript(path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "user",
            "message": {"role": "user", "content": "investigate claude"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "assistant",
            "message": {
                "role": "assistant",
                "stop_reason": "end_turn",
                "content": [{"type": "text", "text": "claude done"}],
                "usage": {
                    "input_tokens": 1000,
                    "cache_read_input_tokens": 250,
                    "cache_creation_input_tokens": 0,
                    "output_tokens": 75,
                },
            },
        },
    ]
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )


def _write_filter_transcript(path) -> None:
    patch_args = json.dumps(
        {
            "input": (
                "*** Begin Patch\n"
                "*** Update File: spice/sessions/briefing.py\n"
                "@@\n"
                "+changed\n"
                "*** End Patch\n"
            )
        }
    )
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-a"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "ignore request"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {"type": "function_call", "name": "exec_command"},
        },
        {
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        },
        {
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-b"},
        },
        {
            "timestamp": "2026-01-01T00:00:05Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "needle request"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:06Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "apply_patch",
                "arguments": patch_args,
            },
        },
        {
            "timestamp": "2026-01-01T00:00:07Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{"text": "needle final"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:08Z",
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        },
    ]
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )


def _write_horizon_transcript(path, *, asks, compactions) -> None:
    events = []
    for index, (timestamp, text) in enumerate(asks):
        turn_id = f"turn-horizon-{index}"
        events.extend(
            [
                {
                    "timestamp": timestamp,
                    "type": "event_msg",
                    "payload": {"type": "task_started", "turn_id": turn_id},
                },
                {
                    "timestamp": timestamp,
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{"text": text}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "phase": "final_answer",
                        "content": [{"text": f"completed {text}"}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "event_msg",
                    "payload": {"type": "task_complete"},
                },
            ]
        )
    events.extend(
        {"timestamp": timestamp, "type": "compacted", "payload": {}}
        for timestamp in compactions
    )
    events.sort(key=lambda event: event["timestamp"])
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )


def _write_learning_transcript(
    codex_home,
    *,
    thread_id: str,
    turn_id: str,
    timestamp: str,
) -> None:
    transcript = codex_home / "sessions" / f"rollout-{thread_id}.jsonl"
    transcript.parent.mkdir(parents=True, exist_ok=True)
    events: list[dict[str, object]] = [
        {
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": turn_id},
        },
        {
            "timestamp": timestamp,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {"text": "Lesson: Use spice task next after phase boundaries."}
                ],
            },
        },
        {
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events),
        encoding="utf-8",
    )


def _section_lines(output: str, header: str) -> list[str]:
    lines = output.splitlines()
    section = [lines[lines.index(header)]]
    for line in lines[lines.index(header) + 1 :]:
        if line and not line.startswith(" "):
            break
        section.append(line)
    return section


def _init_git_repo(path) -> None:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _run(cwd, *args: str) -> None:
    subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)


def test_collect_turns_derives_claude_turns_from_prompt_id(tmp_path, monkeypatch):
    from spice.agent.driver import CLAUDE_DRIVER

    monkeypatch.setattr(records, "driver_for_transcript", lambda _path: CLAUDE_DRIVER)
    lines = [
        {
            "type": "user",
            "timestamp": "2026-01-01T00:00:00Z",
            "promptId": "p1",
            "message": {"content": "first prompt"},
        },
        {
            "type": "assistant",
            "timestamp": "2026-01-01T00:00:01Z",
            "message": {"content": [{"type": "text", "text": "reply one"}]},
        },
        {
            "type": "user",
            "timestamp": "2026-01-01T00:00:02Z",
            "promptId": "p2",
            "message": {"content": "second prompt"},
        },
        {
            "type": "assistant",
            "timestamp": "2026-01-01T00:00:03Z",
            "message": {"content": [{"type": "text", "text": "reply two"}]},
        },
    ]
    path = tmp_path / "claude.jsonl"
    path.write_text("\n".join(json.dumps(line) for line in lines) + "\n")

    turns = records.collect_turns([path])

    assert [turn.turn_id for turn in turns] == ["p1", "p2"]
    assert records.filter_turns(turns, turn_ids=["p2"]) == [turns[1]]


def test_filter_turns_fails_loudly_when_turns_have_no_ids():
    idless = [
        records.TurnRecord(source_file="s.jsonl", start_ts="2026-01-01T00:00:00Z")
    ]
    with pytest.raises(SpiceError):
        records.filter_turns(idless, turn_ids=["whatever"])
    # No turn-id filter requested: id-less turns pass through untouched.
    assert records.filter_turns(idless) == idless
