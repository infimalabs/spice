"""Revision-coherent Serve task-board observation tests."""

from __future__ import annotations

import gc
import os
import threading
import weakref
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from types import MappingProxyType

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.serve import taskboard
from spice.serve.payload import message
from spice.serve.worktree import inventory as worktree_inventory
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import config as task_config, create
from spice.tasks.opslog import OPERATIONS_DB_FILENAME
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _serve_state,
    _target,
    _task_cards,
)

# A board revision is the generation its authority minted, so these fixtures
# carry counts rather than labels: the chrome producer publishes an epoch only
# where it could have counted forward from it.
CROSSING_REVISION = "1785044000000100"
MEASURED_GENERATION = "1785044000000200"
# One board every lane read in a message payload answers: an origin-owned card
# row, the lane's active claim, a completed review carrying a finding, and a
# second completed row the lane drained. Rendering all four from one export is
# what proves the payload crossed onto the shared observation.
CROSSING_ROWS = (
    {
        "id": 1,
        "uuid": "card-row",
        "incepted": "1kGsk2S1",
        "description": "Cross the shared observation",
        "project": "serve.latency",
        "origin": "ack:1jN54zJJ",
        "origin_thread": THREAD_A,
        "creation_surface": "cli",
        "status": "pending",
    },
    {
        "id": 2,
        "uuid": "claim-row",
        "incepted": "1kGsk2S2",
        "description": "Held by the lane",
        "project": "serve.latency",
        "phase": "todo",
        "claim_by": THREAD_A,
        "claim_at": "2026-07-25T20:00:00Z",
        "start": "20260725T200000Z",
        "status": "pending",
    },
    {
        "id": 3,
        "uuid": "review-row",
        "incepted": "1kGsk2S3",
        "description": "Reviewed with findings",
        "project": "serve.latency",
        "status": "completed",
        "review_author": THREAD_A,
        "review_by": "peer-thread",
        "review_finding": "changes",
        "review_at": "2026-07-25T21:00:00Z",
    },
    {
        "id": 4,
        "uuid": "drained-row",
        "incepted": "1kGsk2S4",
        "description": "Drained by the lane",
        "project": "serve.latency",
        "status": "completed",
        "claim_by": THREAD_A,
    },
)
MEASURED_BOARD_ROW_COUNT = 1_330


@pytest.fixture(autouse=True)
def _clear_task_board_observations():
    with taskboard._task_board_condition:
        taskboard._task_board_observations.clear()
        taskboard._task_board_builds.clear()
    yield
    with taskboard._task_board_condition:
        taskboard._task_board_observations.clear()
        taskboard._task_board_builds.clear()


# The export the store is swapped under, and the total once the discarded
# candidate has been rebuilt against the store that replaced it.
FIRST_EXPORT = 1
RETRIED_EXPORTS = 2
# Rows one export returns from a store holding a single seeded task.
KEPT_TASK_ROW_COUNT = 1


def _real_backend(root: Path, *titles: str) -> Path:
    """Materialize a real Taskwarrior backend and seed it with real tasks.

    The store these tests care about is created by Taskwarrior itself, not by
    materialization, so the export at the end is what guarantees a valid
    TaskChampion database exists on disk even when no task was added.
    """
    taskrc = task_config.materialize_task_backend(root)
    for title in titles:
        taskboard.tw.run(["add", title], taskrc=taskrc)
    taskboard.tw.export(["status.any:"], taskrc=taskrc)
    return taskrc


def _store_path(root: Path) -> Path:
    return task_config.data_dir(root) / OPERATIONS_DB_FILENAME


def _replace_store(source_root: Path, target_root: Path) -> None:
    """Rename one real store over another, as an atomic swap under a live path.

    Any write-ahead log beside the target belongs to the store being displaced,
    so it goes with it; leaving it would let SQLite recover the replaced store's
    pages into the replacement.
    """
    for suffix in ("-wal", "-shm"):
        sidecar = _store_path(target_root).with_name(
            _store_path(target_root).name + suffix
        )
        if sidecar.exists():
            sidecar.unlink()
    os.replace(_store_path(source_root), _store_path(target_root))


