"""Parity between the folded task-board projection and a pre-fold computation."""

from __future__ import annotations

from collections.abc import Mapping
from datetime import UTC, datetime

import pytest

from spice.serve import taskboard
from spice.serve.payload import lane, message
from tests.test_servehelpers import (
    THREAD_A,
    _patch_agent_status,
    _repo,
    _serve_state,
    _target,
    _task_cards,
)

# Each battery below reads one board, so its generation only has to be distinct
# from every other battery's: a shared generation would let one fold answer for
# rows another one seeded.
SEMANTIC_EDGE_GENERATION = "1785044000000300"
SEMANTIC_ERROR_GENERATION = "1785044000000400"
OPEN_STATE_GENERATION = "1785044000000500"
ACTIVE_CLAIM_GENERATION = "1785044000000600"
SHARED_INDEX_GENERATION = "1785044000000700"

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


def test_failed_projection_withholds_inventory_and_empties_task_views():
    review_actors = {THREAD_A, f"thread:{THREAD_A}"}
    failed = taskboard.open_task_board_projection(
        taskboard.TaskBoardObservation(
            backend_identity="differential",
            revision=SEMANTIC_ERROR_GENERATION,
            rows=(),
            error="backend unavailable",
        )
    )

    # Every index answers empty, but the inventory is absent rather than empty:
    # it reaches a browser that orders it by this revision, and an empty board
    # stamped with the live one would refuse the recovery that follows it.
    assert failed.task_filter_inventory is None
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
