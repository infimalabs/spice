"""Session briefing task-plane snapshot tests."""

from spice.sessions.briefingtaskplane import classify_task_plane_rows
from spice.tasks import alloc, config, tw


def test_briefing_rows_exports_scope_and_oops_in_one_snapshot(monkeypatch):
    exported = [{"uuid": "snapshot-row"}]
    calls: list[list[str]] = []
    monkeypatch.setattr(alloc.lanes, "team_route_for_actor", lambda actor: None)
    monkeypatch.setattr(
        alloc,
        "effective_route_filter_args",
        lambda actor, route: ["project:session"],
    )

    def export(filters):
        calls.append(filters)
        return exported

    monkeypatch.setattr(tw, "export", export)

    rows = alloc.briefing_rows("actor-a")

    assert rows == exported
    assert calls == [
        [
            "(",
            "(",
            "status.any:",
            "project:session",
            ")",
            "or",
            f"project:{config.OOPS_PROJECT}",
            ")",
        ]
    ]


def test_task_plane_snapshot_classifies_taskwarrior_state_fields():
    rows = [
        _row("ready"),
        _row("wait elapsed", wait="20000101T000000Z"),
        _row("scheduled elapsed", scheduled="20000101T000000Z"),
        _row(
            "active",
            start="20260710T120000Z",
            claim_by="actor-a",
        ),
        _row("review", phase="review"),
        _row("blocked", depends=["dependency-uuid"]),
        _row("waiting", wait="20990101T000000Z"),
        _row("scheduled", scheduled="20990101T000000Z"),
        _row("completed", status="completed"),
        _row("oops pending", project=".oops"),
        _row("oops waiting", project=".oops.tooling", status="waiting"),
    ]

    classified = classify_task_plane_rows(
        rows,
        is_hidden=alloc.is_hidden,
        is_oops=alloc.is_oops,
    )

    assert _descriptions(classified.active) == ["active"]
    assert _descriptions(classified.ready) == [
        "ready",
        "wait elapsed",
        "scheduled elapsed",
    ]
    assert _descriptions(classified.review) == ["review"]
    assert _descriptions(classified.blocked) == ["blocked"]
    assert _descriptions(classified.completed) == ["completed"]
    assert _descriptions(classified.oops) == ["oops pending", "oops waiting"]


def _row(description: str, **fields: object) -> dict[str, object]:
    return {
        "description": description,
        "status": "pending",
        "project": "session.briefing",
        "phase": "todo",
        **fields,
    }


def _descriptions(rows: tuple[dict[str, object], ...]) -> list[str]:
    return [str(row["description"]) for row in rows]
