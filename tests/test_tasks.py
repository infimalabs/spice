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
from spice.serve.teams import (
    TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import alloc, config, gitsync, identity, lanes, ops, render, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PEER_ACTOR = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


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
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def _make_orphan_commit(
    repo: Path, name: str = "orphan.txt", subject: str = "orphan work"
) -> str:
    (repo / name).write_text(f"{subject}\n", encoding="utf-8")
    _run(repo, "git", "add", name)
    _run(repo, "git", "commit", "-m", subject)
    return _git(repo, "rev-parse", "HEAD")


def test_task_adopt_mints_task_over_orphan_then_done_captures_it(remote_task_repo):
    orphan = _make_orphan_commit(remote_task_repo, subject="orphan fix worth keeping")
    assert gitsync.commits_ahead_of_baseline(remote_task_repo) == 1

    output = ops.adopt(project="task.unit")
    handle = output.splitlines()[0].split()[-1]
    row = identity.resolve(handle)

    assert "adopted 1 orphan commit into" in output
    assert f"next: spice task done {handle}" in output
    assert row["claim_by"] == ACTOR_A
    assert bool(row["start"])
    assert row["description"] == "orphan fix worth keeping"
    # The orphan was preserved, not fast-forwarded away.
    assert _git(remote_task_repo, "rev-parse", "HEAD") == orphan

    done_output = ops.done(handle, validation=["orphan captured"])
    review_row = identity.resolve(handle)

    assert f"advanced {handle} -> review" in done_output
    assert review_row["done_head"] == orphan
    assert (
        _git(remote_task_repo, "ls-remote", "origin", "refs/heads/main").split()[0]
        == review_row["done_merge_head"]
    )


def test_task_adopt_can_complete_orphan_in_one_shot(remote_task_repo):
    orphan = _make_orphan_commit(remote_task_repo, subject="one shot orphan")

    output = ops.adopt(
        project="task.unit",
        complete=True,
        validation=["one-shot validation"],
    )
    handle = output.splitlines()[0].split()[-1]
    review_row = identity.resolve(handle)

    assert "adopted 1 orphan commit into" in output
    assert f"advanced {handle} -> review" in output
    assert review_row["validation"] == "one-shot validation"
    assert review_row["done_head"] == orphan


def test_task_adopt_claims_existing_handle_over_orphan(remote_task_repo):
    handle = ops.add(
        "Pre-filed task awaiting its commit",
        project="task.unit",
        priority="medium",
        acceptance=["orphan is folded into this task"],
    )
    orphan = _make_orphan_commit(remote_task_repo)

    output = ops.adopt(handle)
    row = identity.resolve(handle)

    assert f"adopted 1 orphan commit into {handle}" in output
    assert row["claim_by"] == ACTOR_A
    assert bool(row["start"])
    assert _git(remote_task_repo, "rev-parse", "HEAD") == orphan


def test_task_adopt_refuses_when_no_orphan_commit(remote_task_repo):
    with pytest.raises(SpiceError, match="nothing to adopt"):
        ops.adopt(project="task.unit")


def test_task_add_claim_refuses_dirty_tree_without_creating_task(remote_task_repo):
    (remote_task_repo / "README.md").write_text("dirty\n", encoding="utf-8")

    with pytest.raises(SpiceError, match="commit or clear the working tree first"):
        ops.add("Dirty claim should not leak", project="task.unit", claim=True)

    rows = tw.export(["status:pending"])
    assert [
        row for row in rows if row.get("description") == "Dirty claim should not leak"
    ] == []


def test_task_add_claim_creates_and_claims_clean_task(task_repo):
    handle = ops.add("Clean claim lands", project="task.unit", claim=True)
    row = identity.resolve(handle)

    assert row["claim_by"] == ACTOR_A
    assert bool(row["start"])


def test_task_adopt_rejects_handle_with_new_task_fields(remote_task_repo):
    handle = ops.add(
        "Existing task", project="task.unit", priority="medium", acceptance=["x"]
    )
    _make_orphan_commit(remote_task_repo)
    with pytest.raises(SpiceError, match="either an existing <handle> or new-task"):
        ops.adopt(handle, project="task.unit")


def test_task_adopt_parser_accepts_done_with_validation():
    args = build_parser().parse_args(
        ["task", "adopt", "--done", "--validation", "tests passed"]
    )

    assert args.task_action == "adopt"
    assert args.done is True
    assert args.validation == ["tests passed"]


def test_task_done_review_flow_and_author_claim_separation(task_repo, monkeypatch):
    handle = ops.add(
        "Exercise task phase flow",
        project="task.unit",
        priority="medium",
        acceptance=["phase flow is covered"],
    )
    claimed = ops.claim(handle)
    head = _git(task_repo, "rev-parse", "HEAD")
    claimed_row = identity.resolve(handle)

    assert handle in claimed.splitlines()
    assert claimed_row["claim_by"] == ACTOR_A
    assert claimed_row["claim_head"] == head

    done_output = ops.done(handle, validation=["pytest task flow passed"])
    review_row = identity.resolve(handle)
    uuid = identity.uuid_of(review_row)

    assert f"advanced {handle} -> review" in done_output
    assert review_row["phase"] == "review"
    assert str(review_row["phase_i"]) == "1"
    assert review_row["review_author"] == ACTOR_A
    assert review_row["validation"] == "pytest task flow passed"
    assert review_row["done_head"] == head
    assert review_row["done_merge_head"] == head
    assert review_row["done_ref"] == head

    with pytest.raises(SpiceError, match="authored the review"):
        ops.claim(handle)

    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )
    assigned = ops.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == ACTOR_A

    review_output = ops.review(handle, finding="clean", note="review passed")
    completed_row = tw.export([uuid])[0]

    assert f"reviewed {handle} clean; completed {handle}" in review_output
    assert completed_row["status"] == "completed"
    assert completed_row["review_by"] == ACTOR_A
    assert completed_row["review_finding"] == "clean"
    assert completed_row["review_note"] == "review passed"