def _recycled_stat(stat: os.stat_result, *, onto: os.stat_result) -> os.stat_result:
    """Report one store's times under another store's device and inode number.

    This is the filesystem behaviour the store witness has to survive: a number
    freed by an unlinked file handed straight back to the file that replaces it,
    which makes the replacement indistinguishable from the original by name.

    A hand-assembled `os.stat_result` also reports no creation time, since the
    field is left unfilled rather than measured. So what these stats describe is
    a store on a platform that records no creation time at all -- the case the
    retired `st_birthtime` witness silently reduced to device and inode.
    """
    return os.stat_result(
        (
            stat.st_mode,
            onto.st_ino,
            onto.st_dev,
            stat.st_nlink,
            stat.st_uid,
            stat.st_gid,
            stat.st_size,
            stat.st_atime,
            stat.st_mtime,
            stat.st_ctime,
        ),
        {
            "st_atime_ns": stat.st_atime_ns,
            "st_mtime_ns": stat.st_mtime_ns,
            "st_ctime_ns": stat.st_ctime_ns,
        },
    )


def _descriptions(observation) -> list[str]:
    return sorted(str(row["description"]) for row in observation.rows)


def _stub_backend(monkeypatch, revision):
    monkeypatch.setattr(task_config, "task_event_revision", revision)
    monkeypatch.setattr(
        task_config,
        "materialize_task_backend",
        lambda root: root / "taskrc",
    )


def _stub_crossing_board(monkeypatch, tmp_path) -> list[list[str]]:
    """Answer every task read from one stubbed export of ``CROSSING_ROWS``."""
    monkeypatch.setenv(task_config.TASK_BACKEND_ENV, str(tmp_path / "task-backend"))
    _stub_backend(monkeypatch, lambda root: CROSSING_REVISION)
    exports: list[list[str]] = []

    def export(filters, *, taskrc):
        exports.append(list(filters))
        return [dict(row) for row in CROSSING_ROWS]

    monkeypatch.setattr(taskboard.tw, "export", export)
    return exports


def _task_board(payload: dict) -> dict:
    return payload["chrome"]["taskBoard"]["value"]


def _task_derived_slice(payload: dict) -> tuple:
    return (
        _task_board(payload)["taskFilterInventory"],
        payload["statusLine"]["claimedTask"],
        payload["laneInfo"]["reviewPressure"],
        _task_cards(payload),
    )


def _measured_board_rows() -> tuple[dict[str, object], ...]:
    """Reproduce the measured board's row count without timing assertions."""
    filler = tuple(
        {
            "id": index + len(CROSSING_ROWS) + 1,
            "uuid": f"filler-{index:04d}",
            "description": f"Stable board row {index}",
            "project": "serve.latency",
            "status": "pending",
        }
        for index in range(MEASURED_BOARD_ROW_COUNT - len(CROSSING_ROWS))
    )
    return (*CROSSING_ROWS, *filler)


def test_stable_revision_exports_and_normalizes_each_row_once(monkeypatch, tmp_path):
    _stub_backend(monkeypatch, lambda root: "41")
    source_rows = [{"uuid": "one"}, {"uuid": "two"}]
    export_calls: list[tuple[list[str], Path]] = []
    normalized: list[str] = []
    original_normalize = taskboard._normalize_task_row

    def export(filters, *, taskrc):
        export_calls.append((filters, taskrc))
        return source_rows

    def normalize(row):
        normalized.append(str(row["uuid"]))
        return original_normalize(row)

    monkeypatch.setattr(taskboard.tw, "export", export)
    monkeypatch.setattr(taskboard, "_normalize_task_row", normalize)

    first = taskboard.current_task_board_observation(backend_root=tmp_path)
    second = taskboard.current_task_board_observation(backend_root=tmp_path)

    assert first is second
    assert first.revision == "41"
    assert first.error is None
    assert [row["uuid"] for row in first.rows] == ["one", "two"]
    assert all(isinstance(row, MappingProxyType) for row in first.rows)
    assert export_calls == [(["status.any:"], tmp_path / "taskrc")]
    assert normalized == ["one", "two"]
    source_rows[0]["uuid"] = "changed"
    assert first.rows[0]["uuid"] == "one"


