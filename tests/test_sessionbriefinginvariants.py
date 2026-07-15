"""Invariant tests for the supervised briefing redesign."""

from __future__ import annotations

import json
import os
import time


from spice.sessions import records
from spice.sessions.briefing import render_briefing, render_sweep
from tests.test_sessionbriefing import (
    _init_git_repo,
    _record_ack_state_asks,
    _section_lines,
    _write_horizon_transcript,
)
from tests.test_sessionfixtures import (
    SUPERVISED_FIXTURES,
    transcript_driver_for_fixture,
)


SKILL_MARKERS = (
    "[$spice](.agents/skills/spice/SKILL.md)",
    "The linked skill below carries the full authority",
)


def test_supervised_fixture_outputs_exclude_skill_mantra_text(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    for transcript in SUPERVISED_FIXTURES:
        with transcript_driver_for_fixture(monkeypatch, transcript):
            outputs = [
                render_briefing([transcript], max_lines=400, max_bytes=40000),
                render_sweep([transcript], count=3),
            ]

        assert [
            sum(output.count(marker) for marker in SKILL_MARKERS) for output in outputs
        ] == [0, 0]


def test_supervised_fixture_briefing_renders_unique_lines(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    for transcript in SUPERVISED_FIXTURES:
        with transcript_driver_for_fixture(monkeypatch, transcript):
            briefing = render_briefing([transcript], max_lines=400, max_bytes=40000)

        lines = briefing.splitlines()
        assert len(lines) == len(dict.fromkeys(lines))


def test_supervised_fixture_work_windows_render_substantive_sweep_rows(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    for transcript in SUPERVISED_FIXTURES:
        with transcript_driver_for_fixture(monkeypatch, transcript):
            sweep = render_sweep([transcript], count=3)

        windows = _sweep_windows(sweep)
        assert [
            any(row.startswith(("  ask ", "  final ", "  trajectory ")) for row in rows)
            for rows in windows.values()
        ] == [True] * len(windows)
        assert (
            sum(
                "(no dialogue in this window)" in row
                for rows in windows.values()
                for row in rows
            )
            == 0
        )


def test_unknown_tag_fragment_remains_human_input():
    shape = records.classify_user_message("<system-reminder>new harness block")

    assert shape is records.MessageShape.HUMAN


def test_window_bound_and_recency_cap_evict_stale_rows(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "window-bound.jsonl"
    asks = [
        ("2026-01-01T10:00:00Z", "stale request"),
        ("2026-01-01T18:30:00Z", "current request"),
    ]
    _write_horizon_transcript(
        transcript,
        asks=asks,
        compactions=["2026-01-01T18:00:00Z"],
    )
    _record_ack_state_asks(repo, asks)
    monkeypatch.chdir(repo)

    briefing = render_briefing([transcript], max_lines=400, max_bytes=40000)

    assert _section_lines(briefing, "Steering") == [
        "Steering",
        "  2026-01-01T18:30:00.000Z disposition=acked "
        "key=20260101T183000000000Z text=current request",
    ]


def test_large_multi_window_briefing_stays_inside_wall_clock_budget(
    tmp_path, monkeypatch
):
    repo = _init_git_repo(tmp_path / "repo")
    transcript = tmp_path / "large.jsonl"
    _write_large_window_transcript(transcript, count=40)
    monkeypatch.chdir(repo)

    start = time.perf_counter()
    briefing = render_briefing([transcript], max_lines=400, max_bytes=40000)
    elapsed = time.perf_counter() - start

    assert briefing.splitlines()[0] == "Briefing"
    budget = 3.0 if os.environ.get("PYTEST_XDIST_WORKER") else 2.0  # env-policy: allow
    assert elapsed < budget


def _sweep_windows(sweep: str) -> dict[str, list[str]]:
    windows: dict[str, list[str]] = {}
    current: str | None = None
    for line in sweep.splitlines():
        if line.startswith("Window "):
            current = line
            windows[current] = []
        elif current is not None:
            windows[current].append(line)
    return windows


def _write_large_window_transcript(path, *, count: int) -> None:
    events: list[dict[str, object]] = []
    for index in range(count):
        minute = index % 60
        turn_id = f"large-{index}"
        timestamp = f"2026-01-01T00:{minute:02d}:00Z"
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
                        "content": [{"text": f"large request {index}"}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "phase": "final_answer",
                        "content": [{"text": f"large final {index}"}],
                    },
                },
                {
                    "timestamp": timestamp,
                    "type": "event_msg",
                    "payload": {"type": "task_complete"},
                },
                {"timestamp": timestamp, "type": "compacted", "payload": {}},
            ]
        )
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )
