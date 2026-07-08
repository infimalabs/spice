"""Session briefing rendering horizon, recency, and dedup tests."""

import json
import time

from spice.mail.ackstate import (
    ACK_DISPOSITION_REFUSED,
    AckStateWrite,
    record_acked_inbox_items,
)
from spice.mail.inbox import compose_inbox_text
from spice.sessions import briefing as briefing_module
from spice.sessions.briefing import render_briefing, render_sweep
from tests.test_sessionbriefing import (
    _init_git_repo,
    _record_ack_state_asks,
    _section_lines,
    _write_horizon_transcript,
)


def test_briefing_deduplicates_identical_asks_and_finals(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "dedupe.jsonl"
    events = []
    for index, timestamp in enumerate(["2026-01-01T00:00:00Z", "2026-01-01T00:05:00Z"]):
        turn_id = f"turn-{index}"
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
                        "content": [{"text": "repeat request"}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "phase": "final_answer",
                        "content": [{"text": "repeat final"}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "event_msg",
                    "payload": {"type": "task_complete"},
                },
            ]
        )
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert briefing.count("repeat request") == 1
    assert briefing.count("repeat final") == 1
    assert _section_lines(briefing, "Latest Ask") == [
        "Latest Ask",
        "  human 2026-01-01T00:05:00.000Z repeat_count=2 repeat request",
    ]
    assert _section_lines(briefing, "Latest Final") == [
        "Latest Final",
        "  repeat_count=2 repeat final",
    ]


def test_briefing_skill_mantra_leaves_latest_ask_empty(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "skill-mantra.jsonl"
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "turn-skill"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "[$spice](.agents/skills/spice/SKILL.md)"}],
            },
        },
        {
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert _section_lines(briefing, "Latest Ask") == ["Latest Ask", "  -"]


def test_briefing_renders_only_recent_consumed_ack_state_trailers(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    stale_key = "20260704T054409667615Z"
    recent_key = "20260708T054409667615Z"
    stale_name = f"{stale_key}.txt"
    recent_name = f"{recent_key}.txt"
    stale_age = briefing_module.DEFAULT_RECENCY_MAX_SECONDS + 60
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=stale_key,
                inbox_name=stale_name,
                text=compose_inbox_text(
                    body="stale refusal", priority=None, stop=False
                ),
                disposition=ACK_DISPOSITION_REFUSED,
            )
        ],
        now=time.time() - stale_age,
    )
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=recent_key,
                inbox_name=recent_name,
                text=compose_inbox_text(
                    body="recent refusal", priority=None, stop=False
                ),
                disposition=ACK_DISPOSITION_REFUSED,
            )
        ],
        now=time.time(),
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing(
        [],
        end="2026-07-08T06:00:00.000Z",
        max_lines=200,
        max_bytes=20000,
    )

    inbox = _section_lines(briefing, "Inbox")
    assert inbox[:4] == [
        "Inbox",
        "  pending=0",
        "  refused=1",
        "  source=ack_state; status=already_consumed_operator_steering; store=sqlite",
    ]
    assert len(inbox) == 5
    assert inbox[4].startswith("  refused_inbox key=20260708T054409667615Z age=")
    assert inbox[4].endswith(" text=recent refusal")
    assert _section_lines(briefing, "Latest Ask") == [
        "Latest Ask",
        "  refused 2026-07-08T05:44:09.667Z key=20260708T054409667615Z recent refusal",
    ]


