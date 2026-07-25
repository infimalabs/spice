"""Revision-coherent Serve task-board observation tests."""

from __future__ import annotations

import gc
import threading
import weakref
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from types import MappingProxyType

import pytest

from spice.errors import SpiceError
from spice.serve import taskboard
from spice.tasks import config as task_config


@pytest.fixture(autouse=True)
def _clear_task_board_observations():
    with taskboard._task_board_condition:
        taskboard._task_board_observations.clear()
        taskboard._task_board_builds.clear()
    yield
    with taskboard._task_board_condition:
        taskboard._task_board_observations.clear()
        taskboard._task_board_builds.clear()


def _stub_backend(monkeypatch, revision):
    monkeypatch.setattr(task_config, "task_event_revision", revision)
    monkeypatch.setattr(
        task_config,
        "materialize_task_backend",
        lambda root: root / "taskrc",
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
        revision="open-states",
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
        revision="claims",
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
        revision="shared-indexes",
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
    assert reviews is projection.completed_review_rows(
        {"thread:reviewer-a", "reviewer-a"}
    )
    assert reviews == (observation.rows[5], observation.rows[4])
    assert reviews[0] is observation.rows[5]
    assert projection.open_review_followup_count("reviewed-newer") == 2
    assert projection.drained_task_count("agent-a") == 4
    assert projection.drained_task_count("") == 0
