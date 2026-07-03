"""Task phase effort window ledger behavior."""

from __future__ import annotations

from datetime import UTC, datetime
import json
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import ServeTeamStore
from spice.tasks import config, create, effort, identity, ops

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A_MEMBER = thread_actor_id(ACTOR_A)
ACTOR_B_MEMBER = thread_actor_id(ACTOR_B)
BASE_TS = datetime(2026, 1, 1, tzinfo=UTC).timestamp()


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_phase_effort_windows_split_real_task_lifecycle_phases(task_repo, monkeypatch):
    clock = {"now": 100.0}
    monkeypatch.setattr("spice.serve.team.metrics.time.time", lambda: clock["now"])
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A_MEMBER])
    _record_identity(
        store,
        ACTOR_A_MEMBER,
        thread_id=ACTOR_A,
        driver="codex",
        model="gpt-5.5",
        effort_value="xhigh",
    )
    handle = create.add(
        "Measure two effort phases",
        project="task.unit",
        flow=["todo", "verify", "review"],
        acceptance=["phase effort windows split phases"],
    )

    clock["now"] = 100.0
    ops.claim(handle)
    clock["now"] = 145.0
    ops.done(handle, validation=["todo complete"])
    clock["now"] = 160.0
    ops.claim(handle)
    clock["now"] = 220.0
    ops.done(handle, validation=["verify complete"])

    row = identity.resolve(handle)
    windows = store.task_phase_effort_windows([row])

    assert [
        (
            window.handle,
            window.title,
            window.phase,
            window.phase_index,
            window.actor_id,
            window.thread_id,
            window.team_id,
            window.driver,
            window.model,
            window.effort,
            window.started_at,
            window.ended_at,
            window.wall_seconds,
            window.partial_markers,
        )
        for window in windows
    ] == [
        (
            handle,
            "Measure two effort phases",
            "todo",
            0,
            ACTOR_A_MEMBER,
            ACTOR_A,
            team.team_id,
            "codex",
            "gpt-5.5",
            "xhigh",
            100.0,
            145.0,
            45.0,
            (),
        ),
        (
            handle,
            "Measure two effort phases",
            "verify",
            1,
            ACTOR_A_MEMBER,
            ACTOR_A,
            team.team_id,
            "codex",
            "gpt-5.5",
            "xhigh",
            160.0,
            220.0,
            60.0,
            (),
        ),
    ]


def test_phase_effort_windows_mark_partial_lifecycle_segments(task_repo):
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A_MEMBER, ACTOR_B_MEMBER])
    _record_identity(
        store,
        ACTOR_A_MEMBER,
        thread_id=ACTOR_A,
        driver="codex",
        model="gpt-5.5",
        effort_value="xhigh",
    )
    _record_identity(
        store,
        ACTOR_B_MEMBER,
        thread_id=ACTOR_B,
        driver="claude",
        model="claude-sonnet",
        effort_value="medium",
    )
    handle = create.add(
        "Mark partial effort phases",
        project="task.unit",
        flow=["todo", "verify", "review"],
        acceptance=["partial effort windows are marked"],
    )
    task_id = identity.uuid_of(identity.resolve(handle))
    store.record_task_lifecycle_event(
        "phaseAdvance",
        task_id=task_id,
        agent_id=ACTOR_A_MEMBER,
        team_id=team.team_id,
        ts=20.0,
    )
    store.record_task_lifecycle_event(
        "claim",
        task_id=task_id,
        agent_id=ACTOR_A_MEMBER,
        team_id=team.team_id,
        ts=30.0,
    )
    store.record_task_lifecycle_event(
        "claim",
        task_id=task_id,
        agent_id=ACTOR_B_MEMBER,
        team_id=team.team_id,
        ts=45.0,
    )

    windows = store.task_phase_effort_windows([identity.resolve(handle)])

    assert [
        (
            window.phase,
            window.actor_id,
            window.driver,
            window.model,
            window.effort,
            window.started_at,
            window.ended_at,
            window.wall_seconds,
            window.partial_markers,
        )
        for window in windows
    ] == [
        (
            "todo",
            ACTOR_A_MEMBER,
            "codex",
            "gpt-5.5",
            "xhigh",
            None,
            20.0,
            None,
            (effort.PARTIAL_MISSING_START,),
        ),
        (
            "verify",
            ACTOR_A_MEMBER,
            "codex",
            "gpt-5.5",
            "xhigh",
            30.0,
            45.0,
            15.0,
            (effort.PARTIAL_HANDOFF,),
        ),
        (
            "verify",
            ACTOR_B_MEMBER,
            "claude",
            "claude-sonnet",
            "medium",
            45.0,
            None,
            None,
            (effort.PARTIAL_MISSING_END,),
        ),
    ]


