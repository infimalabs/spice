"""Session briefing and rehydration rendering tests."""

import json
import os
import shutil
import subprocess
import time
from datetime import datetime
from types import SimpleNamespace

import pytest

from spice.agent.driver import DRIVER
from spice.cli.parser import build_parser
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    AckStateWrite,
    record_acked_inbox_items,
)
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    compose_inbox_text,
    deadletter_inbox_item,
    write_inbox_item,
)
from spice.sessions import briefing as briefing_module
from spice.sessions import briefingpressure
from spice.sessions.briefing import render_briefing, render_sweep
from spice.sessions import learnings, records
from spice.tasks import config as task_config
from spice.tasks import create, identity as task_identity, ops
from tests.test_sessionfixtures import (
    SUPERVISED_FIXTURES,
    transcript_driver_for_fixture,
)

CODEX_HOME_ENV = "CODEX_HOME"  # env-policy: allow
BRIEFING_FILTER_MAX_LINES = 80
BRIEFING_FILTER_MAX_BYTES = 10_000
BRIEFING_PRUNE_MAX_LINES = 6
BRIEFING_PARSE_MAX_LINES = 10
BRIEFING_PARSE_MAX_BYTES = 1_000
ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


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


def test_compaction_intent_candidates_use_parsed_summary_intent():
    compactions = [
        records.CompactionRecord(
            source_file="session.jsonl",
            ts="2026-01-01T00:00:01.000Z",
            last_assistant_before_text="assistant context",
            summary_after_text=(
                "This session is being continued from a previous conversation."
            ),
            intent_text="parsed recovered ask",
        )
    ]

    candidates = briefing_module.collect_compaction_intent_candidates(compactions)

    assert [(candidate.text, candidate.label) for candidate in candidates] == [
        ("parsed recovered ask", "assistant context")
    ]


