"""Completed-task sizing report signals."""

from __future__ import annotations

from spice.cli.parser import build_parser
from spice.tasks import sizing


def test_task_sizing_cli_parser_accepts_limit():
    args = build_parser().parse_args(["task", "sizing", "--limit", "5"])

    assert args.task_action == "sizing"
    assert args.limit == 5


def test_task_sizing_scores_elapsed_events_and_flow_shape():
    row = _row(
        "Event sized task",
        uuid="task-1",
        phase_0="todo",
        phase_1="verify",
        phase_2="review",
    )
    events = (
        sizing.TaskLifecycleEvent("claim", 0),
        sizing.TaskLifecycleEvent("phaseAdvance", 1_200),
        sizing.TaskLifecycleEvent("claim", 1_800),
        sizing.TaskLifecycleEvent("review", 2_400),
    )

    assessment = sizing.assess_task_size(row, events)
    components = _components(assessment)

    assert assessment.label == "M"
    assert assessment.score == 2
    assert components["elapsed"] == sizing.SizingComponent(
        "elapsed", 1, "task_events:1800s"
    )
    assert components["flow"] == sizing.SizingComponent("flow", 1, "phase:verify")


def test_task_sizing_validation_uses_structured_signal_absence():
    row = _row(
        "Former validation prose false positive",
        uuid="task-2",
        validation="Full browser suite deliberately not run; focused unit only.",
        acceptance="Do not require browser or full-suite validation here.",
    )
    events = (
        sizing.TaskLifecycleEvent("claim", 0),
        sizing.TaskLifecycleEvent("complete", 60),
    )

    assessment = sizing.assess_task_size(row, events)
    components = _components(assessment)

    assert assessment.label == "S"
    assert assessment.score == 0
    assert components["validation"] == sizing.SizingComponent(
        "validation", 0, "no_structured_validation_signal"
    )


def test_task_sizing_report_renders_raw_components():
    row = _row(
        "Rendered sizing task",
        uuid="task-3",
        incepted="20260626T061545678415Z",
        phase_0="todo",
        phase_1="verify",
        phase_2="review",
    )
    events = {
        "task-3": (
            sizing.TaskLifecycleEvent("claim", 0),
            sizing.TaskLifecycleEvent("phaseAdvance", 1_200),
            sizing.TaskLifecycleEvent("claim", 1_800),
            sizing.TaskLifecycleEvent("review", 2_400),
        )
    }

    report = sizing.render_sizing_report(rows=[row], events_by_task=events)

    assert report.startswith("UNIT-20260626T061545678415Z size=M score=2 ")
    assert "elapsed=+1(task_events:1800s)" in report
    assert "validation=+0(no_structured_validation_signal)" in report
    assert "flow=+1(phase:verify)" in report


def _components(
    assessment: sizing.TaskSizeAssessment,
) -> dict[str, sizing.SizingComponent]:
    return {component.name: component for component in assessment.components}


def _row(
    title: str,
    *,
    uuid: str,
    incepted: str = "20260626T061545678415Z",
    validation: str = "",
    acceptance: str = "",
    phase_0: str = "todo",
    phase_1: str = "",
    phase_2: str = "",
) -> dict[str, object]:
    return {
        "uuid": uuid,
        "description": title,
        "project": "task.unit",
        "status": "completed",
        "phase": phase_2 or phase_1 or phase_0,
        "phase_i": 0,
        "phase_0": phase_0,
        "phase_1": phase_1,
        "phase_2": phase_2,
        "priority": "M",
        "incepted": incepted,
        "entry": "20260626T060000000000Z",
        "end": "20260626T060100000000Z",
        "review_finding": "clean",
        "validation": validation,
        "acceptance": acceptance,
    }
