"""Revision-coherent Serve task-board observation tests."""

from __future__ import annotations

import gc
import os
import threading
import weakref
from collections.abc import Mapping
from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime
from pathlib import Path
from types import MappingProxyType

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.serve import taskboard
from spice.serve.payload import lane, message
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
)

# A board revision is the generation its authority minted, so these fixtures
# carry counts rather than labels: the chrome producer publishes an epoch only
# where it could have counted forward from it.
CROSSING_REVISION = "1785044000000100"
MEASURED_GENERATION = "1785044000000200"
SEMANTIC_EDGE_GENERATION = "1785044000000300"
SEMANTIC_ERROR_GENERATION = "1785044000000400"
OPEN_STATE_GENERATION = "1785044000000500"
ACTIVE_CLAIM_GENERATION = "1785044000000600"
SHARED_INDEX_GENERATION = "1785044000000700"
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
SEMANTIC_DASHED_ACTOR = "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"
SEMANTIC_EDGE_ROWS: tuple[Mapping[str, object], ...] = (
    {
        "uuid": "ready-card",
        "description": "Ready task card",
        "project": "serve.latency",
        "status": "pending",
        "origin_thread": THREAD_A,
        "entry": "20260725T190000Z",
    },
    {
        "uuid": "past-scheduled",
        "description": "Past scheduled",
        "project": "serve.latency",
        "status": "pending",
        "scheduled": "20200101T000000Z",
    },
    {
        "uuid": "future-scheduled",
        "description": "Future scheduled",
        "project": "serve.latency",
        "status": "pending",
        "scheduled": "20990101T000000Z",
    },
    {
        "uuid": "blocker",
        "description": "Open blocker",
        "project": "serve.latency",
        "status": "pending",
    },
    {
        "uuid": "blocked",
        "description": "Dependency blocked",
        "project": "serve.latency",
        "status": "pending",
        "depends": ["blocker"],
    },
    {
        "uuid": "claimed-deferred-older",
        "description": "Claim survives its future wait",
        "project": "serve.latency",
        "status": "pending",
        "claim_by": THREAD_A,
        "claim_at": "2026-07-25T20:00:00Z",
        "start": "20260725T200000Z",
        "wait": "20990101T000000Z",
    },
    {
        "uuid": "latest-claim",
        "description": "Latest active claim",
        "project": "serve.latency",
        "status": "pending",
        "claim_by": THREAD_A,
        "claim_at": "2026-07-25T21:00:00Z",
        "start": "20260725T210000Z",
    },
    {
        "uuid": "claimed-but-not-active",
        "description": "Claim metadata without ACTIVE",
        "project": "serve.latency",
        "status": "pending",
        "claim_by": THREAD_A,
        "claim_at": "2026-07-25T22:00:00Z",
    },
    {
        "uuid": "completed-claim",
        "description": "Completed row retains claim owner",
        "project": "serve.latency",
        "status": "completed",
        "claim_by": THREAD_A,
    },
    {
        "uuid": "review-raw",
        "description": "Raw actor review",
        "project": "serve.latency",
        "status": "completed",
        "review_author": THREAD_A,
        "review_by": "peer-a",
        "review_finding": "changes",
        "review_at": "2026-07-25T22:00:00Z",
    },
    {
        "uuid": "review-prefixed",
        "description": "Prefixed actor review",
        "project": "serve.latency",
        "status": "completed",
        "review_author": f"thread:{THREAD_A}",
        "review_by": "peer-b",
        "review_finding": "blocked",
        "review_at": "2026-07-25T23:00:00Z",
    },
    {
        "uuid": "review-clean",
        "description": "Clean review",
        "project": "serve.latency",
        "status": "completed",
        "review_author": THREAD_A,
        "review_finding": "clean",
        "review_at": "2026-07-25T21:30:00Z",
    },
    {
        "uuid": "pending-followup",
        "description": "Pending follow-up",
        "project": "serve.latency",
        "status": "pending",
        "depends": ["review-raw"],
    },
    {
        "uuid": "waiting-followup",
        "description": "Waiting follow-up",
        "project": "serve.latency",
        "status": "waiting",
        "depends": "review-raw,review-prefixed",
    },
    {
        "uuid": "completed-followup",
        "description": "Closed follow-up",
        "project": "serve.latency",
        "status": "completed",
        "depends": ["review-raw"],
    },
    {
        "uuid": "inside-card-window",
        "description": "Inside card window",
        "project": "serve.latency",
        "status": "waiting",
        "origin_thread": THREAD_A,
        "entry": "20260725T210000Z",
    },
    {
        "uuid": "after-card-window",
        "description": "After card window",
        "project": "serve.latency",
        "status": "completed",
        "origin_thread": THREAD_A,
        "entry": "20260725T230000Z",
    },
    {
        "uuid": "noncanonical-stored-origin",
        "description": "Stored origin must still match exactly",
        "project": "serve.latency",
        "status": "pending",
        "origin_thread": SEMANTIC_DASHED_ACTOR,
        "entry": "20260725T210500Z",
    },
)


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