def test_phase_effort_usage_aggregates_transcript_spend_by_window(tmp_path):
    transcript = tmp_path / "thread-a.jsonl"
    _write_usage_transcript(transcript)
    windows = (
        effort.PhaseEffortWindow(
            task_id="task-1",
            handle="EFFORT-00000001",
            title="Aggregate effort",
            phase="todo",
            phase_index=0,
            actor_id=ACTOR_A_MEMBER,
            thread_id=ACTOR_A,
            team_id="team-a",
            driver="codex",
            model="gpt-5.5",
            effort="xhigh",
            started_at=_epoch(0),
            ended_at=_epoch(20),
        ),
        effort.PhaseEffortWindow(
            task_id="task-1",
            handle="EFFORT-00000001",
            title="Aggregate effort",
            phase="verify",
            phase_index=1,
            actor_id=ACTOR_A_MEMBER,
            thread_id=ACTOR_A,
            team_id="team-a",
            driver="codex",
            model="gpt-5.5",
            effort="xhigh",
            started_at=_epoch(20),
            ended_at=_epoch(40),
        ),
    )

    usage = effort.phase_effort_usage_for_windows(windows, {ACTOR_A: [transcript]})

    assert [
        (
            row.handle,
            row.phase,
            row.phase_index,
            row.driver,
            row.model,
            row.effort,
            row.input_tokens,
            row.cached_input_tokens,
            row.output_tokens,
            row.reasoning_output_tokens,
            row.total_tokens,
            row.turn_count,
            row.message_count,
            row.renewal_count,
            row.wall_seconds,
            row.source_files,
            row.partial_markers,
        )
        for row in usage
    ] == [
        (
            "EFFORT-00000001",
            "todo",
            0,
            "codex",
            "gpt-5.5",
            "xhigh",
            100,
            10,
            20,
            5,
            135,
            1,
            2,
            0,
            20.0,
            (str(transcript),),
            (),
        ),
        (
            "EFFORT-00000001",
            "verify",
            1,
            "codex",
            "gpt-5.5",
            "xhigh",
            207,
            20,
            30,
            10,
            267,
            1,
            2,
            1,
            20.0,
            (str(transcript),),
            (),
        ),
    ]


def test_phase_effort_usage_marks_missing_transcript_and_partial_window():
    window = effort.PhaseEffortWindow(
        task_id="task-2",
        handle="EFFORT-00000002",
        title="Missing transcript",
        phase="todo",
        phase_index=0,
        actor_id=ACTOR_B_MEMBER,
        thread_id=ACTOR_B,
        team_id="team-a",
        driver="claude",
        model="claude-sonnet",
        effort="medium",
        started_at=None,
        ended_at=_epoch(20),
        partial_markers=(effort.PARTIAL_MISSING_START,),
    )

    usage = effort.phase_effort_usage_for_windows((window,), {})

    assert len(usage) == 1
    assert usage[0].partial
    assert usage[0].partial_markers == (
        effort.PARTIAL_MISSING_START,
        effort.PARTIAL_MISSING_TRANSCRIPT,
    )
    assert usage[0].source_files == ()
    assert usage[0].total_tokens == 0
    assert usage[0].turn_count == 0
    assert usage[0].wall_seconds is None


