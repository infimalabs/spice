"""Completed-task sizing report evidence."""

from __future__ import annotations

from spice.cli.parser import build_parser
from spice.tasks import effort, sizing


def test_task_sizing_cli_parser_accepts_limit_and_project():
    args = build_parser().parse_args(
        ["task", "sizing", "--project", "task.metrics", "--limit", "5"]
    )

    assert args.task_action == "sizing"
    assert args.project == "task.metrics"
    assert args.limit == 5


def test_task_sizing_scores_complete_phase_effort_review_and_metadata():
    row = _completed_row(
        title="Effort sized task",
        uuid="task-1",
        flow=("todo", "verify", "review"),
    )
    windows = (
        _window("task-1", phase="todo", phase_index=0, start=0, end=1_200),
        _window("task-1", phase="verify", phase_index=1, start=1_800, end=2_400),
    )

    report = sizing.size_completed_task(row, windows=windows)
    components = _components(report)

    assert report.label == "M"
    assert report.score == 2
    assert components["elapsed"] == sizing.SizingComponent(
        "elapsed", 1, "phase_effort_windows:1800s"
    )
    assert components["review"] == sizing.SizingComponent(
        "review", 0, "review_finding:clean"
    )
    assert components["metadata"] == sizing.SizingComponent(
        "metadata", 1, "phase:verify"
    )


def test_task_sizing_keeps_completion_validation_as_unscored_evidence():
    row = _completed_row(
        title="Former validation prose false positive",
        uuid="task-2",
        validation="Full browser suite deliberately not run; focused unit only.",
        acceptance="Do not require browser or full-suite validation here.",
    )
    windows = (_window("task-2", start=0, end=60),)

    report = sizing.size_completed_task(row, windows=windows)

    assert report.label == "S"
    assert report.score == 0
    assert tuple(component.name for component in report.components) == (
        "elapsed",
        "review",
        "metadata",
    )
    assert report.evidence == (
        sizing.SizingEvidence("validation", "recorded", "completion_validation"),
    )


def test_task_sizing_distinguishes_unavailable_evidence_from_measured_zero():
    unavailable_row = _completed_row(
        title="Missing evidence",
        uuid="task-missing",
        validation="",
        review_finding="",
    )
    measured_row = _completed_row(
        title="Measured zero",
        uuid="task-zero",
        validation="",
        flow=("todo",),
        review_finding="",
    )

    unavailable = sizing.size_completed_task(unavailable_row)
    measured = sizing.size_completed_task(
        measured_row,
        windows=(_window("task-zero", start=10, end=10),),
    )

    assert unavailable.label is None
    assert unavailable.score is None
    assert _components(unavailable)["elapsed"] == sizing.SizingComponent(
        "elapsed", None, "no_phase_effort_windows"
    )
    assert _components(unavailable)["review"] == sizing.SizingComponent(
        "review", None, "no_review_finding"
    )
    assert measured.label == "S"
    assert measured.score == 0
    assert _components(measured)["elapsed"] == sizing.SizingComponent(
        "elapsed", 0, "phase_effort_windows:0s"
    )
    assert _components(measured)["review"] == sizing.SizingComponent(
        "review", 0, "phase:not_required"
    )
    assert sizing.render_task_sizing(unavailable).startswith(
        "UNIT-20260626T061545678415Z size=unavailable size_score=unavailable"
    )
    assert "elapsed=+0(phase_effort_windows:0s)" in sizing.render_task_sizing(measured)


def test_task_sizing_marks_incomplete_phase_effort_unavailable():
    row = _completed_row(title="Incomplete effort", uuid="task-incomplete")
    windows = (
        _window(
            "task-incomplete",
            start=20,
            end=None,
        ),
    )

    report = sizing.size_completed_task(row, windows=windows)

    assert report.score is None
    assert _components(report)["elapsed"] == sizing.SizingComponent(
        "elapsed", None, "incomplete_phase_effort_window"
    )


