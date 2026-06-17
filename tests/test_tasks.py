"""Task control-plane lifecycle, allocator, and git publication behavior."""

from __future__ import annotations

import hashlib
import re
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.mail import attachments as mail_attachments
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
GIT_TIMEOUT_RETURN_CODE = 124


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


def test_task_adopt_rejects_handle_with_new_task_fields(remote_task_repo):
    handle = ops.add(
        "Existing task", project="task.unit", priority="medium", acceptance=["x"]
    )
    _make_orphan_commit(remote_task_repo)
    with pytest.raises(SpiceError, match="either an existing <handle> or new-task"):
        ops.adopt(handle, project="task.unit")


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

    assert claimed == handle
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

    assert claimed == handle
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

    assert claimed == handle
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

    assert claimed == handle
    assert store.global_revision() == before


def test_manual_claim_skips_oops_subscription(task_repo):
    assert task_repo.is_dir()
    store = ServeTeamStore()
    team = store.create_team(members=[ACTOR_A], config=TeamConfig(lifetime="Steer"))
    created = ops.oops("Manual oops claim target", description="triage only")
    handle = created.split()[1]
    before = store.global_revision()

    claimed = ops.claim(handle)

    assert claimed == handle
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


def test_task_add_copies_inbox_attachment_refs_to_durable_store(task_repo):
    live_abs = (
        task_repo
        / ".spice"
        / "inbox"
        / "20260102T000000000004Z.attachments"
        / "01-image.png"
    )
    archived_abs = (
        task_repo
        / ".spice"
        / "inbox"
        / "archive"
        / "20260102T000000000004Z.attachments"
        / "01-image.png"
    )
    live_rel = ".spice/inbox/20260102T000000000004Z.attachments/02-image.png"
    archived_rel = (
        ".spice/inbox/archive/20260102T000000000004Z.attachments/03-image.png"
    )
    for path, data in (
        (live_abs, b"absolute-live"),
        (task_repo / live_rel, b"relative-live"),
        (task_repo / archived_rel, b"archived"),
    ):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    handle = ops.add(
        "Preserve attachment references",
        project="task.unit",
        description=(
            f"Screenshot/reference attachment: {live_abs}. "
            f"Relative reference: {live_rel}; already archived: "
            f"{archived_rel}."
        ),
        priority="medium",
        acceptance=[f"Resolve {live_rel} without broad searches."],
    )
    row = identity.resolve(handle)
    paths = _durable_artifact_paths(row["task_description"], row["acceptance"])

    assert len(paths) == 4
    assert len(set(paths)) == 3
    assert all(path.is_file() for path in paths)
    assert all(
        config.backend_root() / "artifacts" / "attachments" in path.parents
        for path in paths
    )
    assert archived_abs not in paths
    assert str(live_abs) not in row["task_description"]
    assert ".spice/inbox/" not in row["task_description"]
    assert ".spice/inbox/" not in row["acceptance"]


def test_task_note_copies_inbox_attachment_refs_to_durable_store(task_repo):
    handle = ops.add(
        "Track attachment note",
        project="task.unit",
        priority="medium",
        acceptance=["notes are normalized"],
    )
    live_rel = ".spice/inbox/20260102T000000000005Z.attachments/01-image.png"
    live_path = task_repo / live_rel
    live_path.parent.mkdir(parents=True, exist_ok=True)
    live_path.write_bytes(b"note-image")

    ops.note(
        handle,
        f"Screenshot reference: {live_rel}",
    )
    shown = render.render_show(handle)
    paths = _durable_artifact_paths(shown)

    assert len(paths) == 1
    assert paths[0].is_file()
    assert paths[0].read_bytes() == b"note-image"
    assert ".spice/inbox/20260102T000000000005Z.attachments" not in shown


def test_task_add_reports_unresolved_attachment_ref(task_repo):
    missing = ".spice/inbox/20260102T000000000006Z.attachments/01-image.png"

    with pytest.raises(SpiceError, match=re.escape(missing)):
        ops.add(
            "Track missing attachment",
            project="task.unit",
            description=f"Screenshot reference: {missing}",
            priority="medium",
            acceptance=["missing attachment is reported"],
        )