def test_task_next_repairs_active_claim_missing_owner(task_repo, monkeypatch):
    handle = ops.add(
        "Repair partial active claim",
        project="task.unit",
        priority="medium",
        acceptance=["active missing-owner claims are repaired"],
    )
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    tw.run([uuid, "modify", "start:now", *ops.CLAIM_CLEAR])
    monkeypatch.setattr(
        "spice.tasks.lanes.team_route_for_actor",
        lambda _actor: {"filter": ["project:task.unit"], "lifetime": "Drive"},
    )

    assigned = ops.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == ACTOR_A
    assert assigned["start"]


def test_manual_claim_subscribes_project_and_routes_review_to_teammate(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A, PEER_ACTOR], config=TeamConfig(lifetime="Steer")
    )
    handle = ops.add(
        "Manual claim out of lane",
        project="task.unit",
        priority="medium",
        acceptance=["manual claim subscribes the project"],
    )

    claimed = ops.claim(handle)
    after_claim = store.team_config(team.team_id)

    assert handle in claimed.splitlines()
    assert after_claim.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_claim.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CLAIM}
    ]

    ops.done(handle, validation=["claim subscription routed review"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    assigned = ops.next_task()

    assert identity.render_handle(assigned or {}) == handle
    assert assigned["claim_by"] == PEER_ACTOR


def test_task_next_auto_claim_does_not_rewrite_team_filters(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A],
        config=TeamConfig(lifetime="Steer", task_filters=("task.unit",)),
    )
    handle = ops.add(
        "Auto claim in lane",
        project="task.unit",
        priority="medium",
        acceptance=["auto claim leaves filter store unchanged"],
    )
    before = store.global_revision()

    assigned = ops.next_task()
    after = store.global_revision()
    entries = store.team_config(team.team_id).task_filter_entries

    assert identity.render_handle(assigned or {}) == handle
    assert after == before
    assert [entry.to_payload() for entry in entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL}
    ]


def test_manual_claim_skips_private_project_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A])
    handle = ops.add(
        "Private manual claim",
        priority="medium",
        acceptance=["private claims do not touch team filters"],
    )
    before = store.global_revision()

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_manual_claim_skips_subscription_for_teamless_actor(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    handle = ops.add(
        "Teamless manual claim",
        project="task.unit",
        priority="medium",
        acceptance=["teamless claims do not create subscriptions"],
    )
    before = store.global_revision()

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store.global_revision() == before


def test_manual_claim_skips_oops_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A], config=TeamConfig(lifetime="Steer"))
    created = ops.oops("Manual oops claim target", description="triage only")
    handle = created.split()[1]
    before = store.global_revision()

    claimed = ops.claim(handle)

    assert handle in claimed.splitlines()
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_final_review_completion_gcs_auto_claim_filter(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A, PEER_ACTOR])
    handle = ops.add(
        "Review keeps project subscribed",
        project="task.unit",
        priority="medium",
        acceptance=["review keeps auto filter until complete"],
    )
    ops.claim(handle)

    ops.done(handle, validation=["implementation leaves review pending"])
    review_config = store.team_config(team.team_id)

    assert review_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in review_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
    ]

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    ops.review(handle, finding="clean", note="review complete")
    final_config = store.team_config(team.team_id)

    assert final_config.task_filters == ()
    assert final_config.task_filter_entries == ()