def test_concurrent_first_readers_share_one_observation(monkeypatch, tmp_path):
    _stub_backend(monkeypatch, lambda root: "52")
    export_started = threading.Event()
    release_export = threading.Event()
    export_calls = 0
    normalized: list[str] = []
    original_normalize = taskboard._normalize_task_row

    def export(filters, *, taskrc):
        nonlocal export_calls
        export_calls += 1
        export_started.set()
        assert release_export.wait(timeout=5)
        return [{"uuid": "one"}, {"uuid": "two"}]

    def normalize(row):
        normalized.append(str(row["uuid"]))
        return original_normalize(row)

    monkeypatch.setattr(taskboard.tw, "export", export)
    monkeypatch.setattr(taskboard, "_normalize_task_row", normalize)

    with ThreadPoolExecutor(max_workers=8) as pool:
        futures = [
            pool.submit(
                taskboard.current_task_board_observation,
                backend_root=tmp_path,
            )
            for _ in range(8)
        ]
        try:
            assert export_started.wait(timeout=5)
        finally:
            release_export.set()
        observations = [future.result(timeout=5) for future in futures]

    assert export_calls == 1
    assert normalized == ["one", "two"]
    assert all(observation is observations[0] for observation in observations)


def test_measured_board_moves_once_across_inventory_messages_and_metrics(
    monkeypatch, tmp_path
):
    """One 1,330-row observation serves every stable-revision Serve surface."""
    repo = _repo(tmp_path)
    target = _target(repo)
    second_target = WorktreeTarget(
        id="target-2",
        repo_root=repo,
        name=repo.name,
        branch="main",
    )
    state = _serve_state(tmp_path, target)
    state.cached_targets = [target, second_target]
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

    backend = tmp_path / "task-backend"
    monkeypatch.setenv(task_config.TASK_BACKEND_ENV, str(backend))
    _stub_backend(monkeypatch, lambda root: MEASURED_GENERATION)
    source_rows = _measured_board_rows()
    export_started = threading.Event()
    release_export = threading.Event()
    exports: list[list[str]] = []
    normalized: list[str] = []
    original_normalize = taskboard._normalize_task_row

    def export(filters, *, taskrc):
        exports.append(list(filters))
        export_started.set()
        assert release_export.wait(timeout=5)
        return [dict(row) for row in source_rows]

    def normalize(row):
        normalized.append(str(row["uuid"]))
        return original_normalize(row)

    monkeypatch.setattr(taskboard.tw, "export", export)
    monkeypatch.setattr(taskboard, "_normalize_task_row", normalize)

    with ThreadPoolExecutor(max_workers=8) as pool:
        futures = [
            pool.submit(
                taskboard.current_task_board_observation,
                backend_root=backend,
            )
            for _ in range(8)
        ]
        try:
            assert export_started.wait(timeout=5)
        finally:
            release_export.set()
        first_readers = [future.result(timeout=5) for future in futures]

    inventory_payload = worktree_inventory.work_trees_payload(state)
    first_message = message.messages_payload_for_worktree(state, target, limit=5)
    repeated_message = message.messages_payload_for_worktree(state, target, limit=5)
    metrics = message.lane_metrics_summary_payload(state, target)

    assert len(inventory_payload["workTrees"]) == 2
    assert all(observation is first_readers[0] for observation in first_readers)
    assert exports == [["status.any:"]]
    assert len(normalized) == MEASURED_BOARD_ROW_COUNT
    assert len(set(normalized)) == MEASURED_BOARD_ROW_COUNT
    assert (
        _task_board(inventory_payload["workTrees"][0])["taskFilterInventory"]
        == _task_board(first_message)["taskFilterInventory"]
    )
    assert "taskFilterInventory" not in inventory_payload
    assert "taskFilterInventory" not in first_message
    assert _task_derived_slice(repeated_message) == _task_derived_slice(first_message)
    assert metrics["drained"] == 2


def test_coalesced_reader_wait_is_bounded(monkeypatch, tmp_path):
    _stub_backend(monkeypatch, lambda root: "53")
    identity = str(tmp_path.resolve())
    moments = iter(
        [
            0.0,
            taskboard.TASK_BOARD_OBSERVATION_TIMEOUT_SECONDS + 1.0,
        ]
    )
    monkeypatch.setattr(taskboard.time, "monotonic", lambda: next(moments))
    with taskboard._task_board_condition:
        taskboard._task_board_builds.add(identity)

    observation = taskboard.current_task_board_observation(backend_root=tmp_path)

    assert observation.rows == ()
    assert observation.error == "timed out waiting for the current task board"