def test_task_add_replaces_partial_durable_attachment(task_repo):
    live_rel = ".spice/inbox/20260102T000000000008Z.attachments/01-image.png"
    live_path = task_repo / live_rel
    data = b"complete-image"
    live_path.parent.mkdir(parents=True, exist_ok=True)
    live_path.write_bytes(data)
    digest = hashlib.sha256(data).hexdigest()
    artifact_path = (
        config.backend_root() / "artifacts" / "attachments" / digest / live_path.name
    )
    artifact_path.parent.mkdir(parents=True, exist_ok=True)
    artifact_path.write_bytes(b"partial")

    handle = ops.add(
        "Replace partial attachment artifact",
        project="task.unit",
        description=f"Screenshot reference: {live_rel}",
        priority="medium",
    )
    row = identity.resolve(handle)
    paths = _durable_artifact_paths(row["task_description"])

    assert paths == [artifact_path]
    assert artifact_path.read_bytes() == data


def test_task_add_publishes_durable_attachment_with_atomic_replace(
    task_repo, monkeypatch
):
    live_rel = ".spice/inbox/20260102T000000000009Z.attachments/01-image.png"
    live_path = task_repo / live_rel
    live_path.parent.mkdir(parents=True, exist_ok=True)
    live_path.write_bytes(b"atomic-image")
    written_paths: list[Path] = []
    original_write = mail_attachments._write_bytes_fsynced

    def record_write(path: Path, data: bytes) -> None:
        written_paths.append(path)
        original_write(path, data)

    monkeypatch.setattr(mail_attachments, "_write_bytes_fsynced", record_write)

    handle = ops.add(
        "Atomically publish attachment artifact",
        project="task.unit",
        description=f"Screenshot reference: {live_rel}",
        priority="medium",
    )
    row = identity.resolve(handle)
    paths = _durable_artifact_paths(row["task_description"])

    assert len(paths) == 1
    assert paths[0].read_bytes() == b"atomic-image"
    assert paths[0] not in written_paths
    assert any(
        path.parent == paths[0].parent
        and path.name.startswith(f".{paths[0].name}.")
        and path.name.endswith(".tmp")
        for path in written_paths
    )


def test_task_note_reports_unresolved_attachment_ref(task_repo):
    handle = ops.add(
        "Track missing attachment note",
        project="task.unit",
        priority="medium",
        acceptance=["missing note attachment is reported"],
    )
    missing = ".spice/inbox/20260102T000000000007Z.attachments/01-image.png"

    with pytest.raises(SpiceError, match=re.escape(missing)):
        ops.note(handle, f"Screenshot reference: {missing}")


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


def test_integrate_and_publish_creates_baseline_first_merge_and_pushes(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")

    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")
    agent_head = _git(repo, "rev-parse", "HEAD")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "baseline.txt").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "baseline.txt")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000001Z",
        repo_root=repo,
        meta={
            "title": "Publish task work",
            "description": "Longer merge body for reviewers.",
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_head"] == agent_head
    assert captured["done_ref"] == merge_head
    assert captured["done_upstream"] == "origin/main"
    assert captured["done_upstream_head"] == upstream_head
    assert _git(repo, "rev-parse", "HEAD") == merge_head
    assert _merge_parents(repo, merge_head) == [upstream_head, agent_head]
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""
    message = _git(repo, "log", "-1", "--format=%B", merge_head)
    assert message == (
        "Publish task work\n\n"
        "Task: TASK-20260101T000000000001Z\n"
        "Task-Phase: todo\n"
        "Task-Project: task.unit\n"
        f"Task-Session: {ACTOR_A}"
    )


def test_integrate_and_publish_retries_non_fast_forward_publish_race(
    tmp_path, monkeypatch
):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")

    (repo / "agent.txt").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "agent.txt")
    _run(repo, "git", "commit", "-m", "agent work")
    agent_head = _git(repo, "rev-parse", "HEAD")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    real_run = gitsync._run
    push_attempts = 0

    def racing_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal push_attempts
        if args and args[0] == "push" and repo_root == repo:
            push_attempts += 1
            if push_attempts == 1:
                (peer / "baseline.txt").write_text(
                    "baseline raced ahead\n", encoding="utf-8"
                )
                _run(peer, "git", "add", "baseline.txt")
                _run(peer, "git", "commit", "-m", "baseline raced ahead")
                _run(peer, "git", "push", "origin", "main")
        return real_run(repo_root, *args)

    monkeypatch.setattr(gitsync, "_run", racing_run)

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000004Z",
        repo_root=repo,
        meta={
            "title": "Publish raced task work",
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task.unit",
        },
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]
    raced_upstream = _git(peer, "rev-parse", "HEAD")
    first_retry_parent, second_retry_parent = _merge_parents(repo, merge_head)

    assert push_attempts == 2
    assert captured["done_head"] == agent_head
    assert captured["done_upstream_head"] == raced_upstream
    assert first_retry_parent == raced_upstream
    assert _merge_parents(repo, second_retry_parent)[1] == agent_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def test_merge_message_omits_task_description_body():
    message = gitsync._compose_message(
        "TASK-20260101T000000000003Z",
        {
            "title": "Fix image labels",
            "description": (
                "Operator steering 20260612T043642083543Z: the labels "
                "input_image and view_image look clickable but do not navigate.\n\n"
                "Archived screenshot references: "
                ".spice/inbox/archive/20260612T043642083543Z.attachments/"
                "01-image.png and "
                ".spice/inbox/archive/20260612T043642083543Z.attachments/"
                "02-image.png.\n\n"
                "Keep the rendered image context stable for reviewers."
            ),
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "serve.ui",
        },
    )

    assert message == (
        "Fix image labels\n\n"
        "Task: TASK-20260101T000000000003Z\n"
        "Task-Phase: todo\n"
        "Task-Project: serve.ui\n"
        f"Task-Session: {ACTOR_A}"
    )


