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