def test_briefing_recovery_uses_parsed_intent_and_prior_steering(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "recovery.jsonl"
    _write_recovery_transcript(transcript, include_summary=True)
    _record_ack_state_asks(
        repo,
        [
            ("2026-01-01T00:00:08Z", "older steering before compaction"),
            ("2026-01-01T00:00:09Z", "operator asks before compaction"),
            ("2026-01-01T00:00:13Z", "newer steering after compaction"),
        ],
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert _section_lines(briefing, "Recovery") == [
        "Recovery",
        "  latest_compaction=2026-01-01T00:00:10.000Z",
        "  assistant_before=ready to compact",
        "  intent=Keep draining allocator-selected tasks. Validate before completion.",
        "  user_after=continue with recovery",
        "  steering=acked 2026-01-01T00:00:09.000Z "
        "key=20260101T000009000000Z operator asks before compaction",
    ]


def test_briefing_recovery_leads_when_latest_event_is_compaction(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "freshly-compacted.jsonl"
    _write_recovery_transcript(transcript, include_summary=False)
    _record_ack_state_ask(
        repo,
        "20260101T000009000000Z",
        "operator asks before compaction",
        ACK_DISPOSITION_ACKED,
        "2026-01-01T00:00:09Z",
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)
    lines = briefing.splitlines()

    assert lines.index("Recovery") < lines.index("Guidance")
    assert _section_lines(briefing, "Recovery") == [
        "Recovery",
        "  latest_compaction=2026-01-01T00:00:10.000Z",
        "  assistant_before=ready to compact",
        "  user_after=-",
        "  steering=acked 2026-01-01T00:00:09.000Z "
        "key=20260101T000009000000Z operator asks before compaction",
    ]


def test_briefing_renders_supervised_fixture_recovery(monkeypatch):
    for transcript in SUPERVISED_FIXTURES:
        with transcript_driver_for_fixture(monkeypatch, transcript):
            briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

        assert f"files={transcript.name}" in briefing
        assert "compactions=3/3" in briefing
        assert "Recovery" in briefing
        assert "Latest Final" in briefing


def test_recovery_lines_do_not_render_assistant_fallback_as_user_after():
    [candidate] = briefing_module.collect_compaction_intent_candidates(
        [
            records.CompactionRecord(
                source_file="session.jsonl",
                ts="2026-01-01T00:00:01.000Z",
                last_assistant_before_text="assistant fallback text",
                first_user_after_text="",
            )
        ]
    )

    lines = briefing_module._recovery_lines([candidate], [])

    assert candidate.text == "assistant fallback text"
    assert lines == [
        "Recovery",
        "  latest_compaction=2026-01-01T00:00:01.000Z",
        "  assistant_before=assistant fallback text",
        "  user_after=-",
    ]


def test_recovery_lines_render_populated_first_user_after_text():
    [candidate] = briefing_module.collect_compaction_intent_candidates(
        [
            records.CompactionRecord(
                source_file="session.jsonl",
                ts="2026-01-01T00:00:01.000Z",
                last_assistant_before_text="assistant before",
                first_user_after_text="operator resumes task",
            )
        ]
    )

    lines = briefing_module._recovery_lines([candidate], [])

    assert lines == [
        "Recovery",
        "  latest_compaction=2026-01-01T00:00:01.000Z",
        "  assistant_before=assistant before",
        "  user_after=operator resumes task",
    ]


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
        briefingpressure.fileloc,
        "scan_loc_violations",
        lambda paths, **_kwargs: [
            briefingpressure.fileloc.LocFinding(
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
        briefingpressure,
        "_scan_dirty_complexity_pressure",
        lambda paths, **_kwargs: [
            briefingpressure.DirtyComplexityRegression(
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
        briefingpressure.magicnums,
        "detect_magic_regressions",
        lambda paths, **_kwargs: [
            briefingpressure.magicnums.MagicFinding(
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


def test_briefing_default_horizon_is_count_bound(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    asks = [
        ("2026-01-01T01:00:00Z", "before count horizon"),
        ("2026-01-01T07:00:00Z", "inside first count window"),
        ("2026-01-01T13:00:00Z", "inside second count window"),
        ("2026-01-01T19:00:00Z", "inside current count window"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=[
            "2026-01-01T06:00:00Z",
            "2026-01-01T12:00:00Z",
            "2026-01-01T18:00:00Z",
        ],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert "files=horizon.jsonl turns=3" in briefing
    assert (
        "horizon_basis=compaction_count start=2026-01-01T06:00:00.000Z compactions=3/3"
    ) in briefing
    assert _section_lines(briefing, "Latest Ask") == [
        "Latest Ask",
        "  acked 2026-01-01T19:00:00.000Z "
        "key=20260101T190000000000Z inside current count window",
    ]


def test_briefing_young_session_floor_extends_to_session_start(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    asks = [
        ("2026-01-01T08:00:00Z", "before first young compaction"),
        ("2026-01-01T10:30:00Z", "young current request"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=[
            "2026-01-01T10:00:00Z",
            "2026-01-01T10:20:00Z",
        ],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert "files=horizon.jsonl turns=2" in briefing
    assert (
        "horizon_basis=wall_clock_floor start=session start compactions=2/3"
    ) in briefing
    assert _section_lines(briefing, "Recent Asks") == [
        "Recent Asks",
        "  acked 2026-01-01T08:00:00.000Z "
        "key=20260101T080000000000Z before first young compaction",
    ]


def test_explicit_start_wins_over_adaptive_horizon_in_briefing_and_sweep(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    asks = [
        ("2026-01-01T01:00:00Z", "operator explicit start request"),
        ("2026-01-01T07:00:00Z", "inside first count window"),
        ("2026-01-01T13:00:00Z", "inside second count window"),
        ("2026-01-01T19:00:00Z", "inside current count window"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=[
            "2026-01-01T06:00:00Z",
            "2026-01-01T12:00:00Z",
            "2026-01-01T18:00:00Z",
        ],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    briefing = render_briefing(
        [transcript],
        start="2026-01-01T01:00:00.000Z",
        max_lines=200,
        max_bytes=20000,
    )
    sweep = render_sweep(
        [transcript],
        count=3,
        start="2026-01-01T01:00:00.000Z",
    )

    assert "files=horizon.jsonl turns=4" in briefing
    assert "Filters\n  start=2026-01-01T01:00:00.000Z" in briefing
    assert "Window 0 (from 2026-01-01T01:00:00.000Z)" in sweep
    assert (
        "ask acked 2026-01-01T01:00:00.000Z "
        "key=20260101T010000000000Z operator explicit start request"
    ) in sweep


def test_sweep_zero_windows_falls_back_to_public_briefing(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    _write_horizon_transcript(
        transcript,
        asks=[("2026-01-01T10:30:00Z", "young current request")],
        compactions=["2026-01-01T10:00:00Z"],
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript])
    payload = briefing_module.build_briefing_payload([transcript], sweep_count=0)

    assert payload.sweep_windows == ()
    assert briefing_module.render_sweep_payload(payload) == briefing
    assert render_sweep([transcript], count=0) == briefing


def test_sweep_horizon_extends_to_wall_clock_floor(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    asks = [
        ("2026-01-01T01:00:00Z", "floor request"),
        ("2026-01-01T10:05:00Z", "recent request one"),
        ("2026-01-01T10:15:00Z", "recent request two"),
        ("2026-01-01T10:30:00Z", "recent request three"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=[
            "2026-01-01T00:00:00Z",
            "2026-01-01T10:00:00Z",
            "2026-01-01T10:10:00Z",
            "2026-01-01T10:20:00Z",
        ],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    sweep = render_sweep([transcript], count=3)

    assert "Sweep\n  windows=4 files=1" in sweep
    assert (
        "horizon_basis=wall_clock_floor start=2026-01-01T00:00:00.000Z compactions=4/3"
    ) in sweep
    assert "Window 0 (from 2026-01-01T00:00:00.000Z)" in sweep
    assert (
        "ask acked 2026-01-01T01:00:00.000Z key=20260101T010000000000Z floor request"
    ) in sweep


def test_sweep_horizon_caps_requested_count(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    asks = [
        ("2026-01-01T01:30:00Z", "cap window one"),
        ("2026-01-01T02:30:00Z", "cap window two"),
        ("2026-01-01T03:30:00Z", "cap window three"),
        ("2026-01-01T04:30:00Z", "cap window four"),
        ("2026-01-01T05:30:00Z", "cap current window"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=[
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "2026-01-01T02:00:00Z",
            "2026-01-01T03:00:00Z",
            "2026-01-01T04:00:00Z",
            "2026-01-01T05:00:00Z",
        ],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    sweep = render_sweep([transcript], count=9)

    assert "Sweep\n  windows=5 files=1" in sweep
    assert (
        "horizon_basis=hard_cap start=2026-01-01T01:00:00.000Z compactions=5/9"
    ) in sweep
    assert "Window 4 (from 2026-01-01T05:00:00.000Z)" in sweep
    assert (
        "ask acked 2026-01-01T05:30:00.000Z "
        "key=20260101T053000000000Z cap current window"
    ) in sweep


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


def _write_recovery_transcript(path, *, include_summary: bool) -> None:
    summary = (
        "This session is being continued from a previous conversation that ran "
        "out of context. The summary below covers the earlier portion.\n\n"
        "Summary:\n"
        "1. Primary Request and Intent:\n"
        "   Keep draining allocator-selected tasks.\n\n"
        "   Validate before completion.\n\n"
        "2. Key Technical Concepts:\n"
        "   - briefing recovery\n"
    )
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-recovery"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "prepare recovery"}],
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
            "payload": {"type": "task_complete"},
        },
        {"timestamp": "2026-01-01T00:00:10Z", "type": "compacted", "payload": {}},
    ]
    if include_summary:
        events.extend(
            [
                {
                    "timestamp": "2026-01-01T00:00:11Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{"text": summary}],
                    },
                },
                {
                    "timestamp": "2026-01-01T00:00:12Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{"text": "continue with recovery"}],
                    },
                },
            ]
        )
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


def _record_ack_state_ask(repo, key: str, body: str, disposition: str, ts: str) -> None:
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=key,
                inbox_name=f"{key}.txt",
                text=compose_inbox_text(body=body, priority=None, stop=False),
                disposition=disposition,
            )
        ],
        now=_epoch_seconds(ts),
    )


def _record_ack_state_asks(repo, asks: list[tuple[str, str]]) -> None:
    for ts, body in asks:
        _record_ack_state_ask(
            repo,
            _ack_key(ts),
            body,
            ACK_DISPOSITION_ACKED,
            ts,
        )


def _ack_key(ts: str) -> str:
    return datetime.fromisoformat(ts.replace("Z", "+00:00")).strftime(
        "%Y%m%dT%H%M%S%fZ"
    )


def _epoch_seconds(ts: str) -> float:
    return datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()


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
