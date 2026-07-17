"""`spice serve --backend` isolates every managed-state root under scratch."""

from __future__ import annotations

import http.client
import json
import subprocess
import threading
from argparse import Namespace
from http import HTTPStatus
from pathlib import Path

import pytest

from spice import paths
from spice.agent.maximmetrics import maxim_metrics_database_path
from spice.agent.paths import agent_thread_pointer_path, agent_thread_state_dir
from spice.agent.runinbox import inbox_pending_signature
from spice.errors import SpiceError
from spice.mail.ackstate import ack_state_database_path
from spice.mail.inbox import (
    collect_inbox_items,
    compose_inbox_text,
    inbox_dir,
    write_inbox_item,
)
from spice.serve import app as serve_app
from spice.serve.app import TASK_BACKEND_LIVE_LANE_ERROR, apply_serve_backends
from spice.tasks import config as task_config
from tests.test_servehelpers import _repo, _serve_state, _target

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


@pytest.mark.parametrize("backend_kind", ["task", "total"])
def test_http_send_with_scratch_backend_preserves_live_inbox(
    tmp_path, scratch_overrides, backend_kind
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    write_inbox_item(
        repo,
        "20260101T000000000001Z.txt",
        compose_inbox_text(body="live seed", priority=None, stop=False),
    )
    before = _inbox_snapshot(repo)
    scratch = tmp_path / "scratch"
    if backend_kind == "total":
        apply_serve_backends(_serve_args(scratch, None))
    else:
        apply_serve_backends(_serve_args(None, scratch / "task"))

    status, payload = _post_json(
        state,
        f"/api/work/trees/{target.id}/send",
        {"text": "must remain scratch-only"},
    )

    paths.set_state_backend(None)
    task_config.set_backend(None)
    assert status == HTTPStatus.METHOD_NOT_ALLOWED
    assert payload == {"ok": False, "error": TASK_BACKEND_LIVE_LANE_ERROR}
    assert _inbox_snapshot(repo) == before


def test_http_agent_ensure_with_scratch_backend_uses_isolation_policy(
    tmp_path, scratch_overrides
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    apply_serve_backends(_serve_args(None, tmp_path / "scratch-task"))

    status, payload = _post_json(
        state,
        f"/api/work/trees/{target.id}/agent/ensure",
        {},
    )

    assert status == HTTPStatus.METHOD_NOT_ALLOWED
    assert payload == {"ok": False, "error": TASK_BACKEND_LIVE_LANE_ERROR}


def test_http_send_with_live_backend_delegates_successfully(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    calls: list[tuple[object, object, dict[str, object]]] = []

    def send_payload(current_state, current_target, payload):
        calls.append((current_state, current_target, payload))
        return {"ok": True, "key": "inbox-key"}, HTTPStatus.OK

    monkeypatch.setattr(serve_app, "work_tree_send_response_payload", send_payload)

    status, payload = _post_json(
        state,
        f"/api/work/trees/{target.id}/send",
        {"text": "continue live work"},
    )

    assert status == HTTPStatus.OK
    assert payload == {"ok": True, "key": "inbox-key"}
    assert calls == [(state, target, {"text": "continue live work"})]


def _inbox_snapshot(repo: Path) -> dict[str, bytes]:
    return {
        item.name: item.source_path.read_bytes() for item in collect_inbox_items(repo)
    }


def _post_json(
    state: serve_app.ServeState, path: str, payload: dict[str, object]
) -> tuple[int, dict[str, object]]:
    server = serve_app._ServeHttpServer(
        ("127.0.0.1", 0), serve_app._ServeHandler, state
    )
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    host, port = server.server_address[:2]
    try:
        connection = http.client.HTTPConnection(host, port, timeout=2)
        connection.request(
            "POST",
            path,
            body=json.dumps(payload),
            headers={"Content-Type": "application/json"},
        )
        response = connection.getresponse()
        body = json.loads(response.read().decode("utf-8"))
        connection.close()
        return response.status, body
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