def test_revision_change_discards_candidate_and_retries(monkeypatch, tmp_path):
    current_revision = "61"
    export_calls = 0

    def revision(root):
        return current_revision

    def export(filters, *, taskrc):
        nonlocal current_revision, export_calls
        export_calls += 1
        if export_calls == 1:
            current_revision = "62"
            return [{"uuid": "stale"}]
        return [{"uuid": "current"}]

    _stub_backend(monkeypatch, revision)
    monkeypatch.setattr(taskboard.tw, "export", export)

    observation = taskboard.current_task_board_observation(backend_root=tmp_path)

    assert export_calls == 2
    assert observation.revision == "62"
    assert [row["uuid"] for row in observation.rows] == ["current"]


def test_revision_churn_cannot_retry_the_observation_forever(monkeypatch, tmp_path):
    revisions = iter(("61", "61", "62"))
    moments = iter(
        [
            0.0,
            taskboard.TASK_BOARD_OBSERVATION_TIMEOUT_SECONDS + 1.0,
        ]
    )
    monkeypatch.setattr(
        taskboard.task_config,
        "task_event_revision",
        lambda root: next(revisions),
    )
    monkeypatch.setattr(taskboard, "_read_task_board", lambda root: [{"uuid": "old"}])
    monkeypatch.setattr(taskboard.time, "monotonic", lambda: next(moments))

    observation = taskboard.current_task_board_observation(backend_root=tmp_path)

    assert observation.revision == "62"
    assert observation.rows == ()
    assert observation.error == "timed out building the current task board"

    monkeypatch.setattr(
        taskboard.task_config,
        "task_event_revision",
        lambda root: "62",
    )
    monkeypatch.setattr(taskboard, "_read_task_board", lambda root: [{"uuid": "fresh"}])
    monkeypatch.setattr(taskboard.time, "monotonic", lambda: 0.0)
    recovered = taskboard.current_task_board_observation(backend_root=tmp_path)

    assert [row["uuid"] for row in recovered.rows] == ["fresh"]


def test_backend_failure_is_empty_and_retryable_at_same_revision(monkeypatch, tmp_path):
    _stub_backend(monkeypatch, lambda root: "71")
    export_calls = 0

    def export(filters, *, taskrc):
        nonlocal export_calls
        export_calls += 1
        if export_calls == 1:
            raise SpiceError("backend unavailable")
        return [{"uuid": "recovered"}]

    monkeypatch.setattr(taskboard.tw, "export", export)

    failed = taskboard.current_task_board_observation(backend_root=tmp_path)
    recovered = taskboard.current_task_board_observation(backend_root=tmp_path)

    assert failed.rows == ()
    assert failed.error == "backend unavailable"
    assert recovered.error is None
    assert [row["uuid"] for row in recovered.rows] == ["recovered"]
    assert export_calls == 2


def test_backend_identity_isolates_observations(monkeypatch, tmp_path):
    backend_a = tmp_path / "a"
    backend_b = tmp_path / "b"
    _stub_backend(monkeypatch, lambda root: "81")
    export_calls: list[Path] = []

    def export(filters, *, taskrc):
        export_calls.append(taskrc)
        return [{"uuid": taskrc.parent.name}]

    monkeypatch.setattr(taskboard.tw, "export", export)

    first_a = taskboard.current_task_board_observation(backend_root=backend_a)
    first_b = taskboard.current_task_board_observation(backend_root=backend_b)
    second_a = taskboard.current_task_board_observation(backend_root=backend_a)

    assert first_a is second_a
    assert first_a.backend_identity != first_b.backend_identity
    assert first_a.rows[0]["uuid"] == "a"
    assert first_b.rows[0]["uuid"] == "b"
    assert export_calls == [backend_a / "taskrc", backend_b / "taskrc"]


def test_team_event_reuses_task_observation(monkeypatch, tmp_path):
    backend = tmp_path / "backend"
    task_config.ensure_task_event_file(backend)
    task_revision = task_config.task_event_revision(backend)
    monkeypatch.setattr(
        task_config,
        "materialize_task_backend",
        lambda root: root / "taskrc",
    )
    export_calls = 0

    def export(filters, *, taskrc):
        nonlocal export_calls
        export_calls += 1
        return [{"uuid": "one"}]

    monkeypatch.setattr(taskboard.tw, "export", export)

    first = taskboard.current_task_board_observation(backend_root=backend)
    task_config.mark_task_backend_changed("team", root=backend)
    second = taskboard.current_task_board_observation(backend_root=backend)

    assert task_config.task_event_revision(backend) == task_revision
    assert first is second
    assert export_calls == 1