def _task_cards(payload: dict) -> list[dict]:
    return [item for item in payload["messages"] if item["kind"] == "task_card"]


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


def _pre_fold_dependencies(row: Mapping[str, object]) -> set[str]:
    raw = row.get("depends")
    if isinstance(raw, list | tuple):
        return {str(value) for value in raw if value}
    if isinstance(raw, str):
        return {value.strip() for value in raw.split(",") if value.strip()}
    return set()


def _pre_fold_datetime(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.strptime(value, taskboard.tw.TW_DATETIME_FORMAT).replace(
            tzinfo=UTC
        )
    except ValueError:
        return None


def _pre_fold_filter_counts(
    rows: tuple[Mapping[str, object], ...],
    *,
    project: str,
) -> dict[str, int]:
    """The deleted filter view's row classifier, retained as a parity oracle."""
    open_rows = tuple(
        row
        for row in rows
        if str(row.get("status") or "pending") in {"pending", "waiting"}
    )
    open_uuids = {str(row.get("uuid") or "") for row in open_rows if row.get("uuid")}
    ready: set[str] = set()
    waiting: set[str] = set()
    blocked: set[str] = set()
    now = datetime.now(UTC)
    for row in open_rows:
        uuid = str(row.get("uuid") or "")
        if not uuid or row.get("claim_by") or row.get("start"):
            continue
        wait_at = _pre_fold_datetime(row.get("wait"))
        if wait_at is not None and wait_at > now:
            waiting.add(uuid)
            continue
        if _pre_fold_dependencies(row) & open_uuids:
            blocked.add(uuid)
            continue
        scheduled_at = _pre_fold_datetime(row.get("scheduled"))
        if scheduled_at is None or scheduled_at <= now:
            ready.add(uuid)

    counts = {
        "openTaskCount": 0,
        "readyTaskCount": 0,
        "inFlightTaskCount": 0,
        "blockedTaskCount": 0,
        "deferredTaskCount": 0,
    }
    for row in open_rows:
        if str(row.get("project") or "") != project:
            continue
        counts["openTaskCount"] += 1
        uuid = str(row.get("uuid") or "")
        if row.get("claim_by"):
            field = "inFlightTaskCount"
        elif uuid in waiting:
            field = "deferredTaskCount"
        elif uuid in blocked:
            field = "blockedTaskCount"
        elif uuid in ready:
            field = "readyTaskCount"
        else:
            field = "deferredTaskCount"
        counts[field] += 1
    return counts


def _pre_fold_active_claim(
    rows: tuple[Mapping[str, object], ...],
    actor: str,
) -> Mapping[str, object] | None:
    active = [
        row
        for row in rows
        if str(row.get("status") or "pending") in {"pending", "waiting"}
        and row.get("start")
        and str(row.get("claim_by") or "") == actor
    ]
    return (
        max(active, key=lambda row: str(row.get("claim_at") or "")) if active else None
    )


def _pre_fold_card_rows(
    rows: tuple[Mapping[str, object], ...],
    actor: str,
) -> tuple[Mapping[str, object], ...]:
    return tuple(row for row in rows if str(row.get("origin_thread") or "") == actor)


def _pre_fold_completed_reviews(
    rows: tuple[Mapping[str, object], ...],
    actors: set[str],
) -> tuple[Mapping[str, object], ...]:
    selected = [
        row
        for row in rows
        if str(row.get("status") or "") == "completed"
        and str(row.get("review_author") or "") in actors
    ]
    selected.sort(
        key=lambda row: str(
            row.get("review_at")
            or row.get("end")
            or row.get("modified")
            or row.get("entry")
            or ""
        ),
        reverse=True,
    )
    return tuple(selected)


def _pre_fold_followup_count(
    rows: tuple[Mapping[str, object], ...],
    reviewed_uuid: str,
) -> int:
    return sum(
        reviewed_uuid in _pre_fold_dependencies(row)
        for row in rows
        if str(row.get("status") or "pending") in {"pending", "waiting"}
    )


def _pre_fold_drained_count(
    rows: tuple[Mapping[str, object], ...],
    actor: str,
) -> int:
    return sum(
        any(str(row.get(field) or "") == actor for field in taskboard.TASK_ACTOR_FIELDS)
        for row in rows
        if str(row.get("status") or "") == "completed"
    )


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


def test_open_projection_preserves_filter_state_parity():
    observation = taskboard.TaskBoardObservation(
        backend_identity="test",
        revision=OPEN_STATE_GENERATION,
        rows=(
            {"uuid": "ready", "project": "serve.latency"},
            {
                "uuid": "in-flight",
                "project": "serve.latency",
                "claim_by": "agent-a",
                "start": "20260725T200000Z",
            },
            {
                "uuid": "blocked",
                "project": "serve.latency",
                "depends": ["ready"],
            },
            {
                "uuid": "future-wait",
                "project": "serve.latency",
                "wait": "20990101T000000Z",
            },
            {
                "uuid": "future-scheduled",
                "project": "serve.latency",
                "scheduled": "20990101T000000Z",
            },
            {
                "uuid": "started-without-claim",
                "project": "serve.latency",
                "start": "20260725T200000Z",
            },
            {"uuid": "private", "project": "agent.abc123.task"},
            {"uuid": "oops", "project": ".oops.correctness"},
            {
                "uuid": "completed",
                "project": "serve.latency",
                "status": "completed",
            },
        ),
    )

    inventory = taskboard.open_task_board_projection(observation).task_filter_inventory
    filters = {item["name"]: item for item in inventory["filters"]}
    stems = {item["name"]: item for item in inventory["primaryStems"]}

    assert filters["serve.latency"] == {
        "name": "serve.latency",
        "primaryStem": "serve",
        "openTaskCount": 6,
        "readyTaskCount": 1,
        "inFlightTaskCount": 1,
        "blockedTaskCount": 1,
        "deferredTaskCount": 3,
    }
    assert stems["agent"]["readyTaskCount"] == 1
    assert stems["oops"]["oopsTaskCount"] == 1
    assert stems["waiting"]["waitingTaskCount"] == 1
    assert inventory["openTaskCount"] == 6


@pytest.fixture
def semantic_edge_projection(
    monkeypatch,
) -> taskboard.OpenTaskBoardProjection:
    observation = taskboard.TaskBoardObservation(
        backend_identity="differential",
        revision=SEMANTIC_EDGE_GENERATION,
        rows=SEMANTIC_EDGE_ROWS,
    )
    projection = taskboard.open_task_board_projection(observation)
    monkeypatch.setattr(
        taskboard.tw,
        "export",
        lambda *_args, **_kwargs: pytest.fail(
            "differential projection queries must not export"
        ),
    )
    return projection


def test_projection_matches_pre_fold_filter_and_claim_edges(
    semantic_edge_projection,
):
    projection = semantic_edge_projection
    inventory_filter = next(
        item
        for item in projection.task_filter_inventory["filters"]
        if item["name"] == "serve.latency"
    )
    expected_counts = _pre_fold_filter_counts(
        SEMANTIC_EDGE_ROWS,
        project="serve.latency",
    )
    assert {
        field: inventory_filter[field]
        for field in taskboard.TASK_FILTER_STATE_COUNT_FIELDS
    } == expected_counts

    canonical_actor = taskboard.tw.canonical_actor(SEMANTIC_DASHED_ACTOR)
    expected_claim = _pre_fold_active_claim(SEMANTIC_EDGE_ROWS, canonical_actor)
    assert expected_claim is SEMANTIC_EDGE_ROWS[6]
    assert projection.active_claim(SEMANTIC_DASHED_ACTOR) is expected_claim


def test_projection_matches_pre_fold_card_selection_and_boundaries(
    semantic_edge_projection,
    monkeypatch,
    tmp_path,
):
    projection = semantic_edge_projection
    canonical_actor = taskboard.tw.canonical_actor(SEMANTIC_DASHED_ACTOR)
    expected_rows = _pre_fold_card_rows(SEMANTIC_EDGE_ROWS, canonical_actor)
    assert projection.task_card_rows(SEMANTIC_DASHED_ACTOR) == expected_rows

    after = "2026-07-25T20:00:00Z#pre-fold"
    before = "2026-07-25T22:00:00Z#pre-fold"
    expected_cards = []
    for row in expected_rows:
        card = message._task_card_message_from_row(row)
        if card is not None and message._message_inside_time_boundary(
            card,
            after=after,
            before=before,
        ):
            expected_cards.append(card)
    assert [card.text for card in expected_cards] == [
        "Task capture: Inside card window (serve.latency)"
    ]

    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    monkeypatch.setattr(message, "open_task_board_projection", lambda: projection)
    payload = message.messages_payload_for_worktree(
        state,
        target,
        limit=10,
        after=after,
        before=before,
        expected_thread_id=THREAD_A,
    )
    payload_cards = _task_cards(payload)
    assert [card["key"] for card in payload_cards] == [
        card.key for card in expected_cards
    ]
    assert [card["display_text"] for card in payload_cards] == [
        card.display_text for card in expected_cards
    ]


def test_projection_matches_pre_fold_review_followup_and_drained_edges(
    semantic_edge_projection,
):
    projection = semantic_edge_projection
    serve_identity = {
        "actorId": f"thread:{THREAD_A}",
        "thread": {"threadId": THREAD_A},
    }
    review_actors = lane._review_pressure_actor_keys(serve_identity)
    expected_reviews = _pre_fold_completed_reviews(
        SEMANTIC_EDGE_ROWS,
        review_actors,
    )
    first_reviews = projection.completed_review_rows(review_actors)
    second_reviews = projection.completed_review_rows(review_actors)
    assert first_reviews == expected_reviews
    assert second_reviews == expected_reviews
    assert second_reviews is not first_reviews

    for reviewed_uuid in ("review-raw", "review-prefixed"):
        assert projection.open_review_followup_count(
            reviewed_uuid
        ) == _pre_fold_followup_count(SEMANTIC_EDGE_ROWS, reviewed_uuid)
    pressure = lane.review_pressure_payload(serve_identity, task_board=projection)
    assert pressure["count"] == 2
    assert pressure["openFollowupCount"] == 3
    assert [item["followupCount"] for item in pressure["items"]] == [1, 2]

    canonical_actor = taskboard.tw.canonical_actor(SEMANTIC_DASHED_ACTOR)
    assert projection.drained_task_count(
        SEMANTIC_DASHED_ACTOR
    ) == _pre_fold_drained_count(SEMANTIC_EDGE_ROWS, canonical_actor)
    assert projection.drained_task_count(SEMANTIC_DASHED_ACTOR) == 3


def test_failed_projection_matches_pre_fold_empty_task_views():
    review_actors = {THREAD_A, f"thread:{THREAD_A}"}
    failed = taskboard.open_task_board_projection(
        taskboard.TaskBoardObservation(
            backend_identity="differential",
            revision=SEMANTIC_ERROR_GENERATION,
            rows=(),
            error="backend unavailable",
        )
    )

    assert failed.task_filter_inventory["filters"] == []
    assert failed.active_claim(SEMANTIC_DASHED_ACTOR) is None
    assert failed.task_card_rows(SEMANTIC_DASHED_ACTOR) == ()
    assert failed.completed_review_rows(review_actors) == ()
    assert failed.open_review_followup_count("review-raw") == 0
    assert failed.drained_task_count(SEMANTIC_DASHED_ACTOR) == 0


def test_open_projection_indexes_latest_claim_without_copying_rows(monkeypatch):
    rows = (
        {
            "uuid": "older",
            "claim_by": "agenta",
            "claim_at": "2026-07-25T20:00:00Z",
            "start": "20260725T200000Z",
        },
        {
            "uuid": "latest-deferred",
            "claim_by": "agenta",
            "claim_at": "2026-07-25T21:00:00Z",
            "start": "20260725T210000Z",
            "wait": "20990101T000000Z",
        },
        {
            "uuid": "newer-but-not-active",
            "claim_by": "agenta",
            "claim_at": "2026-07-25T22:00:00Z",
        },
        {
            "uuid": "completed",
            "status": "completed",
            "claim_by": "agenta",
            "claim_at": "2026-07-25T23:00:00Z",
            "start": "20260725T230000Z",
        },
    )
    observation = taskboard.TaskBoardObservation(
        backend_identity="test",
        revision=ACTIVE_CLAIM_GENERATION,
        rows=rows,
    )

    def unexpected_export(*_args, **_kwargs):
        raise AssertionError("projection queries must not export")

    monkeypatch.setattr(taskboard.tw, "export", unexpected_export)

    projection = taskboard.open_task_board_projection(observation)

    assert projection.active_claim("agent-a") is observation.rows[1]
    assert projection.active_claim("agenta") is observation.rows[1]
    assert projection.active_claim("missing") is None
    assert taskboard.open_task_board_projection(observation) is projection


def test_projection_reuses_card_review_followup_and_drained_indexes(monkeypatch):
    rows = (
        {"uuid": "pending-card", "status": "pending", "origin_thread": "agenta"},
        {"uuid": "waiting-card", "status": "waiting", "origin_thread": "agenta"},
        {
            "uuid": "completed-card",
            "status": "completed",
            "origin_thread": "agenta",
        },
        {
            "uuid": "exact-origin-only",
            "status": "pending",
            "origin_thread": "agent-a",
        },
        {
            "uuid": "reviewed-older",
            "status": "completed",
            "claim_by": "another-actor",
            "review_author": "reviewer-a",
            "review_finding": "changes",
            "review_at": "2026-07-24T20:00:00Z",
        },
        {
            "uuid": "reviewed-newer",
            "status": "completed",
            "claim_by": "another-actor",
            "review_author": "reviewer-a",
            "review_finding": "blocked",
            "review_at": "2026-07-25T20:00:00Z",
        },
        {
            "uuid": "reviewed-other",
            "status": "completed",
            "review_author": "reviewer-b",
            "review_finding": "changes",
            "review_at": "2026-07-26T20:00:00Z",
        },
        {
            "uuid": "pending-followup",
            "status": "pending",
            "depends": ["reviewed-newer"],
        },
        {
            "uuid": "waiting-followup",
            "status": "waiting",
            "depends": "reviewed-newer",
        },
        {
            "uuid": "completed-followup",
            "status": "completed",
            "depends": ["reviewed-newer"],
        },
        {"uuid": "drained-claim", "status": "completed", "claim_by": "agenta"},
        {
            "uuid": "drained-thread",
            "status": "completed",
            "claim_thread": "agenta",
        },
        {
            "uuid": "drained-review-author",
            "status": "completed",
            "review_author": "agenta",
        },
        {
            "uuid": "drained-reviewer",
            "status": "completed",
            "review_by": "agenta",
            "claim_by": "agenta",
        },
        {"uuid": "not-drained", "status": "pending", "claim_by": "agenta"},
    )
    observation = taskboard.TaskBoardObservation(
        backend_identity="test",
        revision=SHARED_INDEX_GENERATION,
        rows=rows,
    )
    projection = taskboard.open_task_board_projection(observation)
    monkeypatch.setattr(
        taskboard.tw,
        "export",
        lambda *_args, **_kwargs: pytest.fail("projection queries must not export"),
    )

    cards = projection.task_card_rows("agent-a")
    reviews = projection.completed_review_rows({"reviewer-a", "thread:reviewer-a"})

    assert cards is projection.task_card_rows("agent-a")
    assert cards == observation.rows[:3]
    repeated_reviews = projection.completed_review_rows(
        {"thread:reviewer-a", "reviewer-a"}
    )
    assert repeated_reviews == reviews
    assert repeated_reviews is not reviews
    assert reviews == (observation.rows[5], observation.rows[4])
    assert reviews[0] is observation.rows[5]
    assert projection.open_review_followup_count("reviewed-newer") == 2
    assert projection.drained_task_count("agent-a") == 4
    assert projection.drained_task_count("") == 0


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
    assert first.store_identity
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
    assert second.store_identity != first.store_identity
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
    assert second.store_identity != first.store_identity
    assert _descriptions(first) == ["original task"]
    assert second.rows == ()


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
    assert first.store_identity != second.store_identity
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
    assert observation.store_identity
    assert observation is reused
