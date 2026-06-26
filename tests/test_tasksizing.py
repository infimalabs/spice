"""Completed-task sizing report signals."""

from __future__ import annotations

from spice.cli.parser import build_parser
from spice.tasks import sizing


def test_task_sizing_cli_parser_accepts_limit_and_project():
    args = build_parser().parse_args(
        ["task", "sizing", "--project", "task.metrics", "--limit", "5"]
    )

    assert args.task_action == "sizing"
    assert args.project == "task.metrics"
    assert args.limit == 5


def test_task_sizing_scores_elapsed_events_and_metadata_shape():
    row = _completed_row(
        title="Event sized task",
        uuid="task-1",
        flow=("todo", "verify", "review"),
    )
    events = (
        sizing.TaskLifecycleEvent("claim", 0),
        sizing.TaskLifecycleEvent("phaseAdvance", 1_200),
        sizing.TaskLifecycleEvent("claim", 1_800),
        sizing.TaskLifecycleEvent("review", 2_400),
    )

    report = sizing.size_completed_task(row, events=events)
    components = _components(report)

    assert report.label == "M"
    assert report.score == 2
    assert components["elapsed"] == sizing.SizingComponent(
        "elapsed", 1, "task_events:1800s"
    )
    assert components["metadata"] == sizing.SizingComponent(
        "metadata", 1, "phase:verify"
    )


def test_task_sizing_validation_uses_structured_signal_absence():
    row = _completed_row(
        title="Former validation prose false positive",
        uuid="task-2",
        validation="Full browser suite deliberately not run; focused unit only.",
        acceptance="Do not require browser or full-suite validation here.",
    )
    events = (
        sizing.TaskLifecycleEvent("claim", 0),
        sizing.TaskLifecycleEvent("complete", 60),
    )

    report = sizing.size_completed_task(row, events=events)
    components = _components(report)

    assert report.label == "S"
    assert report.score == 0
    assert components["validation"] == sizing.SizingComponent(
        "validation", 0, "no_structured_validation_signal"
    )


def test_task_sizing_rows_filter_and_render_raw_components():
    row = _completed_row(
        title="Rendered sizing task",
        uuid="task-3",
        incepted="20260626T061545678415Z",
        project="task.metrics",
        flow=("todo", "verify", "review"),
    )
    events = {
        "task-3": (
            sizing.TaskLifecycleEvent("claim", 0),
            sizing.TaskLifecycleEvent("phaseAdvance", 1_200),
            sizing.TaskLifecycleEvent("claim", 1_800),
            sizing.TaskLifecycleEvent("review", 2_400),
        )
    }

    reports = sizing.completed_task_sizing_rows(
        project="task", rows=[row], events_by_task=events
    )
    output = sizing.render_task_sizing(reports[0])

    assert len(reports) == 1
    assert output.startswith(
        "METRICS-20260626T061545678415Z size=M size_score=2 project=task.metrics "
    )
    assert "elapsed=+1(task_events:1800s)" in output
    assert "validation=+0(no_structured_validation_signal)" in output
    assert "metadata=+1(phase:verify)" in output


def test_task_sizing_cli_renders_completed_rows(monkeypatch, capsys):
    row = _completed_row(
        title="Newest task",
        project="task.metrics",
        incepted="20260626T060000000002Z",
        end="20260626T061000Z",
    )
    monkeypatch.setattr(sizing.tw, "export", lambda filters: [row])
    monkeypatch.setattr(sizing, "_events_by_task_id", lambda _ids: {})

    args = build_parser().parse_args(
        ["task", "sizing", "--project", "task", "--limit", "1"]
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "METRICS-20260626T060000000002Z" in output
    assert "size_score=" in output
    assert "validation=+0(no_structured_validation_signal)" in output


def _components(report: sizing.TaskSizing) -> dict[str, sizing.SizingComponent]:
    return {component.name: component for component in report.components}


def _completed_row(
    *,
    title: str,
    uuid: str | None = None,
    project: str = "task.unit",
    incepted: str = "20260626T061545678415Z",
    entry: str = "20260626T060000Z",
    end: str = "20260626T060100Z",
    validation: str = "",
    review_finding: str = "clean",
    tags: list[str] | None = None,
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
        "tags": tags or [],
        "depends": depends or [],
        "acceptance": acceptance,
    }
    for index, phase in enumerate(flow):
        row[f"phase_{index}"] = phase
    return row