def test_new_revision_replaces_prior_backend_observation(monkeypatch, tmp_path):
    current_revision = "91"
    _stub_backend(monkeypatch, lambda root: current_revision)
    export_calls = 0

    def export(filters, *, taskrc):
        nonlocal export_calls
        export_calls += 1
        return [{"uuid": f"row-{export_calls}"}]

    monkeypatch.setattr(taskboard.tw, "export", export)

    first = taskboard.current_task_board_observation(backend_root=tmp_path)
    first_reference = weakref.ref(first)
    current_revision = "92"
    second = taskboard.current_task_board_observation(backend_root=tmp_path)

    assert first.revision == "91"
    assert second.revision == "92"
    del first
    gc.collect()
    assert first_reference() is None


def test_repeated_message_payloads_answer_every_task_read_from_one_export(
    tmp_path, monkeypatch
):
    """A standalone message payload is a projection of one board observation.

    Filter inventory, task cards, the claimed task, and review pressure are four
    separate reads a message payload used to pay for one export each, per call.
    Two payloads at an unchanged revision now cost one export between them, and
    the second call reproduces the first payload's task-derived shape exactly.
    """
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    exports = _stub_crossing_board(monkeypatch, tmp_path)

    payload = message.messages_payload_for_worktree(state, target, limit=5)
    repeated = message.messages_payload_for_worktree(state, target, limit=5)

    assert exports == [["status.any:"]]
    assert _task_board(payload)["taskFilterInventory"]["openTaskCount"] == 2
    assert "taskFilterInventory" not in payload
    assert [card["source_kind"] for card in _task_cards(payload)] == [
        "cli_task_created"
    ]
    assert payload["statusLine"]["claimedTask"] == {
        "handle": "LATENCY-1kGsk2S2",
        "phase": "todo",
        "title": "Held by the lane",
    }
    assert payload["laneInfo"]["reviewPressure"]["count"] == 1
    assert payload["laneInfo"]["reviewPressure"]["items"][0]["finding"] == "changes"
    assert _task_derived_slice(repeated) == _task_derived_slice(payload)


def test_lane_metrics_stay_lazy_and_draw_drained_from_the_open_observation(
    tmp_path, monkeypatch
):
    """The metrics pane is the only caller that pays for a lane's counters.

    Drained is a completed-row count the observation already indexes, so opening
    the pane spends no export of its own; the durable per-agent summary is the
    one thing the request adds, and message payloads never ask for it.
    """
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    exports = _stub_crossing_board(monkeypatch, tmp_path)
    summarized: list[str] = []
    lane_metric_summary = state.team_store.lane_metric_summary

    def counted_summary(actor, **kwargs):
        summarized.append(actor)
        return lane_metric_summary(actor, **kwargs)

    monkeypatch.setattr(state.team_store, "lane_metric_summary", counted_summary)

    message.messages_payload_for_worktree(state, target, limit=5)
    summarized_before_open = list(summarized)
    metrics = message.lane_metrics_summary_payload(state, target)

    assert summarized_before_open == []
    assert len(summarized) == 1
    # review-row and drained-row are the lane's two completed rows.
    assert metrics["drained"] == 2
    assert exports == [["status.any:"]]


