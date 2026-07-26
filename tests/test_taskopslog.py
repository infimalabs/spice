"""Contract-field mutation notices from the TaskChampion operations log."""

from __future__ import annotations

import json
import shutil
import sqlite3
from pathlib import Path

import pytest

from spice.agent import lifecycle, watchdog
from spice.agent.driver import DRIVER
from spice.tasks import claimstate, config, create, identity, ops, opslog, render, tw
from tests.test_reposcaffolding import init_committed_repo as _init_repo

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
# One recorded instant, stated as the log's own text and as the epoch seconds a
# freshness reader compares against now. Both are written down rather than
# derived from each other, so the conversion under test has nothing to agree
# with but the answer.
RECORDED_STAMP = "2026-07-26T19:51:45.291080Z"
RECORDED_EPOCH = 1785095505.29108
ZONELESS_STAMP = "2026-07-26T19:51:45.291080"
CRAFTED_UUID = "11111111-1111-1111-1111-111111111111"


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


def test_freshness_holds_still_across_a_read_that_rewrites_the_store(task_repo):
    """A Taskwarrior read writes the store, so the store cannot date the log.

    The export changes nothing and still moves every timestamp the filesystem
    keeps for the database, which is what the mtime assertion here pins down.
    Freshness read from the log's own operations is unmoved by that and moves
    for the modification, so it dates the authority rather than the last time
    anyone ran a command against it.
    """
    handle = _claimed_task(priority="L")
    store = opslog.operations_db_path()

    claimed = opslog.latest_operation_epoch()
    claimed_mtime = store.stat().st_mtime_ns
    tw.export(["status.any:"])
    read = opslog.latest_operation_epoch()
    read_mtime = store.stat().st_mtime_ns
    ops.modify(handle, acceptance=["sharpened criterion"])
    modified = opslog.latest_operation_epoch()

    assert read_mtime != claimed_mtime
    assert read == claimed
    assert modified > read


def test_freshness_reads_past_the_separator_that_closes_a_transaction(
    tmp_path, monkeypatch
):
    """TaskChampion's transaction separator carries no instant of its own."""
    data_dir = _crafted_log(
        tmp_path,
        [
            json.dumps(
                {
                    "Update": {
                        "uuid": CRAFTED_UUID,
                        "property": "status",
                        "value": "pending",
                        "timestamp": RECORDED_STAMP,
                    }
                }
            ),
            json.dumps("UndoPoint"),
        ],
    )
    monkeypatch.setattr(config, "data_dir", lambda: data_dir)

    assert opslog.latest_operation_epoch() == RECORDED_EPOCH


def test_freshness_declines_a_stamp_that_names_no_zone(tmp_path, monkeypatch):
    """A zoneless stamp read as local time would be wrong by whole hours."""
    data_dir = _crafted_log(
        tmp_path,
        [
            json.dumps(
                {
                    "Update": {
                        "uuid": CRAFTED_UUID,
                        "property": "status",
                        "value": "pending",
                        "timestamp": ZONELESS_STAMP,
                    }
                }
            )
        ],
    )
    monkeypatch.setattr(config, "data_dir", lambda: data_dir)

    assert opslog.latest_operation_epoch() is None


def _crafted_log(tmp_path: Path, operations: list[str]) -> Path:
    """Write one operations log holding exactly the given operation payloads."""
    data_dir = tmp_path / "crafted"
    data_dir.mkdir()
    con = sqlite3.connect(data_dir / opslog.OPERATIONS_DB_FILENAME)
    try:
        con.execute("CREATE TABLE operations (id INTEGER, uuid TEXT, data TEXT)")
        con.executemany(
            "INSERT INTO operations VALUES (?, ?, ?)",
            [
                (operation_id, CRAFTED_UUID, data)
                for operation_id, data in enumerate(operations, start=1)
            ],
        )
        con.commit()
    finally:
        con.close()
    return data_dir


def test_contract_mutations_track_modifications_exactly(task_repo):
    handle = _claimed_task(priority="L")
    uuid = identity.uuid_of(identity.resolve(handle))
    baseline = opslog.claim_baseline_id(uuid, ACTOR_A)

    ops.modify(handle, title="Retitled opslog probe")
    ops.modify(handle, acceptance=["sharpened criterion"])
    ops.modify(handle, description="claim probe body rewritten")
    tw.run([uuid, "modify", "priority:H"])

    cursor, mutations = opslog.contract_mutations_since(uuid, baseline)
    assert [(item.property, item.old_value, item.new_value) for item in mutations] == [
        ("description", "Opslog probe task", "Retitled opslog probe"),
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


def test_claim_baseline_reports_only_post_claim_modifications(task_repo):
    handle = create.add(
        "Baseline probe task",
        project="task.unit",
        origin="ack:1jN54zJJ",
        acceptance=["initial criterion"],
        priority="L",
    )
    ops.modify(handle, acceptance=["pre-claim rewrite"])
    ops.claim(handle)
    ops.modify(handle, acceptance=["post-claim rewrite"])

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
    held: dict[str, str] = {}

    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors, held)
    quiet_pass = list(calls)

    ops.modify(handle, acceptance=["sharpened criterion"])
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors, held)

    tw.run([uuid, "modify", "priority:H"])
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors, held)

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
    held: dict[str, str] = {}
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors, held)

    ops.modify(handle, project="serve.unit")
    moved_handle = identity.render_handle(identity.resolve(handle))
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors, held)
    lifecycle._renew_supervised_claim(task_repo, ACTOR_A, log_path, {}, cursors, held)

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


def test_show_version_equals_ops_log_tail_and_modify_increases_it(task_repo):
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

    ops.modify(handle, acceptance=["sharpened criterion"])
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
        origin="ack:1jN54zJJ",
        acceptance=["initial criterion"],
        priority=priority,
    )
    ops.claim(handle)
    return handle