def test_phase_model_cost_rows_keep_tuning_comparisons_model_tagged():
    rows = effort.phase_model_cost_rows(_phase_model_cost_usage_rows())

    assert _phase_model_cost_row_tuples(rows) == _expected_phase_model_cost_rows()
    assert _phase_model_cost_group_tuples(rows) == [
        ("claude", "claude-sonnet", "medium", [("verify", 70)]),
        ("codex", "gpt-5.5", "xhigh", [("todo", 150), ("verify", 200)]),
    ]


def _record_identity(
    store: ServeTeamStore,
    actor_id: str,
    *,
    thread_id: str,
    driver: str,
    model: str,
    effort_value: str,
) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id="wt-a",
        thread_id=thread_id,
        actual_driver=driver,
        actual_model=model,
        actual_effort=effort_value,
        desired_driver=driver,
        desired_model=model,
        desired_effort=effort_value,
        transcript_owner=driver,
    )


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)


def _phase_model_cost_usage_rows() -> tuple[effort.PhaseEffortUsage, ...]:
    return (
        _usage(
            task_id="task-1",
            phase="todo",
            phase_index=0,
            driver="codex",
            model="gpt-5.5",
            effort_value="xhigh",
            total_tokens=100,
            input_tokens=80,
            wall_seconds=10.0,
            turn_count=1,
        ),
        _usage(
            task_id="task-2",
            phase="todo",
            phase_index=0,
            driver="codex",
            model="gpt-5.5",
            effort_value="xhigh",
            total_tokens=50,
            input_tokens=40,
            wall_seconds=5.0,
            turn_count=1,
            partial_markers=(effort.PARTIAL_MISSING_END,),
        ),
        _usage(
            task_id="task-1",
            phase="verify",
            phase_index=1,
            driver="codex",
            model="gpt-5.5",
            effort_value="xhigh",
            total_tokens=200,
            input_tokens=170,
            wall_seconds=20.0,
            turn_count=2,
            renewal_count=1,
        ),
        _usage(
            task_id="task-3",
            phase="verify",
            phase_index=1,
            driver="claude",
            model="claude-sonnet",
            effort_value="medium",
            total_tokens=70,
            input_tokens=60,
            wall_seconds=7.0,
            turn_count=1,
        ),
        _usage(
            task_id="task-4",
            phase="todo",
            phase_index=0,
            driver="codex",
            model="",
            effort_value="xhigh",
            total_tokens=999,
            input_tokens=900,
            wall_seconds=99.0,
            turn_count=9,
        ),
    )


def _phase_model_cost_row_tuples(
    rows: tuple[effort.PhaseModelCostRow, ...],
) -> list[tuple]:
    return [
        (
            row.phase,
            row.phase_index,
            row.driver,
            row.model,
            row.effort,
            row.task_count,
            row.window_count,
            row.total_tokens,
            row.input_tokens,
            row.turn_count,
            row.renewal_count,
            row.wall_seconds,
            row.partial_count,
            row.partial_markers,
        )
        for row in rows
    ]


def _expected_phase_model_cost_rows() -> list[tuple]:
    return [
        (
            "verify",
            1,
            "claude",
            "claude-sonnet",
            "medium",
            1,
            1,
            70,
            60,
            1,
            0,
            7.0,
            0,
            (),
        ),
        (
            "todo",
            0,
            "codex",
            "gpt-5.5",
            "xhigh",
            2,
            2,
            150,
            120,
            2,
            0,
            15.0,
            1,
            (effort.PARTIAL_MISSING_END,),
        ),
        (
            "verify",
            1,
            "codex",
            "gpt-5.5",
            "xhigh",
            1,
            1,
            200,
            170,
            2,
            1,
            20.0,
            0,
            (),
        ),
    ]