def test_task_mutation_advances_the_board_a_team_wake_reuses(tmp_path, monkeypatch):
    """A real backend mutation is the only thing that re-reads the board.

    Team writes wake lane watchers through the same event file, so a team-only
    wake must leave the task revision -- and therefore the observation every
    lane payload projects -- exactly where the last task mutation left it.
    """
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    backend = tmp_path / "task-backend"
    monkeypatch.setenv(task_config.TASK_BACKEND_ENV, str(backend))
    monkeypatch.setenv(DRIVER.thread_id_env, THREAD_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-crossing")
    monkeypatch.chdir(repo)
    reads: list[Path] = []
    read_task_board = taskboard._read_task_board

    def counted_read(root: Path):
        reads.append(root)
        return read_task_board(root)

    monkeypatch.setattr(taskboard, "_read_task_board", counted_read)

    empty = message.messages_payload_for_worktree(state, target, limit=5)
    repeated = message.messages_payload_for_worktree(state, target, limit=5)
    reads_before_mutation = len(reads)
    create.add(
        "Cross a real mutation onto the board",
        project="serve.latency",
        origin="ack:1jN54zJJ",
        flow=["todo"],
        acceptance=["the next payload observes the new row"],
        creation_surface=task_config.TASK_CREATION_SURFACE_CLI,
    )
    task_revision = task_config.task_event_revision(backend)
    mutated = message.messages_payload_for_worktree(state, target, limit=5)
    reads_after_mutation = len(reads)
    state.team_store.set_global_fast_mode_enabled(True)
    woken = message.messages_payload_for_worktree(state, target, limit=5)

    assert (reads_before_mutation, reads_after_mutation, len(reads)) == (1, 2, 2)
    assert _task_board(empty)["taskFilterInventory"]["openTaskCount"] == 0
    assert _task_derived_slice(repeated) == _task_derived_slice(empty)
    assert _task_board(mutated)["taskFilterInventory"]["openTaskCount"] == 1
    assert [card["display_text"] for card in _task_cards(mutated)] == [
        "Task capture: Cross a real mutation onto the board (serve.latency)"
    ]
    assert (
        task_config.task_event_path(backend)
        .read_text(encoding="utf-8")
        .endswith(" team\n")
    )
    assert task_config.task_event_revision(backend) == task_revision
    assert _task_derived_slice(woken) == _task_derived_slice(mutated)


def test_an_unchanged_real_store_is_reused_without_a_second_export(tmp_path):
    root = tmp_path / "backend"
    _real_backend(root, "kept task")

    first = taskboard.current_task_board_observation(backend_root=root)
    second = taskboard.current_task_board_observation(backend_root=root)

    assert first is second
    assert first.store_generation
    assert _descriptions(first) == ["kept task"]


def test_replacing_the_store_at_an_equal_revision_rebuilds_the_board(tmp_path):
    """The wake file is untouched, so only the store itself says anything moved."""
    root = tmp_path / "backend"
    _real_backend(root, "original task")
    first = taskboard.current_task_board_observation(backend_root=root)
    revision = task_config.task_event_revision(root)

    replacement = tmp_path / "replacement"
    _real_backend(replacement, "replacement task")
    _replace_store(replacement, root)
    second = taskboard.current_task_board_observation(backend_root=root)

    assert task_config.task_event_revision(root) == revision
    assert second.store_generation != first.store_generation
    assert _descriptions(first) == ["original task"]
    assert _descriptions(second) == ["replacement task"]


def test_deleting_and_recreating_the_store_rebuilds_the_board(tmp_path):
    root = tmp_path / "backend"
    taskrc = _real_backend(root, "original task")
    first = taskboard.current_task_board_observation(backend_root=root)
    revision = task_config.task_event_revision(root)

    _store_path(root).unlink()
    taskboard.tw.export(["status.any:"], taskrc=taskrc)
    second = taskboard.current_task_board_observation(backend_root=root)

    assert task_config.task_event_revision(root) == revision
    assert second.store_generation != first.store_generation
    assert _descriptions(first) == ["original task"]
    assert second.rows == ()


def test_a_store_remade_onto_a_recycled_inode_number_rebuilds_the_board(
    monkeypatch, tmp_path
):
    """Every stat the board takes reports the original store's device and inode.

    The platform is made to reuse the inode number rather than hoped into it, so
    the file identity of the replacement equals the original's and the rebuild
    can only come from the generation. The stats carry no creation time either,
    which is the Linux stat the retired witness reduced to device and inode on.
    """
    root = tmp_path / "backend"
    _real_backend(root, "original task")
    original_stat = taskboard._store_stat(root)
    real_store_stat = taskboard._store_stat

    def recycled_store_stat(target: Path) -> os.stat_result:
        return _recycled_stat(real_store_stat(target), onto=original_stat)

    monkeypatch.setattr(taskboard, "_store_stat", recycled_store_stat)
    first = taskboard.current_task_board_observation(backend_root=root)
    revision = task_config.task_event_revision(root)

    replacement = tmp_path / "replacement"
    _real_backend(replacement, "replacement task")
    _replace_store(replacement, root)
    second = taskboard.current_task_board_observation(backend_root=root)
    swapped_stat = recycled_store_stat(root)

    assert task_config.task_event_revision(root) == revision
    assert (swapped_stat.st_dev, swapped_stat.st_ino) == (
        original_stat.st_dev,
        original_stat.st_ino,
    )
    assert second.store_generation != first.store_generation
    assert _descriptions(first) == ["original task"]
    assert _descriptions(second) == ["replacement task"]


def test_a_board_over_a_written_store_costs_one_export_and_a_new_generation(
    monkeypatch, tmp_path
):
    """A board built over a store that already exists pays exactly one export.

    The export writes the store it reads, so a check across it that noticed
    writes would discard its own rows and export again until the deadline. What
    keeps that from passing vacuously is the bare export between the two boards:
    the generation moves across it and costs the second board its own export, so
    the store demonstrably does change under a read that stays coherent anyway.
    """
    root = tmp_path / "backend"
    taskrc = _real_backend(root, "kept task")
    real_export = taskboard.tw.export
    exported: list[int] = []

    def export(filters, *, taskrc):
        rows = real_export(filters, taskrc=taskrc)
        exported.append(len(rows))
        return rows

    monkeypatch.setattr(taskboard.tw, "export", export)
    first = taskboard.current_task_board_observation(backend_root=root)
    exports_for_one_board = len(exported)
    real_export(["status.any:"], taskrc=taskrc)
    second = taskboard.current_task_board_observation(backend_root=root)

    assert exports_for_one_board == FIRST_EXPORT
    assert exported == [KEPT_TASK_ROW_COUNT, KEPT_TASK_ROW_COUNT]
    assert second.store_generation != first.store_generation
    assert _descriptions(second) == ["kept task"]


def test_a_store_replaced_during_the_export_is_discarded_and_retried(
    monkeypatch, tmp_path
):
    """Rows read across a swap belong to no one store, so the candidate is dropped."""
    root = tmp_path / "backend"
    _real_backend(root, "original task")
    replacement = tmp_path / "replacement"
    _real_backend(replacement, "replacement task")

    real_export = taskboard.tw.export
    exported: list[int] = []

    def export(filters, *, taskrc):
        rows = real_export(filters, taskrc=taskrc)
        exported.append(len(rows))
        if len(exported) == FIRST_EXPORT:
            _replace_store(replacement, root)
        return rows

    monkeypatch.setattr(taskboard.tw, "export", export)
    observation = taskboard.current_task_board_observation(backend_root=root)

    assert len(exported) == RETRIED_EXPORTS
    assert _descriptions(observation) == ["replacement task"]


def test_separate_real_backends_keep_separate_store_observations(tmp_path):
    first_root = tmp_path / "first"
    second_root = tmp_path / "second"
    _real_backend(first_root, "first task")
    _real_backend(second_root, "second task")

    first = taskboard.current_task_board_observation(backend_root=first_root)
    second = taskboard.current_task_board_observation(backend_root=second_root)
    first_again = taskboard.current_task_board_observation(backend_root=first_root)

    assert first is first_again
    assert first.store_generation != second.store_generation
    assert _descriptions(first) == ["first task"]
    assert _descriptions(second) == ["second task"]


def test_a_team_event_reuses_the_real_store_observation(tmp_path):
    root = tmp_path / "backend"
    _real_backend(root, "kept task")
    first = taskboard.current_task_board_observation(backend_root=root)
    revision = task_config.task_event_revision(root)

    task_config.mark_task_backend_changed("team", root=root)
    second = taskboard.current_task_board_observation(backend_root=root)

    assert task_config.task_event_revision(root) == revision
    assert first is second
    assert _descriptions(second) == ["kept task"]


def test_the_first_board_read_settles_on_the_store_its_own_export_created(
    monkeypatch, tmp_path
):
    """Nothing was there to go stale, so one export settles the first board.

    The store does not exist until an export creates it, so the identity before
    the first export is the empty witness and the identity after it is real.
    Reading that as a store swapped mid-build would discard a candidate that is
    perfectly coherent and pay for a second export on every cold start.

    The generation this settles on is taken from the same stat that ended the
    build, which is what lets the very next read hit: a generation measured
    before the export would already be behind the store the export wrote.
    """
    root = tmp_path / "backend"
    task_config.materialize_task_backend(root)
    real_export = taskboard.tw.export
    exported: list[int] = []

    def export(filters, *, taskrc):
        rows = real_export(filters, taskrc=taskrc)
        exported.append(len(rows))
        return rows

    monkeypatch.setattr(taskboard.tw, "export", export)
    observation = taskboard.current_task_board_observation(backend_root=root)
    reused = taskboard.current_task_board_observation(backend_root=root)

    assert exported == [0]
    assert _store_path(root).is_file()
    assert observation.store_generation
    assert observation is reused
