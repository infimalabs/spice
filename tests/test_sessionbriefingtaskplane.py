"""Session briefing task-plane snapshot tests."""

from __future__ import annotations

import shutil

import pytest

from spice.sessions import briefingtaskplane
from spice.sessions.briefingtaskplane import classify_task_plane_rows
from spice.tasks import alloc, config, identity, tw


def test_briefing_snapshot_exports_once_and_marks_route_visibility(monkeypatch):
    actor = "actor-a"
    route = {
        "filter": ["project:session"],
        "manual": [],
        "lifetime": "Drive",
    }
    exported = [
        _row("route project", uuid="route-project"),
        _row(
            "private project",
            uuid="private-project",
            project=config.private_project(actor),
        ),
        _row(
            "actor origin",
            uuid="actor-origin",
            project="serve.ui",
            origin_thread=actor,
        ),
        _row("outside route", uuid="outside-route", project="serve.ui"),
        _row("oops pending", uuid="oops-pending", project=".oops"),
    ]
    calls: list[list[str]] = []
    filter_calls: list[dict[str, object] | None] = []
    monkeypatch.setattr(alloc.lanes, "team_route_for_actor", lambda selected: route)

    def effective_filter_terms(selected):
        filter_calls.append(selected)
        return ["project:session"]

    monkeypatch.setattr(alloc.lanes, "effective_filter_terms", effective_filter_terms)

    def export(filters):
        calls.append(filters)
        return exported

    monkeypatch.setattr(tw, "export", export)

    snapshot = alloc.briefing_snapshot(actor)

    assert snapshot.rows == tuple(exported)
    assert snapshot.visible_uuids == frozenset(
        {"route-project", "private-project", "actor-origin"}
    )
    assert calls == [["status.any:"]]
    assert filter_calls == [route]


def test_task_plane_snapshot_classifies_taskwarrior_state_fields():
    rows = [
        _row("ready", uuid="ready"),
        _row("wait elapsed", uuid="wait-elapsed", wait="20000101T000000Z"),
        _row(
            "scheduled elapsed",
            uuid="scheduled-elapsed",
            scheduled="20000101T000000Z",
        ),
        _row(
            "active",
            uuid="active",
            start="20260710T120000Z",
            claim_by="actor-a",
        ),
        _row("review", uuid="review", phase="review"),
        _row("blocked", uuid="blocked", depends=["pending-dependency"]),
        _row(
            "resolved dependency",
            uuid="resolved-dependency",
            depends=["completed-dependency"],
        ),
        _row("waiting", uuid="waiting", wait="20990101T000000Z"),
        _row(
            "scheduled",
            uuid="scheduled",
            scheduled="20990101T000000Z",
        ),
        _row("completed", uuid="completed", status="completed"),
        _row(
            "pending dependency",
            uuid="pending-dependency",
            project="serve.external",
        ),
        _row(
            "completed dependency",
            uuid="completed-dependency",
            project="serve.external",
            status="completed",
        ),
        _row("oops pending", uuid="oops-pending", project=".oops"),
        _row(
            "oops waiting",
            uuid="oops-waiting",
            project=".oops.tooling",
            status="waiting",
        ),
    ]
    visible_uuids = frozenset(
        {
            "ready",
            "wait-elapsed",
            "scheduled-elapsed",
            "active",
            "review",
            "blocked",
            "resolved-dependency",
            "waiting",
            "scheduled",
            "completed",
        }
    )

    classified = classify_task_plane_rows(
        rows,
        visible_uuids=visible_uuids,
        is_hidden=alloc.is_hidden,
        is_oops=alloc.is_oops,
    )

    assert _descriptions(classified.active) == ["active"]
    assert _descriptions(classified.ready) == [
        "ready",
        "wait elapsed",
        "scheduled elapsed",
        "resolved dependency",
    ]
    assert _descriptions(classified.review) == ["review"]
    assert _descriptions(classified.blocked) == ["blocked"]
    assert _descriptions(classified.completed) == ["completed"]
    assert _descriptions(classified.oops) == ["oops pending", "oops waiting"]


