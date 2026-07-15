"""Session forensics: context metering and identity primitives."""

import argparse
import gzip
import json
import sqlite3
import subprocess

import pytest

from spice.cli.parser import build_parser
from spice.sessions.briefing import render_briefing
from spice.sessions.cli import handle_session, render_thread_summary
from spice.sessions.meter import (
    ActiveContextSnapshot,
    active_context_percent,
    collect_latest_context_meter,
    context_pressure_level,
    context_pressure_should_warn,
)
from spice.sessions import records
from spice.sessions.util import first_text, normalize_timestamp
from spice.errors import SpiceError
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
from tests.test_sessionfixtures import (
    SUPERVISED_FIXTURES,
    transcript_driver_for_fixture,
)

CODEX_HOME_ENV = "CODEX_HOME"  # env-policy: allow
THREAD_DASHED = "11111111-2222-3333-4444-555555555555"
THREAD_CANONICAL = "11111111222233334444555555555555"
GZIP_SESSION_TOTAL_TOKENS = 115


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


def test_session_briefing_reads_direct_gzip_jsonl_path(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.chdir(repo)
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
    meter = collect_latest_context_meter([transcript])

    output = capsys.readouterr().out
    assert "files=session.jsonl.gz turns=1" in output
    assert _section_headers(output) == [
        "Briefing",
        "Horizon",
        "Latest Final",
        "Activity",
        "Git",
        "Inbox",
    ]
    assert "Latest Final\n  compressed done" in output
    assert meter.snapshot_count == 1
    assert meter.latest_snapshot is not None
    assert meter.latest_snapshot.total_tokens == GZIP_SESSION_TOTAL_TOKENS


def test_session_briefing_excludes_non_human_transcript_messages(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    transcript = tmp_path / "scaffold.jsonl"
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-scaffold"},
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
                "content": [{"text": COMPACTION_SUMMARY_OPENING}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"text": "<task-notification>task done</task-notification>"}
                ],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"text": "<environment_context>cwd=repo</environment_context>"}
                ],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:05Z",
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert _section_headers(briefing) == [
        "Briefing",
        "Horizon",
        "Activity",
        "Git",
        "Inbox",
    ]


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


def test_session_summary_keeps_activity_shape_without_pressure_guidance(
    tmp_path, capsys
):
    transcript = tmp_path / "summary.jsonl"
    _write_thread_transcript(transcript)

    handle_session(_summary_args(transcript, recent=3))

    output = capsys.readouterr().out
    assert output.splitlines()[:4] == [
        "Summary",
        "  turns=1 completed=1 compactions=0",
        "  commands=1 patches=0 errors=0 web_searches=0",
        "Recent Prompts",
    ]
    assert "  2026-01-01T00:00:00.000Z investigate thread" in output


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


def _summary_args(transcript, **overrides):
    values = {
        "session_action": "summary",
        "recent": 8,
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
    lines = summary.splitlines()

    assert "Thread" in summary
    assert f"id={THREAD_CANONICAL}" in summary
    assert "driver=codex" in summary
    assert f"transcript={transcript.resolve()}" in summary
    assert "turns=1 compactions=0" in summary
    assert "latest_user=investigate thread" in summary
    assert "latest_assistant=thread done" in summary
    assert "latest_final=thread done" in summary
    assert "commands=1 patches=0 errors=0 web_searches=0" in summary
    assert lines[4].startswith("  turns=1 compactions=0 ")
    assert lines[5] == "Latest Activity"


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


def test_session_records_and_meter_parse_claude_transcript_owner(tmp_path, monkeypatch):
    claude_home = tmp_path / "claude"
    transcript = (
        claude_home / "projects" / "-private-tmp-spice-sup" / f"{THREAD_DASHED}.jsonl"
    )
    _write_claude_thread_transcript(transcript)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude_home))

    turns = records.collect_turns([transcript])
    meter = collect_latest_context_meter([transcript])

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
COMPACTION_INTENT_TEXT = (
    "Keep draining allocator-selected tasks.\n\nValidate before completion."
)
COMPACTION_SUMMARY_TEXT = (
    f"{COMPACTION_SUMMARY_OPENING}\n\n"
    "Summary:\n"
    "1. Primary Request and Intent:\n"
    "   Keep draining allocator-selected tasks.\n\n"
    "   Validate before completion.\n\n"
    "2. Key Technical Concepts:\n"
    "   - spice task allocator\n"
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
            "<recommended_plugins>plugin catalog</recommended_plugins>",
            records.MessageShape.ENVIRONMENT_SCAFFOLD,
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


def test_classify_user_message_unknown_tag_is_environment_scaffold():
    shape = records.classify_user_message(
        "<future-harness-block>new envelope</future-harness-block>"
    )

    assert shape is records.MessageShape.ENVIRONMENT_SCAFFOLD


@pytest.mark.parametrize(
    "text",
    [
        "<future-harness-block>new envelope",
        "<future-harness-block>new envelope</different-block>",
    ],
)
def test_classify_user_message_non_envelope_tag_text_is_human(text):
    assert records.classify_user_message(text) is records.MessageShape.HUMAN


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


def test_supervised_session_fixtures_cover_all_user_message_shapes(monkeypatch):
    for transcript in SUPERVISED_FIXTURES:
        with transcript_driver_for_fixture(monkeypatch, transcript):
            turns = records.collect_turns([transcript])
            compactions = records.collect_compactions([transcript])

        shapes = {message.shape for turn in turns for message in turn.user_messages}
        assert shapes == set(records.MessageShape)
        assert len(compactions) == 3
        assert [record.summary_after_text for record in compactions] == [
            COMPACTION_SUMMARY_OPENING,
            COMPACTION_SUMMARY_OPENING,
            COMPACTION_SUMMARY_OPENING,
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
                "content": [{"text": COMPACTION_SUMMARY_TEXT}],
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
    assert compactions[0].summary_after_text == COMPACTION_SUMMARY_TEXT
    assert compactions[0].intent_text == COMPACTION_INTENT_TEXT
    assert compactions[0].first_user_after_text == "pick the work back up"


def test_collect_compactions_marks_unparseable_summary_intent(tmp_path):
    transcript = tmp_path / "session.jsonl"
    events = [
        {"timestamp": "2026-01-01T00:00:00Z", "type": "compacted", "payload": {}},
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": COMPACTION_SUMMARY_OPENING}],
            },
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )

    compactions = records.collect_compactions([transcript])

    assert compactions[0].intent_text == records.UNPARSEABLE_COMPACTION_INTENT


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
            "timestamp": "2026-01-01T00:00:02.500Z",
            "type": "response_item",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 80_000,
                        "cached_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 80_000,
                    },
                    "total_token_usage": {"total_tokens": 80_000},
                    "model_context_window": 100_000,
                },
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


def _section_headers(output: str) -> list[str]:
    return [line for line in output.splitlines() if line and not line.startswith(" ")]


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
