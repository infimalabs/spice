"""Session briefing task-plane snapshot tests."""

from __future__ import annotations

from pathlib import Path

from spice.sessions import briefingtaskplane
from spice.sessions.briefingtaskplane import classify_task_plane_rows
from spice.tasks import alloc, config, identity, tw


def test_briefing_rows_share_one_bootstrap_across_exact_state_exports(
    tmp_path, monkeypatch
):
    taskrc = tmp_path / "taskrc"
    inventory = [{"uuid": "inventory-row"}]
    ready = [{"uuid": "ready-row"}]
    blocked = [{"uuid": "blocked-row"}]
    calls: list[tuple[list[str], Path | None]] = []
    bootstraps: list[Path] = []
    monkeypatch.setattr(alloc.lanes, "team_route_for_actor", lambda _actor: None)
    monkeypatch.setattr(
        alloc,
        "effective_route_filter_args",
        lambda _actor, _route: ["project:session"],
    )
    monkeypatch.setattr(
        config, "bootstrap", lambda: bootstraps.append(taskrc) or taskrc
    )

    def export(
        filters: list[str] | None = None,
        *,
        overrides: list[str] | None = None,
        taskrc: Path | None = None,
    ) -> list[dict[str, object]]:
        del overrides
        selected = list(filters or [])
        calls.append((selected, taskrc))
        if "+READY" in selected:
            return ready
        if "+BLOCKED" in selected:
            return blocked
        return inventory

    monkeypatch.setattr(tw, "export", export)

    rows = alloc.briefing_rows("actor-a")

    assert bootstraps == [taskrc]
    assert rows == alloc.BriefingRows(
        inventory=tuple(inventory),
        ready=tuple(ready),
        blocked=tuple(blocked),
    )
    assert calls == [
        (
            [
                "(",
                "(",
                "status.any:",
                "project:session",
                ")",
                "or",
                f"project:{config.OOPS_PROJECT}",
                ")",
            ],
            taskrc,
        ),
        (
            ["status:pending", "+READY", "-ACTIVE", "project:session"],
            taskrc,
        ),
        (["status:pending", "+BLOCKED", "project:session"], taskrc),
    ]


def test_task_plane_snapshot_uses_taskwarrior_virtual_state_rows():
    rows = [
        _row("ready"),
        _row("completed dependency ready", depends=["completed-uuid"]),
        _row(
            "active",
            start="20260710T120000Z",
            claim_by="actor-a",
        ),
        _row("review", phase="review"),
        _row("blocked", depends=["pending-uuid"]),
        _row("completed", status="completed"),
        _row("oops pending", project=".oops"),
        _row("oops waiting", project=".oops.tooling", status="waiting"),
    ]

    classified = classify_task_plane_rows(
        rows,
        ready_rows=[rows[0], rows[1]],
        blocked_rows=[rows[4]],
        is_hidden=alloc.is_hidden,
        is_oops=alloc.is_oops,
    )

    assert _descriptions(classified.active) == ["active"]
    assert _descriptions(classified.ready) == ["ready", "completed dependency ready"]
    assert _descriptions(classified.review) == ["review"]
    assert _descriptions(classified.blocked) == ["blocked"]
    assert _descriptions(classified.completed) == ["completed"]
    assert _descriptions(classified.oops) == ["oops pending", "oops waiting"]


def test_task_plane_candidate_carries_active_project(tmp_path, monkeypatch):
    actor = "actor-a"
    active = _row(
        "active",
        start="20260710T120000Z",
        claim_by=actor,
        project="session.briefing",
        acceptance="active accepted",
    )
    monkeypatch.setattr(briefingtaskplane, "repo_root_from_cwd", lambda: tmp_path)
    monkeypatch.setattr(tw, "current_actor", lambda: actor)
    monkeypatch.setattr(
        alloc,
        "briefing_rows",
        lambda _actor: alloc.BriefingRows(
            inventory=(active,),
            ready=(),
            blocked=(),
        ),
    )
    monkeypatch.setattr(identity, "render_handle", lambda _row: "ACTIVE-1")

    candidates = briefingtaskplane.collect_task_plane_candidates()

    assert candidates[0].project == "session.briefing"


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
