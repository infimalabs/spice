"""Revision-coherent Serve task-board observation tests."""

from __future__ import annotations

import gc
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
from spice.tasks import config as task_config, create
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _serve_state,
    _target,
)

CROSSING_REVISION = "crossing"
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


def _task_derived_slice(payload: dict) -> tuple:
    return (
        payload["taskFilterInventory"],
        payload["statusLine"]["claimedTask"],
        payload["laneInfo"]["reviewPressure"],
        _task_cards(payload),
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
    assert payload["taskFilterInventory"]["openTaskCount"] == 2
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
    assert empty["taskFilterInventory"]["openTaskCount"] == 0
    assert _task_derived_slice(repeated) == _task_derived_slice(empty)
    assert mutated["taskFilterInventory"]["openTaskCount"] == 1
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
