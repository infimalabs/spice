"""Completed-task sizing report."""

from __future__ import annotations

from spice.cli.parser import build_parser
from spice.tasks import sizing


def test_completed_task_sizing_labels_small_and_xl_rows():
    small = _completed_row(
        title="Small docs fix",
        project="docs.study",
        claim_at="2026-06-26T06:00:00.000000Z",
        validation_entry="20260626T060400Z",
        validation="pytest; evaluated blocked/stale/oops states",
        review_finding="clean",
        acceptance="Requires browser full suite external validation.",
        annotations=[
            {
                "description": "blocked/stale/oops words in a note are not signals",
                "entry": "20260626T060200Z",
            },
        ],
    )
    xl = _completed_row(
        title="Cross-surface implementation",
        project="serve.ui",
        tags=["BLOCKED"],
        claim_at="2026-06-26T01:00:00.000000Z",
        validation_entry="20260626T033000Z",
        validation=[
            "uv run pytest tests/test_a.py",
            "spice dev pre-commit",
            "playwright smoke",
            "node tests/browser.js",
        ],
        review_finding="changes",
        annotations=[
            {
                "description": "review: finding=changes; by=reviewer",
                "entry": "20260626T034000Z",
            },
        ],
        depends=["a", "b", "c"],
        flow=("todo", "verify", "review"),
        acceptance="Requires browser validation.",
    )

    small_report = sizing.size_completed_task(small)
    xl_report = sizing.size_completed_task(xl)
    review_claim = sizing.size_completed_task(
        _completed_row(
            title="Review claim overwrote implementation claim",
            project="task.metrics",
            entry="20260626T060000Z",
            claim_at="2026-06-26T06:10:00.000000Z",
            validation_entry="20260626T060500Z",
            validation="pytest",
        )
    )

    assert small_report.label == "S"
    assert small_report.score == 0
    assert _component(small_report, "blocked").points == 0
    assert _component(small_report, "blocked").detail == "none"
    assert _component(small_report, "metadata").detail == "deps=0"
    assert _component(review_claim, "elapsed").detail == "5m"
    assert xl_report.label == "XL"
    assert xl_report.score >= 6
    assert _component(xl_report, "validation").points == 2
    assert _component(xl_report, "validation").detail == "records=4"
    assert _component(xl_report, "blocked").points == 2
    assert _component(xl_report, "blocked").detail == "tag:blocked"
    assert {component.name for component in xl_report.components} == {
        "elapsed",
        "validation",
        "review",
        "blocked",
        "metadata",
    }


def test_task_sizing_cli_renders_completed_rows(monkeypatch, capsys):
    rows = [
        _completed_row(
            title="Newest task",
            project="task.metrics",
            incepted="20260626T060000000002Z",
            end="20260626T061000Z",
            validation="uv run pytest; spice dev pre-commit",
        ),
        _completed_row(
            title="Other project",
            project="serve.ui",
            incepted="20260626T060000000001Z",
            end="20260626T060900Z",
            validation="pytest",
        ),
    ]
    monkeypatch.setattr(sizing.tw, "export", lambda filters: rows)

    args = build_parser().parse_args(
        ["task", "sizing", "--project", "task", "--limit", "1"]
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "METRICS-20260626T060000000002Z" in output
    assert "size_score=" in output
    assert "validation=+" in output
    assert "review=+" in output
    assert "Other project" not in output


def _completed_row(
    *,
    title: str,
    project: str,
    incepted: str = "20260626T060000000000Z",
    entry: str = "20260626T055900Z",
    claim_at: str = "2026-06-26T06:00:00.000000Z",
    validation_entry: str = "20260626T060500Z",
    end: str = "20260626T061000Z",
    validation: str | list[str] = "",
    review_finding: str = "clean",
    annotations: list[dict[str, str]] | None = None,
    tags: list[str] | None = None,
    depends: list[str] | None = None,
    flow: tuple[str, ...] = ("todo", "review"),
    acceptance: str = "",
) -> dict[str, object]:
    validation_items = [validation] if isinstance(validation, str) else validation
    validation_items = [item for item in validation_items if item]
    validation_field = " | ".join(validation_items)
    all_annotations = [
        {
            "description": f"validation: {item}",
            "entry": validation_entry,
        }
        for item in validation_items
    ] + [
        *(annotations or []),
    ]
    row: dict[str, object] = {
        "uuid": f"uuid-{incepted}",
        "incepted": incepted,
        "description": title,
        "project": project,
        "status": "completed",
        "entry": entry,
        "claim_at": claim_at,
        "end": end,
        "validation": validation_field,
        "annotations": all_annotations,
        "review_finding": review_finding,
        "tags": tags or [],
        "depends": depends or [],
        "acceptance": acceptance,
    }
    for index, phase in enumerate(flow):
        row[f"phase_{index}"] = phase
    return row


def _component(report: sizing.TaskSizing, name: str) -> sizing.SizingComponent:
    return next(component for component in report.components if component.name == name)
