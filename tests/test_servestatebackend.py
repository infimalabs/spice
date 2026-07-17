"""`spice serve --backend` isolates every managed-state root under scratch."""

from __future__ import annotations

import json
import subprocess
from argparse import Namespace
from pathlib import Path

import pytest

from spice import paths
from spice.agent.maximmetrics import maxim_metrics_database_path
from spice.agent.paths import agent_thread_pointer_path, agent_thread_state_dir
from spice.errors import SpiceError
from spice.mail.ackstate import ack_state_database_path
from spice.serve.app import apply_serve_backends
from spice.tasks import config as task_config

SESSION_THREAD_ID = "1kTestThread"


@pytest.fixture
def scratch_overrides():
    yield
    paths.set_state_backend(None)
    task_config.set_backend(None)


def _serve_args(backend: Path | None, task_backend: Path | None) -> Namespace:
    return Namespace(
        backend=str(backend) if backend is not None else None,
        task_backend=str(task_backend) if task_backend is not None else None,
    )


def test_backend_prefixes_every_managed_state_surface(tmp_path, scratch_overrides):
    scratch = tmp_path / "scratch"
    live = tmp_path / "live"
    apply_serve_backends(_serve_args(scratch, None))
    surfaces = {
        "shared_root": paths.shared_state_root(live),
        "worktree_root": paths.worktree_state_root(live),
        "agent_registry": agent_thread_pointer_path(live),
        "session_records": agent_thread_state_dir(live, SESSION_THREAD_ID),
        "ack_state": ack_state_database_path(live),
        "maxim_metrics": maxim_metrics_database_path(live),
        "task_store": task_config.backend_root(),
    }
    resolved = scratch.resolve()
    for name, surface in surfaces.items():
        assert surface.is_relative_to(resolved), name


def test_backend_keys_each_worktree_to_its_own_subtree(tmp_path, scratch_overrides):
    apply_serve_backends(_serve_args(tmp_path / "scratch", None))
    lane_a = paths.worktree_state_root(tmp_path / "lane-a")
    lane_b = paths.worktree_state_root(tmp_path / "lane-b")
    assert lane_a != lane_b
    assert lane_a.parent == lane_b.parent


def test_backend_carries_the_task_store_by_default(tmp_path, scratch_overrides):
    scratch = tmp_path / "scratch"
    apply_serve_backends(_serve_args(scratch, None))
    assert (
        task_config.backend_root() == scratch.resolve() / paths.STATE_BACKEND_TASK_DIR
    )


def test_explicit_task_backend_wins_for_the_task_store_alone(
    tmp_path, scratch_overrides
):
    scratch = tmp_path / "scratch"
    task_scratch = tmp_path / "task-scratch"
    apply_serve_backends(_serve_args(scratch, task_scratch))
    assert task_config.backend_root() == task_scratch.resolve()
    assert paths.shared_state_root(tmp_path / "live").is_relative_to(scratch.resolve())


def test_relative_backend_is_refused_loudly(scratch_overrides):
    with pytest.raises(SpiceError, match="--backend requires an absolute scratch path"):
        apply_serve_backends(Namespace(backend="scratch", task_backend=None))


def test_live_state_stays_byte_identical_under_backend_writes(
    tmp_path, scratch_overrides
):
    live = tmp_path / "live"
    live.mkdir()
    subprocess.run(
        ["git", "init", str(live)], check=True, capture_output=True, text=True
    )
    live_state = paths.shared_state_root(live)
    seed = {
        Path("agents") / "registry.json": '{"agents": ["live"]}\n',
        Path("mail") / "inbox.json": '{"items": []}\n',
        Path("sessions") / "record.json": '{"session": "live"}\n',
    }
    for relative, content in seed.items():
        target = live_state / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
    before = {
        item.relative_to(live_state): item.read_bytes()
        for item in sorted(live_state.rglob("*"))
        if item.is_file()
    }
    assert sorted(before) == sorted(seed)

    apply_serve_backends(_serve_args(tmp_path / "scratch", None))
    probe = paths.shared_state_path(live, Path("agents") / "registry.json")
    paths.atomic_write_json(probe, {"agents": ["scratch"]})

    assert probe.is_relative_to((tmp_path / "scratch").resolve())
    assert json.loads(probe.read_text(encoding="utf-8")) == {"agents": ["scratch"]}
    after = {
        item.relative_to(live_state): item.read_bytes()
        for item in sorted(live_state.rglob("*"))
        if item.is_file()
    }
    assert after == before
