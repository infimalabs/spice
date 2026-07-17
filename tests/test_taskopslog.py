"""Contract-field mutation notices from the TaskChampion operations log."""

from __future__ import annotations

import json
import shutil
import sqlite3
import subprocess
from pathlib import Path

import pytest

from spice.agent import lifecycle, watchdog
from spice.agent.driver import DRIVER
from spice.tasks import claimstate, config, create, identity, ops, opslog, render, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


@pytest.fixture(params=["task-backend", "task backend ?%"])
def task_repo(tmp_path, monkeypatch, request):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / request.param
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_read_only_connector_encodes_every_uri_reserved_path_character(
    tmp_path, monkeypatch
):
    data_dir = tmp_path / "task data ?#%"
    data_dir.mkdir()
    database = data_dir / opslog.OPERATIONS_DB_FILENAME
    uuid = "11111111-1111-1111-1111-111111111111"
    con = sqlite3.connect(database)
    try:
        con.execute("CREATE TABLE operations (id INTEGER, uuid TEXT, data TEXT)")
        con.executemany(
            "INSERT INTO operations VALUES (?, ?, ?)",
            [
                (
                    1,
                    uuid,
                    json.dumps({"Update": {"property": "claim_by", "value": ACTOR_A}}),
                ),
                (
                    2,
                    uuid,
                    json.dumps({"Create": {"uuid": uuid}}),
                ),
                (
                    3,
                    uuid,
                    json.dumps(
                        {
                            "Update": {
                                "property": "acceptance",
                                "old_value": "old",
                                "value": "new",
                                "timestamp": "now",
                            }
                        }
                    ),
                ),
            ],
        )
        con.commit()
    finally:
        con.close()
    monkeypatch.setattr(config, "data_dir", lambda: data_dir)

    baseline = opslog.claim_baseline_id(uuid, ACTOR_A)
    cursor, mutations = opslog.contract_mutations_since(uuid, baseline)

    assert opslog.operations_db_uri(database.resolve()) == (
        f"{database.resolve().as_uri()}?mode=ro"
    )
    assert opslog.task_version(uuid) == 3
    assert baseline == 1
    assert cursor == 3
    assert mutations == [opslog.ContractMutation("acceptance", "old", "new", "now")]


def test_connector_opens_the_one_resolved_database(tmp_path, monkeypatch):
    uuid = "11111111-1111-1111-1111-111111111111"
    databases = [tmp_path / "resolved.sqlite3", tmp_path / "later.sqlite3"]
    for operation_id, database in zip((1, 9), databases, strict=True):
        con = sqlite3.connect(database)
        try:
            con.execute("CREATE TABLE operations (id INTEGER, uuid TEXT, data TEXT)")
            con.execute(
                "INSERT INTO operations VALUES (?, ?, ?)",
                (operation_id, uuid, json.dumps({"Update": {"uuid": uuid}})),
            )
            con.commit()
        finally:
            con.close()

    resolved: list[Path] = []

    def alternating_backend() -> Path:
        path = databases[len(resolved)]
        resolved.append(path)
        return path

    monkeypatch.setattr(opslog, "operations_db_path", alternating_backend)

    assert opslog.task_version(uuid) == 1
    assert resolved == [databases[0]]


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


def test_supervisor_notice_reports_one_claimed_project_move(task_repo, monkeypatch):
    handle = _claimed_task(priority="L")
    calls: list[tuple[str, dict[str, object]]] = []
    monkeypatch.setattr(
        watchdog,
        "publish_supervisor_feedback",
        lambda _repo, _log, kind, **fields: calls.append((kind, fields)),
    )
    log_path = task_repo / "supervisor.log"
    cursors: dict[str, int] = {}
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors)

    ops.edit(handle, project="serve.unit")
    moved_handle = identity.render_handle(identity.resolve(handle))
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors)
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors)

    assert [(kind, fields["fields"], fields["detail"]) for kind, fields in calls] == [
        (
            "claim.contract-changed",
            "project",
            "project: task.unit -> serve.unit",
        )
    ]
    assert log_path.read_text(encoding="utf-8") == (
        f"spice claim contract changed: {moved_handle} "
        "project: task.unit -> serve.unit\n"
    )


def test_show_version_equals_ops_log_tail_and_edit_increases_it(task_repo):
    handle = _claimed_task(priority="L")
    uuid = identity.uuid_of(identity.resolve(handle))

    shown = _shown_version(handle)
    database = opslog.operations_db_path()
    con = sqlite3.connect(opslog.operations_db_uri(database), uri=True)
    try:
        tail = con.execute(
            "SELECT MAX(id) FROM operations WHERE uuid = ?", (uuid,)
        ).fetchone()[0]
    finally:
        con.close()
    assert shown == int(tail)
    assert shown > 0

    ops.edit(handle, acceptance=["sharpened criterion"])
    assert _shown_version(handle) > shown


def _shown_version(handle: str) -> int:
    for line in render.render_show(handle).splitlines():
        if line.startswith("version "):
            return int(line.split()[1])
    raise AssertionError("version row missing from task show output")


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
