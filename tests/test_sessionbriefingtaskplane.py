"""Task-plane snapshot coverage for session briefings."""

from __future__ import annotations

from pathlib import Path

from spice.sessions import briefingtaskplane
from spice.tasks import alloc, config, identity, lanes, tw


def test_task_plane_uses_one_inventory_and_two_virtual_state_exports(
    tmp_path, monkeypatch
):
    actor = "actor-a"
    taskrc = tmp_path / "taskrc"
    scope = ["project:task"]
    inventory = [
        _task_row(
            "ACTIVE-1",
            status="pending",
            phase="todo",
            project="task.unit",
            claim_by=actor,
            acceptance="active accepted",
        ),
        _task_row("REVIEW-1", status="pending", phase="review"),
        _task_row("DONE-1", status="completed", validation="done validated"),
        _task_row("OOPS-1", status="waiting", project=config.OOPS_PROJECT),
    ]
    ready = [_task_row("READY-1", status="pending", phase="todo")]
    blocked = [_task_row("BLOCKED-1", status="pending", phase="plan")]
    exports: list[tuple[list[str], Path | None]] = []
    bootstraps: list[Path] = []

    monkeypatch.setattr(briefingtaskplane, "repo_root_from_cwd", lambda: tmp_path)
    monkeypatch.setattr(tw, "current_actor", lambda: actor)
    monkeypatch.setattr(lanes, "team_route_for_actor", lambda _actor: None)
    monkeypatch.setattr(
        alloc, "effective_route_filter_args", lambda _actor, _route: scope
    )
    monkeypatch.setattr(
        config, "bootstrap", lambda: bootstraps.append(taskrc) or taskrc
    )
    monkeypatch.setattr(identity, "render_handle", lambda row: str(row["handle"]))

    def fake_export(
        filters: list[str] | None = None,
        *,
        overrides: list[str] | None = None,
        taskrc: Path | None = None,
    ) -> list[dict[str, object]]:
        del overrides
        selected = list(filters or [])
        exports.append((selected, taskrc))
        if "+READY" in selected:
            return ready
        if "+BLOCKED" in selected:
            return blocked
        return inventory

    monkeypatch.setattr(tw, "export", fake_export)

    candidates = briefingtaskplane.collect_task_plane_candidates()

    assert bootstraps == [taskrc]
    assert exports == [
        (
            [
                "(",
                "(",
                *scope,
                ")",
                "or",
                f"project:{config.OOPS_PROJECT}",
                ")",
            ],
            taskrc,
        ),
        (["status:pending", "+READY", "-ACTIVE", *scope], taskrc),
        (["status:pending", "+BLOCKED", *scope], taskrc),
    ]
    assert [candidate.text.split()[0] for candidate in candidates] == [
        "claim",
        "posture",
        "ready",
        "review",
        "completed",
        "oops",
    ]
    assert candidates[0].project == "task.unit"
    assert "active=1 ready=1 review=1 blocked=1 oops=1" in candidates[1].text


def _task_row(handle: str, **fields: object) -> dict[str, object]:
    return {
        "handle": handle,
        "description": f"{handle} description",
        "entry": "20260101T000000Z",
        "urgency": 1.0,
        **fields,
    }