def test_empty_project_gc_removes_auto_sources_but_preserves_manual(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A, PEER_ACTOR],
        config=TeamConfig(task_filters=("task.unit",)),
    )
    handle = ops.add(
        "Manual filter survives auto gc",
        project="task.unit",
        priority="medium",
        acceptance=["manual task filter survives empty-project gc"],
    )
    ops.claim(handle)
    with_auto = store.team_config(team.team_id)

    assert [entry.to_payload() for entry in with_auto.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL},
    ]

    ops.done(handle, validation=["implementation complete"])
    # Keep the manual source through the final review path while reclaiming auto.
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    ops.review(handle, finding="clean", note="manual survives")

    final_config = store.team_config(team.team_id)

    assert final_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in final_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_MANUAL}
    ]


def test_delete_gcs_empty_auto_create_filter_after_project_subtree_empties(
    task_repo,
):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A])
    store.add_task_filter(
        team.team_id, "task.unit", source=TASK_FILTER_SOURCE_AUTO_CREATE
    )
    parent = ops.add(
        "Parent task",
        project="task.unit",
        priority="medium",
        acceptance=["parent deletion keeps filter while child pending"],
    )
    child = ops.add(
        "Child task",
        project="task.unit.child",
        priority="medium",
        acceptance=["child deletion empties parent subtree"],
    )

    ops.delete(parent, "parent abandoned")
    still_live = store.team_config(team.team_id)

    assert still_live.task_filters == ("task.unit", "task.unit.child")

    ops.delete(child, "child abandoned")
    emptied = store.team_config(team.team_id)
    after_empty_revision = store.global_revision()
    ops._gc_empty_project_task_filters("task.unit")

    assert emptied.task_filters == ()
    assert emptied.task_filter_entries == ()
    assert store.global_revision() == after_empty_revision


def test_drive_task_creation_subscribes_project_idempotently(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A], config=TeamConfig(lifetime="Drive"))

    first = ops.add(
        "Drive creates first task",
        project="task.unit",
        priority="medium",
        acceptance=["drive creation subscribes"],
    )
    after_first = store.global_revision()
    after_first_config = store.team_config(team.team_id)
    second = ops.add(
        "Drive creates second task",
        project="task.unit",
        priority="medium",
        acceptance=["duplicate drive creation is idempotent"],
    )
    after_second = store.global_revision()

    assert first != second
    assert after_first_config.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_first_config.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]
    assert after_second == after_first


