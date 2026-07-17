"""Contract-field mutation notices from the TaskChampion operations log."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import lifecycle, watchdog
from spice.agent.driver import DRIVER
from spice.tasks import claimstate, config, create, identity, ops, opslog, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


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


def test_contract_mutations_track_edits_exactly(task_repo):
    handle = _claimed_task(priority="L")
    uuid = identity.uuid_of(identity.resolve(handle))
    baseline = opslog.claim_baseline_id(uuid, ACTOR_A)

    ops.edit(handle, acceptance=["sharpened criterion"])
    ops.edit(handle, description="claim probe body rewritten")
    tw.run([uuid, "modify", "priority:H"])

    cursor, mutations = opslog.contract_mutations_since(uuid, baseline)
    assert [(item.property, item.old_value, item.new_value) for item in mutations] == [
        ("acceptance", "initial criterion", "sharpened criterion"),
        ("task_description", "", "claim probe body rewritten"),
        ("priority", "L", "H"),
    ]
    assert cursor > baseline


def test_renewal_cycle_reports_no_contract_mutations(task_repo):
    handle = _claimed_task(priority="L")
    uuid = identity.uuid_of(identity.resolve(handle))
    baseline = opslog.claim_baseline_id(uuid, ACTOR_A)

    result = claimstate.renew_claim()
    cursor, mutations = opslog.contract_mutations_since(uuid, baseline)

    assert result.renewed
    assert result.uuid == uuid
    assert mutations == []
    assert cursor > baseline


def test_claim_baseline_reports_only_post_claim_edits(task_repo):
    handle = create.add(
        "Baseline probe task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["initial criterion"],
        priority="L",
    )
    ops.edit(handle, acceptance=["pre-claim rewrite"])
    ops.claim(handle)
    ops.edit(handle, acceptance=["post-claim rewrite"])

    uuid = identity.uuid_of(identity.resolve(handle))
    baseline = opslog.claim_baseline_id(uuid, ACTOR_A)
    cursor, mutations = opslog.contract_mutations_since(uuid, baseline)

    assert [(item.property, item.old_value, item.new_value) for item in mutations] == [
        ("acceptance", "pre-claim rewrite", "post-claim rewrite")
    ]
    assert cursor > baseline


def test_supervisor_notice_names_changed_fields_and_renotices(task_repo, monkeypatch):
    handle = _claimed_task(priority="L")
    uuid = identity.uuid_of(identity.resolve(handle))
    calls: list[tuple[str, dict[str, object]]] = []
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda _repo, _log, kind, **fields: calls.append((kind, fields)),
    )
    log_path = task_repo / "supervisor.log"
    cursors: dict[str, int] = {}

    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors)
    quiet_pass = list(calls)

    ops.edit(handle, acceptance=["sharpened criterion"])
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors)

    tw.run([uuid, "modify", "priority:H"])
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors)

    assert quiet_pass == []
    assert [
        (kind, fields["handle"], fields["fields"], fields["detail"])
        for kind, fields in calls
    ] == [
        (
            "claim.contract-changed",
            handle,
            "acceptance",
            "acceptance: initial criterion -> sharpened criterion",
        ),
        ("claim.contract-changed", handle, "priority", "priority: L -> H"),
    ]
    assert log_path.read_text(encoding="utf-8") == (
        f"spice claim contract changed: {handle} "
        "acceptance: initial criterion -> sharpened criterion\n"
        f"spice claim contract changed: {handle} priority: L -> H\n"
    )


def test_render_notice_compacts_long_values():
    long_value = " ".join(["word"] * 40)
    preview = long_value[: opslog.VALUE_PREVIEW_CHARS - 1] + "…"
    notice = opslog.render_notice(
        [opslog.ContractMutation("description", "", long_value, "ts")]
    )
    assert notice == f"description: - -> {preview}"


def _claimed_task(*, priority: str) -> str:
    handle = create.add(
        "Opslog probe task",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        acceptance=["initial criterion"],
        priority=priority,
    )
    ops.claim(handle)
    return handle


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