def test_merge_message_uses_fallback_subject_and_trailers_only():
    message = gitsync._compose_message(
        "TASK-20260101T000000000004Z",
        {
            "title": "",
            "description": (
                "Operator steering 20260612T054500966259Z: final task merge "
                "commit bodies currently include the task description, which "
                "can read well but carries too many transient details such as "
                "'operator steering ...' wording and links/paths to .spice "
                "inbox artifacts that will not exist for readers later. Adjust "
                "task completion/merge commit body generation."
            ),
            "actor": ACTOR_A,
            "phase": "todo",
            "project": "task",
        },
    )

    assert message == (
        "Integrate TASK-20260101T000000000004Z\n\n"
        "Task: TASK-20260101T000000000004Z\n"
        "Task-Phase: todo\n"
        "Task-Project: task\n"
        f"Task-Session: {ACTOR_A}"
    )


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


def test_integrate_and_publish_conflict_guides_resolution_and_retry(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    with pytest.raises(gitsync.MergeConflict) as exc_info:
        gitsync.integrate_and_publish("TASK-20260101T000000000002Z", repo_root=repo)

    message = str(exc_info.value)
    assert "README.md" in message
    assert "keep the merge state open" in message
    assert "commit while MERGE_HEAD exists" in message
    assert "git status --short" in message
    assert "git rev-parse --verify MERGE_HEAD" in message
    assert "git add -- README.md" in message
    assert 'spice task done TASK-20260101T000000000002Z --validation "..."' in message
    assert _git(repo, "rev-parse", "--verify", "MERGE_HEAD") == upstream_head
    assert _git(repo, "status", "--porcelain") == "UU README.md"

    (repo / "README.md").write_text("resolved work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(
        repo,
        "git",
        "commit",
        "-m",
        "Resolve baseline overlap for TASK-20260101T000000000002Z",
    )

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000002Z", repo_root=repo
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert captured["done_upstream_head"] == upstream_head
    assert _merge_parents(repo, merge_head)[0] == upstream_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert _git(repo, "status", "--porcelain") == ""


def _durable_artifact_paths(*texts: str) -> list[Path]:
    paths: list[Path] = []
    for text in texts:
        for token in re.split(r"\s+", str(text)):
            candidate = token.strip(".,;:")
            if "/artifacts/attachments/" in candidate:
                paths.append(Path(candidate))
    return paths


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


def _merge_parents(repo: Path, commit: str) -> list[str]:
    line = _git(repo, "rev-list", "--parents", "-n", "1", commit)
    return line.split()[1:]


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
    assigned = ops.next_task()
    assert identity.render_handle(assigned or {}) == handle
    return handle


def _uda_map(args: list[str]) -> dict[str, str]:
    return dict(arg.split(":", 1) for arg in args)


def _git(repo: Path, *args: str) -> str:
    return _run(repo, "git", *args).stdout.strip()


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)


def test_task_add_title_flag_is_alias_for_positional(task_repo, capsys):
    args = build_parser().parse_args(
        ["task", "add", "--title", "Alias title lands", "--project", "task.unit"]
    )

    assert args.func(args) == 0
    created = capsys.readouterr().out.split()[1]
    row = identity.resolve(created)

    assert row["description"] == "Alias title lands"


