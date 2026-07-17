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
from spice.agent.runinbox import inbox_pending_signature
from spice.errors import SpiceError
from spice.mail.ackstate import ack_state_database_path
from spice.mail.inbox import collect_inbox_items, inbox_dir
from spice.serve.app import apply_serve_backends
from spice.serve.httpapi import lane_watch_paths_for_target
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.worktree.target import WorktreeTarget
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
        "operator_inbox": inbox_dir(live),
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


def test_backend_isolates_operator_inbox_reads_and_writes(tmp_path, scratch_overrides):
    live = tmp_path / "live"
    live_inbox = inbox_dir(live)
    assert live_inbox == live / paths.STATE_DIRNAME / paths.INBOX_DIRNAME
    live_inbox.mkdir(parents=True)
    pending = live_inbox / "20260101T000000000000Z.txt"
    pending.write_text("live steering stays put\n", encoding="utf-8")
    before = {item.name: item.read_bytes() for item in live_inbox.iterdir()}

    apply_serve_backends(_serve_args(tmp_path / "scratch", None))
    scratch_inbox = inbox_dir(live)
    assert scratch_inbox.is_relative_to((tmp_path / "scratch").resolve())
    scratch_inbox.mkdir(parents=True)
    (scratch_inbox / "20260102T000000000000Z.txt").write_text(
        "scratch steering\n", encoding="utf-8"
    )
    items = collect_inbox_items(live)
    assert [item.name for item in items] == ["20260102T000000000000Z.txt"]
    assert items[0].text == "scratch steering\n"
    assert [row[0] for row in inbox_pending_signature(live)] == [
        "20260102T000000000000Z.txt"
    ]

    paths.set_state_backend(None)
    assert {item.name: item.read_bytes() for item in live_inbox.iterdir()} == before
    restored = collect_inbox_items(live)
    assert [(item.name, item.text) for item in restored] == [
        ("20260101T000000000000Z.txt", "live steering stays put\n")
    ]


def test_backend_isolates_serve_watcher_and_payload_paths(tmp_path, scratch_overrides):
    live = tmp_path / "live"
    live_inbox = inbox_dir(live)
    live_inbox.mkdir(parents=True)
    (live_inbox / "20260101T000000000000Z.txt").write_text(
        "live steering stays put\n", encoding="utf-8"
    )
    before = _tree_snapshot(live)

    apply_serve_backends(_serve_args(tmp_path / "scratch", None))
    target = WorktreeTarget(id="lane", repo_root=live, name="live", branch="main")
    watch_paths = lane_watch_paths_for_target(None, target, None, None)
    scratch = (tmp_path / "scratch").resolve()
    assert len(watch_paths) >= 2
    for path in watch_paths:
        assert path.is_relative_to(scratch), path
    scratch_inbox = inbox_dir(live)
    assert scratch_inbox.is_dir()
    (scratch_inbox / "20260102T000000000000Z.txt").write_text(
        "scratch steering\n", encoding="utf-8"
    )
    payload = pending_inbox_identity_payload(live)
    assert payload["pendingInboxKeys"] == ["20260102T000000000000Z"]
    assert payload["pendingInboxCount"] == 1
    assert payload["pendingInboxVersion"] > 0

    paths.set_state_backend(None)
    task_config.set_backend(None)
    assert _tree_snapshot(live) == before
    live_payload = pending_inbox_identity_payload(live)
    assert live_payload["pendingInboxKeys"] == ["20260101T000000000000Z"]


def _tree_snapshot(root: Path) -> dict[Path, bytes | None]:
    """Files with their bytes plus bare directories: a stray mkdir shows up."""
    return {
        item.relative_to(root): (item.read_bytes() if item.is_file() else None)
        for item in sorted(root.rglob("*"))
    }


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