def test_steer_task_creation_keeps_manual_subscription_boundary(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A], config=TeamConfig(lifetime="Steer"))
    before = store.global_revision()

    handle = ops.add(
        "Steer creates task",
        project="task.unit",
        priority="medium",
        acceptance=["steer creation does not auto-subscribe"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_drain_task_creation_uses_effective_visibility_not_stored_filter(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A], config=TeamConfig(lifetime="Drain"))
    before = store.global_revision()

    handle = ops.add(
        "Drain creates task",
        project="task.unit",
        priority="medium",
        acceptance=["drain creation relies on computed visibility"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_teamless_task_creation_skips_drive_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    before = store.global_revision()

    handle = ops.add(
        "Teamless creates task",
        project="task.unit",
        priority="medium",
        acceptance=["teamless creation has no team subscription"],
    )

    assert identity.resolve(handle)["project"] == "task.unit"
    assert store.global_revision() == before


def test_drive_oops_creation_skips_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A], config=TeamConfig(lifetime="Drive"))
    before = store.global_revision()

    created = ops.oops("Drive oops creation", description="triage only")
    handle = created.split()[1]
    row = identity.resolve(handle)

    assert row["project"] == config.OOPS_PROJECT
    assert store.global_revision() == before
    assert store.team_config(team.team_id).task_filters == ()


def test_drive_create_allocate_review_and_gc_capstone(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_A, PEER_ACTOR], config=TeamConfig(lifetime="Drive")
    )
    handle = ops.add(
        "Drive capstone task",
        project="task.unit",
        priority="medium",
        acceptance=["drive lifecycle capstone"],
    )
    after_create = store.team_config(team.team_id)

    assert after_create.task_filters == ("task.unit",)
    assert [entry.to_payload() for entry in after_create.task_filter_entries] == [
        {"project": "task.unit", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]

    assigned = ops.next_task()

    assert identity.render_handle(assigned or {}) == handle

    ops.done(handle, validation=["implementation complete"])
    review_pending = store.team_config(team.team_id)

    assert review_pending.task_filters == ("task.unit",)

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    review = ops.next_task()

    assert identity.render_handle(review or {}) == handle

    ops.review(handle, finding="clean", note="capstone review complete")
    after_review = store.team_config(team.team_id)

    assert after_review.task_filters == ()
    assert after_review.task_filter_entries == ()


def test_drain_visibility_and_empty_steer_private_fail_closed(task_repo, monkeypatch):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    store.create_team(members=[ACTOR_A], config=TeamConfig(lifetime="Drain"))
    store.create_team(members=[PEER_ACTOR], config=TeamConfig(lifetime="Steer"))
    public = ops.add(
        "Drain-visible public task",
        project="serve.ui",
        priority="medium",
        acceptance=["drain sees assignable public work"],
    )
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    private = ops.add(
        "Peer private task",
        priority="medium",
        acceptance=["empty steer sees own private work"],
    )

    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    drain_assigned = ops.next_task()

    assert identity.render_handle(drain_assigned or {}) == public
    assert drain_assigned["project"] == "serve.ui"

    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    steer_assigned = ops.next_task()

    assert identity.render_handle(steer_assigned or {}) == private
    assert steer_assigned["project"] == ops.default_project(PEER_ACTOR)


def test_lifetime_filter_args_use_single_visibility_contract(task_repo):
    assert task_repo.is_dir()
    stored = ["project:task.unit"]
    private = f"project:{ops.default_project(ACTOR_A)}"

    assert lanes.filter_args({"filter": stored, "lifetime": "Steer"}) == stored
    assert lanes.filter_args({"filter": stored, "lifetime": "Drive"}) == stored
    assert lanes.filter_args({"filter": stored, "lifetime": "Drain"}) == [
        "(",
        "project:serve",
        "or",
        "project:task",
        ")",
    ]
    assert lanes.filter_args({"filter": [], "lifetime": "Steer"}) == []
    assert ops.effective_filter_args(ACTOR_A, []) == [private]
    assert ops.effective_filter_args(
        ACTOR_A,
        lanes.filter_args({"filter": stored, "lifetime": "Drain"}),
    ) == [
        "(",
        private,
        "or",
        "(",
        "project:serve",
        "or",
        "project:task",
        ")",
        ")",
    ]


def test_task_add_stores_description_and_caps_title(task_repo):
    overlong = "A" * (ops.TASK_TITLE_LIMIT + 1)
    with pytest.raises(SpiceError, match="move detail into --description"):
        ops.add(
            overlong,
            project="task.unit",
            priority="medium",
            acceptance=["title cap is enforced"],
        )

    body = "Longer context reviewers should keep current."
    handle = ops.add(
        "Short subject",
        project="task.unit",
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
    shared_ref = ".spice/attachments/digest/01-image.png"

    handle = ops.add(
        "Preserve shared attachment references",
        project="task.unit",
        description=f"Screenshot/reference attachment: {shared_ref}.",
        priority="medium",
        acceptance=[f"Open {shared_ref}."],
    )
    row = identity.resolve(handle)

    assert row["task_description"] == f"Screenshot/reference attachment: {shared_ref}."
    assert row["acceptance"] == f"Open {shared_ref}."
    assert shared.is_file()


def test_task_note_preserves_shared_attachment_refs(task_repo):
    handle = ops.add(
        "Track attachment note",
        project="task.unit",
        priority="medium",
        acceptance=["notes are normalized"],
    )
    shared = shared_attachment_root(task_repo) / "digest" / "02-image.png"
    shared.parent.mkdir(parents=True, exist_ok=True)
    shared.write_bytes(b"note-image")
    shared_ref = ".spice/attachments/digest/02-image.png"

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

    handle = ops.add(
        "Exercise configured flow",
        project="qa.pipeline",
        priority="medium",
        acceptance=["configured flow is applied"],
    )
    row = identity.resolve(handle)
    catalog = config.task_project_validation_catalog()

    assert config.resolve_flow(None, "qa.pipeline") == ["todo", "verify", "review"]
    assert ops.phases_of(row) == ["todo", "verify", "review"]
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


def test_unclean_review_links_existing_followup(task_repo, monkeypatch):
    handle = _review_claim(task_repo, monkeypatch)
    reviewed_uuid = identity.uuid_of(identity.resolve(handle))
    existing = ops.add(
        "Existing review follow-up",
        project="task.unit",
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
    handle = ops.add(
        "Review follow-up invariant",
        project="task.unit",
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
    assigned = ops.next_task()
    assert identity.render_handle(assigned or {}) == handle
    return handle


def _git(repo: Path, *args: str) -> str:
    return _run(repo, "git", *args).stdout.strip()


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