def test_task_add_takes_exactly_one_title_form(task_repo):
    args = build_parser().parse_args(
        ["task", "add", "Positional title", "--title", "Flag title"]
    )

    with pytest.raises(SpiceError, match="positional title or --title"):
        args.func(args)


def test_gitsync_network_commands_are_noninteractive_and_bounded(tmp_path, monkeypatch):
    seen: dict[str, object] = {}

    def fake_run(command: list[str], **kwargs):
        seen["command"] = command
        seen["env"] = kwargs["env"]
        seen["timeout"] = kwargs.get("timeout")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(gitsync.subprocess, "run", fake_run)

    gitsync._run(tmp_path, "fetch", "origin")

    env = seen["env"]
    assert isinstance(env, dict)
    assert env["GIT_TERMINAL_PROMPT"] == "0"
    assert env["GIT_SSH_COMMAND"] == gitsync.TASK_GIT_SSH_COMMAND
    assert seen["timeout"] == gitsync.GIT_NETWORK_TIMEOUT_SECONDS


def test_gitsync_network_timeout_returns_failed_process(tmp_path, monkeypatch):
    def fake_run(command: list[str], **kwargs):
        raise subprocess.TimeoutExpired(
            command, kwargs["timeout"], output="partial", stderr="stalled"
        )

    monkeypatch.setattr(gitsync.subprocess, "run", fake_run)

    completed = gitsync._run(tmp_path, "fetch", "origin")

    assert completed.returncode == GIT_TIMEOUT_RETURN_CODE
    assert completed.stdout == "partial"
    assert "git fetch timed out after 30s" in completed.stderr


def test_task_oops_description_records_triage_context(task_repo, capsys):
    args = build_parser().parse_args(
        [
            "task",
            "oops",
            "wrapper",
            "hiccup",
            "--description",
            "Longer triage context for the board.",
        ]
    )

    assert args.func(args) == 0
    out = capsys.readouterr().out
    created = re.search(r"OOPS-\S+", out).group(0)
    row = identity.resolve(created)

    assert row["description"] == "wrapper hiccup"
    assert row["task_description"] == "Longer triage context for the board."
    assert row["project"] == config.OOPS_PROJECT


def test_task_oops_accepts_priority_style_severity_shorthand(task_repo, capsys):
    args = build_parser().parse_args(
        ["task", "oops", "wrapper", "hiccup", "--severity", "H"]
    )

    assert args.func(args) == 0
    out = capsys.readouterr().out
    created = re.search(r"OOPS-\S+", out).group(0)
    row = identity.resolve(created)

    assert "[high]" in out
    assert row["priority"] == "H"
    assert "high" in row["tags"]
    assert row["project"] == config.OOPS_PROJECT


def test_task_add_rejects_oops_system_project(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match="reserved for system task creation"):
        ops.add(
            "Manual oops project",
            project=config.OOPS_PROJECT,
            priority="medium",
            acceptance=["oops is system-created only"],
        )


def test_integrate_and_publish_refuses_committed_conflict_markers(tmp_path):
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")

    (repo / "README.md").write_text("agent work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "agent work")

    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "README.md").write_text("baseline work\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md")
    _run(peer, "git", "commit", "-m", "baseline work")
    _run(peer, "git", "push", "origin", "main")
    upstream_head = _git(peer, "rev-parse", "HEAD")

    with pytest.raises(gitsync.MergeConflict):
        gitsync.integrate_and_publish("TASK-20260101T000000000003Z", repo_root=repo)

    conflicted = (repo / "README.md").read_text(encoding="utf-8")
    assert "<<<<<<<" in conflicted
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "Resolve baseline overlap, badly")

    with pytest.raises(SpiceError, match="conflict markers") as exc_info:
        gitsync.integrate_and_publish("TASK-20260101T000000000003Z", repo_root=repo)

    message = str(exc_info.value)
    assert "README.md" in message
    assert "git add -- README.md" in message
    assert "git commit --amend --no-edit" in message
    assert (
        _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == upstream_head
    )

    (repo / "README.md").write_text("resolved work\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "--amend", "--no-edit")

    result = gitsync.integrate_and_publish(
        "TASK-20260101T000000000003Z", repo_root=repo
    )
    captured = _uda_map(result.uda_args)
    merge_head = captured["done_merge_head"]

    assert _merge_parents(repo, merge_head)[0] == upstream_head
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
