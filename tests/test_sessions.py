"""Session forensics: context metering and identity primitives."""

import argparse
import json
import os
import sqlite3
import subprocess
import time

import pytest

from spice.cli.parser import build_parser
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    deadletter_inbox_item,
    write_inbox_item,
)
from spice.sessions import briefing as briefing_module
from spice.sessions.briefing import render_briefing
from spice.sessions.cli import handle_session, render_thread_summary
from spice.sessions.meter import (
    ActiveContextSnapshot,
    active_context_percent,
    collect_context_meter,
    context_meter_instruction,
    context_pressure_level,
    context_pressure_should_warn,
)
from spice.sessions import records
from spice.sessions.util import first_text, normalize_timestamp
from spice.errors import SpiceError
from spice.tasks.identity import (
    INCEPTED_RE,
    canonicalize_zulu_free_handle,
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

    assert turns[0].user_messages == ["investigate claude"]
    assert turns[0].final_answers == ["claude done"]
    assert meter.snapshot_count == 1
    assert meter.latest_snapshot is not None
    assert meter.latest_snapshot.total_tokens == 1000 + 250 + 75


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
        "Real-time ACK loop: put a plain-text ACK header near the start of each "
        "working assistant message: "
        "ACK <key> [<key> ...]: <what changed or was captured>"
    ) in briefing
    assert "acknowledged keys clear once processed" in briefing
    assert "Do not bury ACKs mid-message or save them for final response" in briefing
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
        lambda paths, *, repo_root: [
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


def test_mint_incepted_shape_and_collision_advance():
    existing = {"20260101T000000000001Z"}
    stamp = mint_incepted(existing)
    assert INCEPTED_RE.match(stamp) is not None


def test_key_for_prefers_project_segment():
    assert key_for("serve.livebus", "anything at all") == "LIVEBUS"
    assert key_for(None, "fix the broken thing") == "FTBT"


def test_render_handle_is_key_dash_incepted():
    row = {
        "incepted": "20260101T000000000001Z",
        "project": "task.alloc",
        "description": "allocate fairly",
    }
    assert render_handle(row) == "ALLOC-20260101T000000000001Z"


def test_zulu_free_handle_gains_z():
    handle, added = canonicalize_zulu_free_handle("ALLOC-20260101T000000000001")
    assert handle == "ALLOC-20260101T000000000001Z"
    assert added is True


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
