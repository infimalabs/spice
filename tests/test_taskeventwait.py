"""Native allocator-event wait behavior."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from threading import Thread

import pytest

from spice.mail.inbox import write_inbox_item
from spice.tasks import config, eventwait


@pytest.fixture
def wait_backend(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    backend = tmp_path / "task-backend"
    repo.mkdir()
    config.set_backend(str(backend))
    monkeypatch.setattr(config, "repo_root", lambda: repo)
    try:
        yield repo, backend
    finally:
        config.set_backend(None)


def test_allocator_wait_wakes_on_atomic_task_event(wait_backend):
    _repo, backend = wait_backend
    baseline = eventwait.task_event_token()
    wakes: list[eventwait.AllocatorWake] = []
    thread = Thread(
        target=lambda: wakes.append(
            eventwait.wait_for_allocator_event(
                baseline, (datetime.now(UTC) + timedelta(seconds=10)).isoformat()
            )
        ),
        daemon=True,
    )

    thread.start()
    config.mark_task_backend_changed("test", root=backend)
    thread.join(timeout=5.0)

    assert [wake.kind for wake in wakes] == ["task"]
    assert [wake.task_token for wake in wakes] == [eventwait.task_event_token()]


def test_allocator_wait_wakes_on_published_steering(wait_backend):
    repo, _backend = wait_backend
    baseline = eventwait.task_event_token()
    wakes: list[eventwait.AllocatorWake] = []
    thread = Thread(
        target=lambda: wakes.append(
            eventwait.wait_for_allocator_event(
                baseline, (datetime.now(UTC) + timedelta(seconds=10)).isoformat()
            )
        ),
        daemon=True,
    )

    thread.start()
    write_inbox_item(repo, "1kG3test.txt", "operator steering")
    thread.join(timeout=5.0)

    assert [wake.kind for wake in wakes] == ["steering"]
    assert [wake.task_token for wake in wakes] == [baseline]


def test_allocator_wait_wakes_at_peer_claim_deadline(wait_backend):
    baseline = eventwait.task_event_token()
    deadline = datetime.now(UTC) + timedelta(milliseconds=100)

    wake = eventwait.wait_for_allocator_event(baseline, deadline.isoformat())

    assert wake == eventwait.AllocatorWake("deadline", baseline)