def _phase_model_cost_group_tuples(
    rows: tuple[effort.PhaseModelCostRow, ...],
) -> list[tuple[str, str, str, list[tuple[str, int]]]]:
    return [
        (
            group.driver,
            group.model,
            group.effort,
            [(row.phase, row.total_tokens) for row in group.rows],
        )
        for group in effort.phase_model_cost_groups(rows)
    ]


def _usage(
    *,
    task_id: str,
    phase: str,
    phase_index: int,
    driver: str,
    model: str,
    effort_value: str,
    total_tokens: int,
    input_tokens: int,
    wall_seconds: float,
    turn_count: int,
    renewal_count: int = 0,
    partial_markers: tuple[str, ...] = (),
) -> effort.PhaseEffortUsage:
    window = effort.PhaseEffortWindow(
        task_id=task_id,
        handle=task_id,
        title=task_id,
        phase=phase,
        phase_index=phase_index,
        actor_id=ACTOR_A_MEMBER,
        thread_id=ACTOR_A,
        team_id="team-a",
        driver=driver,
        model=model,
        effort=effort_value,
        started_at=0.0,
        ended_at=wall_seconds,
    )
    return effort.PhaseEffortUsage(
        window=window,
        source_files=("thread.jsonl",),
        input_tokens=input_tokens,
        total_tokens=total_tokens,
        turn_count=turn_count,
        renewal_count=renewal_count,
        partial_markers=partial_markers,
    )


def _write_usage_transcript(path: Path) -> None:
    events = [
        _event(1, "event_msg", {"type": "task_started", "turn_id": "turn-todo"}),
        _message(2, "user", "todo work"),
        _message(3, "assistant", "todo done"),
        _token_count(
            4,
            input_tokens=100,
            cached_input_tokens=10,
            output_tokens=20,
            reasoning_output_tokens=5,
            total_tokens=135,
        ),
        _event(5, "event_msg", {"type": "task_complete"}),
        _token_count(
            20,
            input_tokens=7,
            cached_input_tokens=0,
            output_tokens=0,
            reasoning_output_tokens=0,
            total_tokens=7,
        ),
        _event(21, "event_msg", {"type": "task_started", "turn_id": "turn-verify"}),
        _message(22, "user", "verify work"),
        _message(23, "assistant", "verify done"),
        _token_count(
            24,
            input_tokens=200,
            cached_input_tokens=20,
            output_tokens=30,
            reasoning_output_tokens=10,
            total_tokens=260,
        ),
        _event(25, "event_msg", {"type": "task_complete"}),
        _event(30, "compacted", {}),
    ]
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )


def _event(second: int, event_type: str, payload: dict) -> dict[str, object]:
    return {"timestamp": _ts(second), "type": event_type, "payload": payload}


def _message(second: int, role: str, text: str) -> dict[str, object]:
    return _event(
        second,
        "response_item",
        {
            "type": "message",
            "role": role,
            "content": [{"type": "output_text", "text": text}],
        },
    )


def _token_count(
    second: int,
    *,
    input_tokens: int,
    cached_input_tokens: int,
    output_tokens: int,
    reasoning_output_tokens: int,
    total_tokens: int,
) -> dict[str, object]:
    return _event(
        second,
        "event_msg",
        {
            "type": "token_count",
            "info": {
                "last_token_usage": {
                    "input_tokens": input_tokens,
                    "cached_input_tokens": cached_input_tokens,
                    "output_tokens": output_tokens,
                    "reasoning_output_tokens": reasoning_output_tokens,
                    "total_tokens": total_tokens,
                },
                "total_token_usage": {"total_tokens": total_tokens},
                "model_context_window": 200_000,
            },
        },
    )


def _ts(second: int) -> str:
    return f"2026-01-01T00:00:{second:02d}.000Z"


def _epoch(second: int) -> float:
    return BASE_TS + second
