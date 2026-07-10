"""Task control-plane lifecycle, allocator, and git publication behavior."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.paths import shared_attachment_root
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CREATE,
    ServeTeamStore,
    TeamConfig,
)
from spice.serve.team.ids import thread_actor_id
from spice.tasks import alloc, claimstate, config, create, identity, ops, render, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A_MEMBER = thread_actor_id(ACTOR_A)
PEER_ACTOR_MEMBER = thread_actor_id(PEER_ACTOR)


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


@pytest.fixture
def remote_task_repo(tmp_path, monkeypatch):
    """A task-wired worktree with a real upstream baseline (origin/main)."""
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "repo")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def _make_loose_commit(
    repo: Path, name: str = "loose.txt", subject: str = "loose work"
) -> str:
    (repo / name).write_text(f"{subject}\n", encoding="utf-8")
    _run(repo, "git", "add", name)
    _run(repo, "git", "commit", "-m", subject)
    return _git(repo, "rev-parse", "HEAD")


def _ready_handles() -> set[str]:
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return {
        identity.render_handle(row)
        for row in rows
        if not alloc.is_hidden(row) and not str(row.get("claim_by") or "")
    }


def test_task_edit_changes_priority_in_place(task_repo):
    handle = create.add(
        "Bump me",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="low",
    )
    assert identity.resolve(handle)["priority"] == "L"

    ops.edit(handle, priority="high")

    assert identity.resolve(handle)["priority"] == "H"


def test_task_edit_reassigns_project_in_place(task_repo):
    handle = create.add(
        "Move me",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
    )

    ops.edit(handle, project="task.moved")

    assert identity.resolve(handle)["project"] == "task.moved"


def test_task_edit_requires_at_least_one_field(task_repo):
    handle = create.add(
        "Leave me",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
    )
    with pytest.raises(SpiceError):
        ops.edit(handle)


def test_task_edit_replaces_acceptance_in_place(task_repo):
    handle = create.add(
        "Reword my acceptance",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["original criterion"],
    )

    ops.edit(handle, acceptance=["first bookend", "second bookend"])

    assert identity.resolve(handle)["acceptance"] == "first bookend | second bookend"


def test_task_edit_acceptance_unsticks_plan_phase(task_repo):
    handle = create.add(
        "Plan gains bookend acceptance via edit",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["plan", "todo", "review"],
    )
    child = create.add(
        "Accepted child for edited plan",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["child node has acceptance"],
    )
    ops.depends(handle, [child])
    ops.claim(handle)
    with pytest.raises(SpiceError, match="bookend acceptance"):
        ops.done(handle, validation=["plan attempted without bookend"])

    ops.edit(handle, acceptance=["plan bookend added via edit"])
    output = ops.done(handle, validation=["plan board populated"])

    row = identity.resolve(handle)
    assert f"advanced {handle} -> todo" in output
    assert row["phase"] == "todo"
    assert row["acceptance"] == "plan bookend added via edit"


def test_task_edit_acceptance_rejects_completed_task(task_repo):
    handle = create.add(
        "Complete before acceptance edit",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        flow=["todo"],
        acceptance=["complete once"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["completed ahead of the edit attempt"])

    with pytest.raises(SpiceError, match="cannot edit acceptance for a completed"):
        ops.edit(handle, acceptance=["late criterion"])


def test_task_delete_allows_unclaimed_task(task_repo):
    handle = create.add(
        "Delete unclaimed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
    )
    uuid = identity.uuid_of(identity.resolve(handle))

    output = ops.delete(handle, "duplicate")
    row = tw.export([uuid])[0]

    assert output == handle
    assert row["status"] == "deleted"
    assert row["delete_reason"] == "duplicate"


def test_task_delete_refuses_live_claim_without_override(task_repo):
    handle = create.add(
        "Delete claimed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
    )
    ops.claim(handle)

    with pytest.raises(SpiceError, match=f"cannot delete {handle}: live claim held"):
        ops.delete(handle, "duplicate")

    row = identity.resolve(handle)
    assert row["status"] == "pending"
    assert row["claim_by"] == ACTOR_A


def test_task_delete_force_claimed_logs_holder(task_repo):
    handle = create.add(
        "Force delete claimed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
    )
    ops.claim(handle)
    claimed = identity.resolve(handle)
    uuid = identity.uuid_of(claimed)
    holder = (
        f"claim_by={ACTOR_A} claim_thread={ACTOR_A} "
        f"claim_until={claimed['claim_until']}"
    )

    output = ops.delete(handle, "duplicate", force_claimed=True)
    row = tw.export([uuid])[0]
    annotations = [str(item.get("description") or "") for item in row["annotations"]]

    assert output == f"warning: deleted {handle} despite live claim {holder}\n{handle}"
    assert row["status"] == "deleted"
    assert f"forced delete of live claim: {holder}" in annotations
    assert "deleted: duplicate" in annotations


def test_task_wake_clears_multiple_waits_and_makes_tasks_current(task_repo):
    handles = [
        create.add(
            f"Wake delayed task {index}",
            project="task.unit",
            origin="ack:20260101T000000000000Z",
            priority="medium",
            wait=config.OOPS_WAIT,
        )
        for index in range(4)
    ]
    delayed = [identity.resolve(handle) for handle in handles]

    assert all(row.get("wait") for row in delayed)
    assert not set(handles) & _ready_handles()

    output = ops.wake(handles)
    rows = [identity.resolve(handle) for handle in handles]

    for handle in handles:
        assert f"woke {handle}: wait:" in output
    assert "next: spice task next" in output
    assert all(not str(row.get("wait") or "") for row in rows)
    assert set(handles) <= _ready_handles()
    assert all(not str(row.get("claim_by") or "") for row in rows)
    assert all(not row.get("start") for row in rows)


def test_task_wake_rejects_batch_without_partial_clear(task_repo):
    delayed = create.add(
        "Wake batch delayed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        wait=config.OOPS_WAIT,
    )
    claimed = create.add(
        "Wake batch claimed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        claim=True,
    )

    with pytest.raises(SpiceError, match="active or claimed"):
        ops.wake([delayed, claimed])

    row = identity.resolve(delayed)
    assert row.get("wait")
    assert delayed not in _ready_handles()
    assert not str(row.get("claim_by") or "")
    assert not row.get("start")


def test_task_wake_refuses_deferred_oops_triage(task_repo):
    created = ops.oops(
        "Delayed oops remains triage",
        description="triage only",
        origin="ack:20260101T000000000000Z",
    )
    handle = created.split()[1]

    with pytest.raises(SpiceError, match="oops triage") as exc:
        ops.wake([handle])

    assert "wake --into <public-project>" in str(exc.value)


def test_task_oops_kind_routes_to_child_board_with_caller_tags_only(task_repo):
    created = ops.oops(
        "Kind routes to a child board",
        description="triage only",
        kind="Tooling",
        tags=["repro"],
        origin="ack:20260101T000000000000Z",
    )
    handle = created.split()[1]
    row = identity.resolve(handle)

    assert row["project"] == f"{config.OOPS_PROJECT}.tooling"
    assert row["phase"] == "plan"
    assert handle.startswith("TOOLING-")
    assert row.get("tags") == ["repro"]
    assert row.get("wait")


def test_task_wake_into_promotes_deferred_oops_into_public_project(task_repo):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    created = ops.oops(
        "Promote this oops into the queue",
        description="promotion candidate",
        origin="ack:20260101T000000000000Z",
    )
    handle = created.split()[1]
    assert identity.resolve(handle).get("wait")

    output = ops.wake([handle], into="task.unit")
    row = identity.resolve(handle)
    fresh = identity.render_handle(row)
    team_config = store.team_config(team.team_id)

    assert row["project"] == "task.unit"
    assert not str(row.get("wait") or "")
    assert row.get("tags", []) == []
    assert fresh != handle
    assert f"promoted {handle} -> {fresh}: wait: project:task.unit" in output
    assert "route_filter=added:task.unit:auto:create" in output
    assert team_config.task_filters == ("task.unit",)
    assert fresh in _ready_handles()


def test_task_wake_into_rejects_hidden_or_malformed_target_and_keeps_wait(task_repo):
    created = ops.oops(
        "Hidden target keeps deferral",
        description="triage only",
        origin="ack:20260101T000000000000Z",
    )
    handle = created.split()[1]
    project_before = str(identity.resolve(handle)["project"])

    with pytest.raises(SpiceError, match="hidden project stem"):
        ops.wake([handle], into=".oops.triage")
    with pytest.raises(SpiceError, match="at least"):
        ops.wake([handle], into="task")

    row = identity.resolve(handle)
    assert row.get("wait")
    assert str(row["project"]) == project_before


def test_task_wake_into_still_refuses_active_or_claimed(task_repo):
    claimed = create.add(
        "Promotion refuses claimed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        claim=True,
    )

    with pytest.raises(SpiceError, match="active or claimed"):
        ops.wake([claimed], into="task.unit")


def test_drive_wake_auto_subscribes_woken_project(task_repo):
    handle = create.add(
        "Drive wakes delayed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        wait=config.OOPS_WAIT,
        acceptance=["drive wake subscribes delayed work"],
    )
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drive")
    )
    before = store.global_revision()

    output = ops.wake([handle])
    team_config = store.team_config(team.team_id)

    assert store.global_revision() > before
    assert f"woke {handle}: wait:" in output
    assert "route_filter=added:task.unit:auto:create" in output
    assert team_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in team_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]


def test_drain_wake_auto_subscribes_woken_project(task_repo):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Drain")
    )
    handle = create.add(
        "Drain wakes delayed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        wait=config.OOPS_WAIT,
        acceptance=["drain wake subscribes delayed work"],
    )
    assert store.team_config(team.team_id).task_filters == ()
    before = store.global_revision()

    output = ops.wake([handle])
    team_config = store.team_config(team.team_id)

    assert store.global_revision() > before
    assert "route_filter=added:task.unit:auto:create" in output
    assert team_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in team_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]


def test_steer_wake_keeps_preparation_only_boundary(task_repo):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A_MEMBER], config=TeamConfig(lifetime="Steer")
    )
    handle = create.add(
        "Steer wakes delayed task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        wait=config.OOPS_WAIT,
        acceptance=["steer wake remains preparation only"],
    )
    before = store.global_revision()

    output = ops.wake([handle])

    assert store.global_revision() == before
    assert "route_filter=skipped:task.unit:lifetime:Steer" in output
    assert store.team_config(team.team_id).task_filters == ()


def test_task_add_stores_description_and_caps_title(task_repo):
    overlong = "A" * (create.TASK_TITLE_LIMIT + 1)
    with pytest.raises(SpiceError, match="move detail into --description"):
        create.add(
            overlong,
            project="task.unit",
            origin="ack:20260101T000000000000Z",
            priority="medium",
            acceptance=["title cap is enforced"],
        )

    body = "Longer context reviewers should keep current."
    handle = create.add(
        "Short subject",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        description=body,
        priority="medium",
        acceptance=["description is stored"],
    )
    row = identity.resolve(handle)

    assert row["description"] == "Short subject"
    assert row["task_description"] == body
    shown = render.render_show(handle)
    assert "title Short subject" in shown
    assert f"description {body}" in shown


def test_task_add_preserves_shared_attachment_refs(task_repo):
    shared = shared_attachment_root(task_repo) / "digest" / "01-image.png"
    shared.parent.mkdir(parents=True, exist_ok=True)
    shared.write_bytes(b"shared-image")
    shared_ref = shared.as_posix()

    handle = create.add(
        "Preserve shared attachment references",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        description=f"Screenshot/reference attachment: {shared_ref}.",
        priority="medium",
        acceptance=[f"Open {shared_ref}."],
    )
    row = identity.resolve(handle)

    assert row["task_description"] == f"Screenshot/reference attachment: {shared_ref}."
    assert row["acceptance"] == f"Open {shared_ref}."
    assert shared.is_file()


def test_task_note_preserves_shared_attachment_refs(task_repo):
    handle = create.add(
        "Track attachment note",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["notes are normalized"],
    )
    shared = shared_attachment_root(task_repo) / "digest" / "02-image.png"
    shared.parent.mkdir(parents=True, exist_ok=True)
    shared.write_bytes(b"note-image")
    shared_ref = shared.as_posix()

    ops.note(
        handle,
        f"Screenshot reference: {shared_ref}",
    )
    shown = render.render_show(handle)

    assert f"Screenshot reference: {shared_ref}" in shown
    assert shared.is_file()


def test_repo_configured_per_stem_default_flow_feeds_task_add(task_repo):
    (task_repo / "pyproject.toml").write_text(
        "[tool.spice.tasks]\n"
        'stems = ["qa"]\n'
        "\n"
        "[tool.spice.tasks.flows]\n"
        'qa = ["todo", "verify", "review"]\n',
        encoding="utf-8",
    )

    handle = create.add(
        "Exercise configured flow",
        project="qa.pipeline",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["configured flow is applied"],
    )
    row = identity.resolve(handle)
    catalog = config.task_project_validation_catalog()

    assert config.resolve_flow(None, "qa.pipeline") == ["todo", "verify", "review"]
    assert claimstate.phases_of(row) == ["todo", "verify", "review"]
    assert catalog["perStemFlows"]["qa"] == ["todo", "verify", "review"]


def test_repo_configured_per_stem_default_flow_rejects_unknown_phase(task_repo):
    (task_repo / "pyproject.toml").write_text(
        "[tool.spice.tasks]\n"
        'stems = ["qa"]\n'
        "\n"
        "[tool.spice.tasks.flows]\n"
        'qa = ["todo", "ship", "review"]\n',
        encoding="utf-8",
    )

    with pytest.raises(SpiceError, match="phase 'ship' is not approved"):
        config.resolve_flow(None, "qa.pipeline")


def test_allocator_spreads_from_peer_cell_then_sticks_to_last_cell():
    ready = [
        _row("same-crowded", project="task.alpha", phase="todo", urgency=10),
        _row("different", project="task.beta", phase="todo", urgency=9),
        _row("same-project", project="task.alpha", phase="review", urgency=8),
        _row("outside-band", project="task.gamma", phase="todo", urgency=1),
    ]
    claimed = [
        _row(
            "last",
            project="task.alpha",
            phase="todo",
            urgency=1,
            claim_at="2026-01-01T00:00:00Z",
            claim_by=ACTOR_A,
        )
    ]
    active = [
        _row(
            "peer",
            project="task.alpha",
            phase="todo",
            urgency=1,
            claim_by=PEER_ACTOR,
        )
    ]

    ordered = alloc.order(ready, ACTOR_A, claimed, active)

    assert [row["description"] for row in ordered] == [
        "different",
        "same-project",
        "same-crowded",
        "outside-band",
    ]


def test_task_review_help_requires_description_check(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit):
        parser.parse_args(["task", "review", "--help"])

    help_text = capsys.readouterr().out
    assert "verify the task description is current" in help_text
    assert "description=..." in help_text
    assert (
        "Findings other than clean require durable follow-up tracking through "
        "either --then or --followup" in help_text
    )
    assert "adds the reviewed task as its dependency" in help_text


def test_unclean_review_requires_followup_tracking(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)

    with pytest.raises(SpiceError, match="requires follow-up tracking"):
        ops.review(handle, finding="changes", note="needs work")

    row = identity.resolve(handle)
    assert row["phase"] == "review"
    assert str(row.get("review_by") or "") == ""


def test_unclean_review_spawns_dependent_followup(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)
    reviewed_uuid = identity.uuid_of(identity.resolve(handle))

    output = ops.review(
        handle,
        finding="changes",
        note="needs coverage",
        then=[
            "title=Add review coverage | project=task.unit | "
            "acceptance=Regression covers the requested review change"
        ],
    )
    spawned = next(
        line.split()[1] for line in output.splitlines() if line.startswith("spawned ")
    )
    followup = identity.resolve(spawned)
    reviewed = tw.export([reviewed_uuid])[0]

    assert f"reviewed {handle} changes; completed {handle}" in output
    assert followup["description"] == "Add review coverage"
    assert reviewed_uuid in followup.get("depends", [])
    assert reviewed["status"] == "completed"
    assert reviewed["review_finding"] == "changes"


def test_unclean_review_passes_followups_to_feedback_bridge(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)
    calls: list[dict[str, object]] = []

    def fake_feedback(row, *, finding, note, followups, reviewer, reviewed_at):
        calls.append(
            {
                "handle": identity.render_handle(row),
                "finding": finding,
                "note": note,
                "followups": list(followups),
                "reviewer": reviewer,
                "reviewed_at": reviewed_at,
            }
        )
        return ops.reviewfeedback.ReviewFeedbackResult(
            "delivered",
            "source=task-review",
            key="20260102T000000000001Z",
            target_repo_root=str(task_repo),
        )

    monkeypatch.setattr(ops.reviewfeedback, "emit_review_feedback", fake_feedback)

    output = ops.review(
        handle,
        finding="changes",
        note="needs coverage",
        then=[
            "title=Add review coverage | project=task.unit | "
            "acceptance=Regression covers the requested review change"
        ],
    )
    spawned = next(
        line.split()[1] for line in output.splitlines() if line.startswith("spawned ")
    )

    assert calls == [
        {
            "handle": handle,
            "finding": "changes",
            "note": "needs coverage",
            "followups": [spawned],
            "reviewer": PEER_ACTOR,
            "reviewed_at": calls[0]["reviewed_at"],
        }
    ]
    assert str(calls[0]["reviewed_at"])
    assert (
        "review-feedback delivered; key=20260102T000000000001Z; "
        f"target={task_repo}; source=task-review"
    ) in output


def test_unclean_review_links_existing_followup(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)
    reviewed_uuid = identity.uuid_of(identity.resolve(handle))
    existing = create.add(
        "Existing review follow-up",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["Tracks the requested review change"],
    )

    output = ops.review(
        handle,
        finding="changes",
        note="use existing task",
        followup=[existing],
    )
    followup = identity.resolve(existing)
    reviewed = tw.export([reviewed_uuid])[0]

    assert f"reviewed {handle} changes; completed {handle}" in output
    assert f"linked {existing}" in output
    assert reviewed_uuid in followup.get("depends", [])
    assert reviewed["status"] == "completed"
    assert reviewed["review_finding"] == "changes"


def _row(
    description: str,
    *,
    project: str,
    phase: str,
    urgency: float,
    claim_at: str = "",
    claim_by: str = "",
) -> dict[str, object]:
    return {
        "description": description,
        "project": project,
        "phase": phase,
        "urgency": urgency,
        "claim_at": claim_at,
        "claim_by": claim_by,
    }


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _configure_git_identity(path)
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _configure_git_identity(repo: Path) -> None:
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")


def _review_claim(task_repo: Path, monkeypatch) -> str:
    assert task_repo.is_dir()
    handle = create.add(
        "Review follow-up invariant",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        acceptance=["review follow-up tracking is enforced"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["implementation validated"])
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    assigned = alloc.next_task()
    assert identity.render_handle(assigned or {}) == handle
    return handle


def test_bound_quality_gate_blocks_completion_while_metric_nonzero(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    (repo / "spice").mkdir(parents=True)
    (repo / "tests").mkdir()
    (repo / "spice" / "foo.py").write_text("_secret = 1\n", encoding="utf-8")
    (repo / "tests" / "test_foo.py").write_text(
        "from spice.foo import _secret\n\n"
        "def test_secret():\n    assert _secret == 1\n",
        encoding="utf-8",
    )
    monkeypatch.setattr("spice.tasks.ops.repo_root_from_cwd", lambda: repo)

    # A task bound to the coupling gate cannot complete while the gate is dirty.
    with pytest.raises(SpiceError, match="bound to a quality gate"):
        ops._require_bound_quality_gates_clean({"tags": ["gate:coupling"]})

    # An untagged task is unaffected even when the same repo is dirty.
    ops._require_bound_quality_gates_clean({"tags": ["plain"]})

    # Once the coupling is gone, the bound gate passes.
    (repo / "tests" / "test_foo.py").write_text(
        "def test_nothing():\n    assert True\n", encoding="utf-8"
    )
    ops._require_bound_quality_gates_clean({"tags": ["gate:coupling"]})


def _git(repo: Path, *args: str) -> str:
    return _run(repo, "git", *args).stdout.strip()


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
