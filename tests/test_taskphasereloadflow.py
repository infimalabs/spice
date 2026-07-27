"""Task-state proofs for fresh-checkout phase continuation."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import create, identity, ops, phasecontinuation, tw
from tests.test_tasks import (
    ACTOR_A,
    PEER_ACTOR,
    _make_loose_commit,
    remote_task_repo,
    task_repo,
)

__all__ = ["remote_task_repo", "task_repo"]

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)


def _run(repo: Path, *args: str) -> str:
    result = subprocess.run(
        list(args),
        cwd=repo,
        capture_output=True,
        check=True,
        text=True,
    )
    return result.stdout.strip()


def _commit(repo: Path, message: str) -> str:
    _run(repo, "git", "add", ".")
    _run(repo, "git", "commit", "-m", message)
    return _run(repo, "git", "rev-parse", "HEAD")


def test_exact_done_resume_does_not_duplicate_annotations(task_repo, monkeypatch):
    handle = create.add(
        "Resume one completion exactly",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo"],
        acceptance=["exact continuation replay is annotation-idempotent"],
    )
    ops.claim(handle)
    payload = {
        "handle": handle,
        "actor": ACTOR_A,
        "validation": ["resume validated"],
        "judgment": "sound",
        "notes": ["stable note"],
        "chain_next": False,
        "sync_notes": [],
        "sync_uda_args": [],
    }
    original_advance = ops._advance
    monkeypatch.setattr(
        ops,
        "_advance",
        lambda _row: (_ for _ in ()).throw(SpiceError("injected before advance")),
    )

    with pytest.raises(SpiceError, match="injected before advance"):
        ops._continue_done(payload)

    monkeypatch.setattr(ops, "_advance", original_advance)
    output = ops._continue_done(payload)
    row = identity.resolve(handle)
    annotations = [
        str(item.get("description") or "") for item in row.get("annotations") or []
    ]

    assert f"completed {handle}" in output
    assert annotations.count("stable note") == 1
    assert annotations.count("validation: resume validated") == 1


def test_done_continuation_payload_keeps_its_exact_key_set(task_repo, monkeypatch):
    # The sibling arm of the same protocol. Its hand-built payload elsewhere in
    # this module cannot see the emitter drift, and a required key added here
    # would refuse a live completion after its landing is already authoritative.
    handle = create.add(
        "Pin one completion payload",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo"],
        acceptance=["the completion payload key set is the wire contract"],
    )
    ops.claim(handle)
    captured: dict[str, object] = {}

    def capture(operation, payload, **_kwargs):
        captured.update(operation=operation, payload=payload)
        return "captured"

    monkeypatch.setattr(phasecontinuation, "continue_after_integration", capture)
    assert ops.done(handle, validation=["payload pinned"]) == "captured"
    payload = captured["payload"]

    assert captured["operation"] == "done"
    assert isinstance(payload, dict)
    assert sorted(payload) == [
        "chain_next",
        "handle",
        "judgment",
        "notes",
        "sync_notes",
        "sync_uda_args",
        "validation",
    ]


def test_done_continuation_derives_the_completer_from_durable_claim_state(
    task_repo, monkeypatch
):
    # The shape a process running pre-landing code still emits, carrying the
    # completer key this landing removed. Dropping a key is the safe direction
    # across the checkout seam precisely because the decoder can ignore one,
    # so this payload must complete on the durable claim holder while the
    # stale value rides along unread. A decoder still trusting that value would
    # refuse the completion outright, because it names someone else.
    handle = create.add(
        "Complete from durable claim state",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo"],
        acceptance=["the durable claim holder is the completer of record"],
    )
    ops.claim(handle)
    payload = {
        "handle": handle,
        "actor": PEER_ACTOR,
        "validation": ["derived completer verified"],
        "judgment": "sound",
        "notes": [],
        "chain_next": False,
        "sync_notes": [],
        "sync_uda_args": [],
    }

    output = ops._continue_done(payload)
    row = identity.resolve(handle)

    assert f"completed {handle}" in output
    assert row["status"] == "completed"
    assert row["validation"] == "derived completer verified"


def test_exact_review_resume_reuses_prepared_followup(task_repo, monkeypatch):
    handle = create.add(
        "Resume one review exactly",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["exact continuation replay preserves follow-up identity"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["todo complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    captured: dict[str, object] = {}

    def capture(operation, payload, **_kwargs):
        captured.update(operation=operation, payload=payload)
        return "captured"

    monkeypatch.setattr(phasecontinuation, "continue_after_integration", capture)
    assert (
        ops.review(
            handle,
            finding="clean",
            note="stable review",
            then=[
                "title=Stable follow-up | project=task.unit | "
                "acceptance=Created exactly once"
            ],
        )
        == "captured"
    )
    assert captured["operation"] == "review"
    payload = captured["payload"]
    assert isinstance(payload, dict)
    # The payload is decoded by whichever checkout the landing selected, so its
    # exact key set is the wire contract. This is the shape 987f38b1 emitted,
    # before the reload landing made a reviewer and a timestamp required and
    # refused a live review mid-flight; pinning it fails if a key returns.
    assert sorted(payload) == [
        "creation_surface",
        "finding",
        "followup",
        "handle",
        "note",
        "prepared_followups",
        "sync_uda_args",
    ]
    original_advance = ops._advance
    monkeypatch.setattr(
        ops,
        "_advance",
        lambda _row: (_ for _ in ()).throw(SpiceError("injected after follow-up")),
    )

    with pytest.raises(SpiceError, match="injected after follow-up"):
        ops._continue_review(payload)
    first_reviewed_at = identity.resolve(handle)["review_at"]

    monkeypatch.setattr(ops, "_advance", original_advance)
    output = ops._continue_review(payload)
    children = [
        row for row in tw.export() if row.get("description") == "Stable follow-up"
    ]
    reviewed = identity.resolve(handle)
    annotations = [
        str(item.get("description") or "") for item in reviewed.get("annotations") or []
    ]

    assert f"reviewed {handle} clean; completed {handle}" in output
    assert len(children) == 1
    assert reviewed["review_at"] == first_reviewed_at
    assert reviewed["review_by"] == PEER_ACTOR
    assert annotations.count(f"review: finding=clean; by={PEER_ACTOR}") == 1


def test_review_continuation_time_follows_the_recorded_verdict(task_repo, monkeypatch):
    handle = create.add(
        "Stamp one review time per verdict",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["the recorded verdict decides the review time"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["todo complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)
    captured: dict[str, object] = {}

    def capture(operation, payload, **_kwargs):
        captured.update(operation=operation, payload=payload)
        return "captured"

    monkeypatch.setattr(phasecontinuation, "continue_after_integration", capture)
    ops.review(handle, finding="clean", note="first pass")
    payload = captured["payload"]
    assert isinstance(payload, dict)
    monkeypatch.setattr(
        ops,
        "_advance",
        lambda _row: (_ for _ in ()).throw(SpiceError("injected after finding")),
    )

    with pytest.raises(SpiceError, match="injected after finding"):
        ops._continue_review(payload)
    first = identity.resolve(handle)["review_at"]

    with pytest.raises(SpiceError, match="injected after finding"):
        ops._continue_review(payload)
    replayed = identity.resolve(handle)["review_at"]

    with pytest.raises(SpiceError, match="injected after finding"):
        ops._continue_review(dict(payload, finding="changes"))
    reverdicted = identity.resolve(handle)

    assert replayed == first
    assert reverdicted["review_finding"] == "changes"
    assert reverdicted["review_at"] != first


def test_head_moving_done_next_claims_follow_on_from_the_fresh_process(
    remote_task_repo,
):
    current = create.add(
        "Land before fresh chained allocation",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo"],
        acceptance=["HEAD-moving completion reloads before allocation"],
    )
    follow_on = create.add(
        "Claimed by fresh chained allocation",
        project="task.unit",
        origin="ack:1jN54zJJ",
        flow=["todo"],
        acceptance=["fresh allocation retains ordinary ownership"],
    )
    ops.claim(current)
    landed_work = _make_loose_commit(
        remote_task_repo,
        name="reload.txt",
        subject="head-moving task work",
    )

    output = ops.done(
        current,
        validation=["fresh chained allocation verified"],
        judgment="fresh checkout retained judgment",
        notes=["fresh checkout retained note"],
        chain_next=True,
    )
    completed = identity.resolve(current)

    assert f"completed {current}" in output
    assert "next task:" in output
    assert follow_on in output
    assert identity.resolve(follow_on)["claim_by"] == ACTOR_A
    assert completed["done_head"] == landed_work
    assert completed["judgment"] == "fresh checkout retained judgment"
    assert any(
        item.get("description") == "fresh checkout retained note"
        for item in completed.get("annotations") or []
    )


def test_head_moving_review_completes_from_the_fresh_process(
    remote_task_repo, tmp_path, monkeypatch
):
    handle = create.add(
        "Review after a concurrent landing",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["HEAD-moving review reloads before task mutation"],
    )
    ops.claim(handle)
    ops.done(handle, validation=["todo phase complete"])
    monkeypatch.setenv(DRIVER.thread_id_env, PEER_ACTOR)
    ops.claim(handle)

    remote = _run(remote_task_repo, "git", "remote", "get-url", "origin")
    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", remote, str(peer))
    _run(peer, "git", "config", "user.email", "test@example.com")
    _run(peer, "git", "config", "user.name", "Test")
    (peer / "peer.txt").write_text("concurrent landing\n", encoding="utf-8")
    _commit(peer, "concurrent landing")
    _run(peer, "git", "push", "origin", "main")
    peer_head = _run(peer, "git", "rev-parse", "HEAD")

    output = ops.review(
        handle,
        finding="clean",
        note="fresh review verified",
        then=[
            "title=Fresh review follow-up | project=task.unit | "
            "acceptance=Crossed the reload seam once"
        ],
    )
    row = identity.resolve(handle)
    followups = [
        task
        for task in tw.export()
        if task.get("description") == "Fresh review follow-up"
    ]

    assert f"reviewed {handle} clean; completed {handle}" in output
    assert row["status"] == "completed"
    assert row["review_finding"] == "clean"
    assert row["review_note"] == "fresh review verified"
    assert len(followups) == 1
    assert _run(remote_task_repo, "git", "rev-parse", "HEAD") == peer_head