def test_task_sizing_marks_partial_handoff_unavailable_even_with_timestamps():
    row = _completed_row(title="Handoff effort", uuid="task-handoff")
    windows = (
        _window(
            "task-handoff",
            start=20,
            end=80,
            markers=(effort.PARTIAL_HANDOFF,),
        ),
    )

    report = sizing.size_completed_task(row, windows=windows)

    assert report.label is None
    assert report.score is None
    assert _components(report)["elapsed"] == sizing.SizingComponent(
        "elapsed", None, "partial_phase_effort_window"
    )


def test_task_sizing_rows_filter_and_render_raw_evidence():
    row = _completed_row(
        title="Rendered sizing task",
        uuid="task-3",
        incepted="20260626T061545678415Z",
        project="task.metrics",
        flow=("todo", "verify", "review"),
        validation="focused sizing tests passed",
    )
    windows = {
        "task-3": (
            _window("task-3", phase="todo", phase_index=0, start=0, end=1_200),
            _window("task-3", phase="verify", phase_index=1, start=1_800, end=2_400),
        )
    }

    reports = sizing.completed_task_sizing_rows(
        project="task", rows=[row], effort_windows_by_task=windows
    )
    output = sizing.render_task_sizing(reports[0])

    assert len(reports) == 1
    assert output.startswith(
        "METRICS-20260626T061545678415Z size=M size_score=2 project=task.metrics "
    )
    assert "elapsed=+1(phase_effort_windows:1800s)" in output
    assert "review=+0(review_finding:clean)" in output
    assert "metadata=+1(phase:verify)" in output
    assert "validation=recorded(completion_validation)" in output


def test_task_sizing_cli_renders_completed_rows(monkeypatch, capsys):
    row = _completed_row(
        title="Newest task",
        uuid="task-cli",
        project="task.metrics",
        incepted="20260626T060000000002Z",
        end="20260626T061000Z",
        validation="focused sizing tests passed",
    )
    monkeypatch.setattr(sizing.tw, "export", lambda filters: [row])
    monkeypatch.setattr(
        sizing,
        "_effort_windows_by_task",
        lambda _rows: {"task-cli": (_window("task-cli", start=0, end=60),)},
    )

    args = build_parser().parse_args(
        ["task", "sizing", "--project", "task", "--limit", "1"]
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "METRICS-20260626T060000000002Z" in output
    assert "size_score=0" in output
    assert "validation=recorded(completion_validation)" in output


def _components(report: sizing.TaskSizing) -> dict[str, sizing.SizingComponent]:
    return {component.name: component for component in report.components}


def _window(
    task_id: str,
    *,
    phase: str = "todo",
    phase_index: int = 0,
    start: float | None,
    end: float | None,
    markers: tuple[str, ...] = (),
) -> effort.PhaseEffortWindow:
    return effort.PhaseEffortWindow(
        task_id=task_id,
        handle="UNIT-1kTest",
        title="Sizing fixture",
        phase=phase,
        phase_index=phase_index,
        actor_id="agent-a",
        thread_id="thread-a",
        team_id="team-a",
        driver="codex",
        model="gpt",
        effort="high",
        started_at=start,
        ended_at=end,
        partial_markers=markers,
    )


def _completed_row(
    *,
    title: str,
    uuid: str | None = None,
    project: str = "task.unit",
    incepted: str = "20260626T061545678415Z",
    entry: str = "20260626T060000Z",
    end: str = "20260626T060100Z",
    validation: str = "focused tests passed",
    review_finding: str = "clean",
    depends: list[str] | None = None,
    flow: tuple[str, ...] = ("todo", "review"),
    acceptance: str = "",
) -> dict[str, object]:
    row: dict[str, object] = {
        "uuid": uuid or f"uuid-{incepted}",
        "incepted": incepted,
        "description": title,
        "project": project,
        "status": "completed",
        "entry": entry,
        "end": end,
        "validation": validation,
        "review_finding": review_finding,
        "depends": depends or [],
        "acceptance": acceptance,
    }
    for index, phase in enumerate(flow):
        row[f"phase_{index}"] = phase
    return row