def test_snapshot_categories_match_live_taskwarrior_virtual_tags(tmp_path):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    config.set_backend(str(tmp_path / "task-backend"))
    try:
        ready = _add_task("snapshot ready")
        active = _add_task("snapshot active")
        tw.run([str(active["uuid"]), "modify", "start:now", "claim_by:actor-a"])
        dependency = _add_task("snapshot dependency")
        blocked = _add_task("snapshot blocked", depends=f"depends:{dependency['uuid']}")
        completed_dependency = _add_task("snapshot completed dependency")
        resolved = _add_task(
            "snapshot resolved", depends=f"depends:{completed_dependency['uuid']}"
        )
        tw.run([str(completed_dependency["uuid"]), "done"])
        review = _add_task("snapshot review", phase="review")
        completed = _add_task("snapshot completed")
        tw.run([str(completed["uuid"]), "done"])
        _add_task(
            "snapshot oops",
            project=".oops",
            phase="plan",
            wait="wait:2099-01-01",
        )

        all_rows = tw.export(["status.any:"])
        visible_uuids = frozenset(
            str(row["uuid"]) for row in all_rows if not alloc.is_hidden(row)
        )
        classified = classify_task_plane_rows(
            all_rows,
            visible_uuids=visible_uuids,
            is_hidden=alloc.is_hidden,
            is_oops=alloc.is_oops,
        )
        expected = {
            "active": tw.export(["status:pending", "+ACTIVE"]),
            "ready": [
                row
                for row in tw.export(["status:pending", "+READY", "-ACTIVE"])
                if str(row.get("phase") or "") != "review"
            ],
            "review": tw.export(["status:pending", "phase:review"]),
            "blocked": tw.export(["status:pending", "+BLOCKED"]),
            "completed": tw.export(["status:completed"]),
            "oops": alloc.oops_rows(),
        }

        assert {
            name: {str(row["uuid"]) for row in getattr(classified, name)}
            for name in expected
        } == {
            name: {str(row["uuid"]) for row in rows} for name, rows in expected.items()
        }
        assert _descriptions(classified.blocked) == [str(blocked["description"])]
        assert str(resolved["description"]) in _descriptions(classified.ready)
        assert str(ready["description"]) in _descriptions(classified.ready)
        assert _descriptions(classified.active) == [str(active["description"])]
        assert _descriptions(classified.review) == [str(review["description"])]
    finally:
        config.set_backend(None)


def test_task_plane_candidate_carries_active_project(tmp_path, monkeypatch):
    actor = "actor-a"
    active = _row(
        "active",
        uuid="active",
        start="20260710T120000Z",
        claim_by=actor,
        project="session.briefing",
        acceptance="active accepted",
    )
    monkeypatch.setattr(briefingtaskplane, "repo_root_from_cwd", lambda: tmp_path)
    monkeypatch.setattr(tw, "current_actor", lambda: actor)
    monkeypatch.setattr(
        alloc,
        "briefing_snapshot",
        lambda _actor: alloc.BriefingTaskSnapshot(
            rows=(active,),
            visible_uuids=frozenset({"active"}),
        ),
    )
    monkeypatch.setattr(identity, "render_handle", lambda _row: "ACTIVE-1")

    candidates = briefingtaskplane.collect_task_plane_candidates()

    assert candidates[0].project == "session.briefing"


def _add_task(
    description: str,
    *,
    project: str = "session.briefing",
    phase: str = "todo",
    depends: str = "",
    wait: str = "",
) -> dict[str, object]:
    tw.run(
        [
            "add",
            description,
            f"project:{project}",
            f"phase:{phase}",
            *([depends] if depends else []),
            *([wait] if wait else []),
        ]
    )
    return tw.export([f"description.is:{description}"])[0]


def _row(description: str, **fields: object) -> dict[str, object]:
    return {
        "description": description,
        "status": "pending",
        "project": "session.briefing",
        "phase": "todo",
        "entry": "20260101T000000Z",
        "urgency": 1.0,
        **fields,
    }


def _descriptions(rows: tuple[dict[str, object], ...]) -> list[str]:
    return [str(row["description"]) for row in rows]