def test_briefing_renders_recent_asks_and_finals_inside_recency_floor(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "recency.jsonl"
    _write_horizon_transcript(
        transcript,
        asks=[
            ("2026-01-01T00:00:00Z", "old request"),
            ("2026-01-01T05:00:00Z", "recent request"),
        ],
        compactions=[],
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert "files=recency.jsonl turns=1" in briefing
    assert _section_lines(briefing, "Latest Ask") == [
        "Latest Ask",
        "  human 2026-01-01T05:00:00.000Z recent request",
    ]
    assert _section_lines(briefing, "Latest Final") == [
        "Latest Final",
        "  completed recent request",
    ]


def test_briefing_default_horizon_is_count_bound(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    asks = [
        ("2026-01-01T15:00:00Z", "before count horizon"),
        ("2026-01-01T16:30:00Z", "inside first count window"),
        ("2026-01-01T17:30:00Z", "inside second count window"),
        ("2026-01-01T18:30:00Z", "inside current count window"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=[
            "2026-01-01T16:00:00Z",
            "2026-01-01T17:00:00Z",
            "2026-01-01T18:00:00Z",
        ],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert "files=horizon.jsonl turns=3" in briefing
    assert (
        "horizon_basis=compaction_count start=2026-01-01T16:00:00.000Z compactions=3/3"
    ) in briefing
    assert _section_lines(briefing, "Latest Ask") == [
        "Latest Ask",
        "  acked 2026-01-01T18:30:00.000Z "
        "key=20260101T183000000000Z inside current count window",
    ]


def test_briefing_default_horizon_starts_at_oldest_selected_window(
    tmp_path, monkeypatch
):
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

    assert "files=horizon.jsonl turns=1" in briefing
    assert (
        "horizon_basis=compaction_count start=2026-01-01T10:00:00.000Z compactions=2/3"
    ) in briefing
    assert _section_lines(briefing, "Latest Ask") == [
        "Latest Ask",
        "  acked 2026-01-01T10:30:00.000Z "
        "key=20260101T103000000000Z young current request",
    ]


def test_briefing_parses_only_the_selected_compaction_tail(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "tail.jsonl"
    events = [
        {
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "old-turn"},
        },
        {
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"text": "<unknown-scaffold>old harness</unknown-scaffold>"}
                ],
            },
        },
        {"timestamp": "2026-01-01T01:00:00Z", "type": "compacted", "payload": {}},
        {"timestamp": "2026-01-01T02:00:00Z", "type": "compacted", "payload": {}},
        {
            "timestamp": "2026-01-01T02:10:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "tail-a"},
        },
        {
            "timestamp": "2026-01-01T02:10:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "tail request one"}],
            },
        },
        {"timestamp": "2026-01-01T03:00:00Z", "type": "compacted", "payload": {}},
        {
            "timestamp": "2026-01-01T03:10:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "tail-b"},
        },
        {
            "timestamp": "2026-01-01T03:10:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "tail request two"}],
            },
        },
        {"timestamp": "2026-01-01T04:00:00Z", "type": "compacted", "payload": {}},
        {
            "timestamp": "2026-01-01T04:10:00Z",
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "tail-c"},
        },
        {
            "timestamp": "2026-01-01T04:10:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"text": "tail request three"}],
            },
        },
    ]
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=200, max_bytes=20000)

    assert "files=tail.jsonl turns=3" in briefing
    assert (
        "horizon_basis=compaction_count start=2026-01-01T02:00:00.000Z compactions=3/3"
    ) in briefing
    assert _section_lines(briefing, "Latest Ask") == [
        "Latest Ask",
        "  human 2026-01-01T04:10:00.000Z tail request three",
    ]


def test_explicit_start_wins_over_adaptive_horizon_in_briefing_and_sweep(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "horizon.jsonl"
    asks = [
        ("2026-01-01T15:00:00Z", "operator explicit start request"),
        ("2026-01-01T16:30:00Z", "inside first count window"),
        ("2026-01-01T17:30:00Z", "inside second count window"),
        ("2026-01-01T18:30:00Z", "inside current count window"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=[
            "2026-01-01T16:00:00Z",
            "2026-01-01T17:00:00Z",
            "2026-01-01T18:00:00Z",
        ],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    briefing = render_briefing(
        [transcript],
        start="2026-01-01T15:00:00.000Z",
        max_lines=200,
        max_bytes=20000,
    )
    sweep = render_sweep(
        [transcript],
        count=3,
        start="2026-01-01T15:00:00.000Z",
    )

    assert "files=horizon.jsonl turns=4" in briefing
    assert "Filters\n  start=2026-01-01T15:00:00.000Z" in briefing
    assert "Window 0 (from 2026-01-01T15:00:00.000Z)" in sweep
    assert (
        "ask acked 2026-01-01T15:00:00.000Z "
        "key=20260101T150000000000Z operator explicit start request"
    ) in sweep


def test_sweep_uses_last_requested_compaction_windows(tmp_path, monkeypatch):
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

    assert "Sweep\n  windows=3 files=1" in sweep
    assert (
        "horizon_basis=compaction_count start=2026-01-01T10:00:00.000Z compactions=3/3"
    ) in sweep
    assert "Window 0 (from 2026-01-01T10:00:00.000Z)" in sweep
    assert (
        "ask acked 2026-01-01T10:05:00.000Z "
        "key=20260101T100500000000Z recent request one"
    ) in sweep


def test_sweep_deduplicates_and_interleaves_window_rows(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "sweep-dedupe.jsonl"
    events = [{"timestamp": "2026-01-01T00:00:00Z", "type": "compacted", "payload": {}}]
    rows = [
        ("2026-01-01T00:01:00Z", "turn-a", "repeat request", "first final"),
        ("2026-01-01T00:02:00Z", "turn-b", "repeat request", "second final"),
        ("2026-01-01T00:03:00Z", "turn-c", "distinct request", "third final"),
    ]
    for timestamp, turn_id, ask, final in rows:
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
                        "content": [{"text": ask}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "phase": "final_answer",
                        "content": [{"text": final}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "event_msg",
                    "payload": {"type": "task_complete"},
                },
            ]
        )
    transcript.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )
    monkeypatch.chdir(repo)

    sweep = render_sweep([transcript], count=1)
    lines = sweep.splitlines()
    row_lines = [line for line in lines if line.startswith(("  ask ", "  final "))]

    assert sum("repeat request" in line for line in row_lines) == 1
    assert row_lines == [
        "  ask human 2026-01-01T00:03:00.000Z distinct request",
        "  final 2026-01-01T00:03:00.000Z third final",
        "  ask human 2026-01-01T00:02:00.000Z repeat_count=2 repeat request",
    ]


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
