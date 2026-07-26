"""Automatic SLA due dates survive the Taskwarrior UTC/local date boundary."""

from __future__ import annotations

import shutil
from datetime import UTC, datetime, timedelta
from zoneinfo import ZoneInfo

import pytest

from spice.tasks import alloc, config, create, identity, ops, tw
from tests.test_reposcaffolding import (
    init_committed_repo as _init_repo,
)
from tests.test_reposcaffolding import (
    make_task_repo_fixture,
)

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ORIGIN = "ack:1jN54zJJ"
TZ_CHICAGO = "America/Chicago"
TZ_UTC = "UTC"

# Aware instants with sub-second noise, pinned on each side of the
# America/Chicago 2026-03-08 spring-forward transition.
CST_INSTANT = datetime(2026, 3, 7, 18, 0, 0, 123456, tzinfo=UTC)
CDT_INSTANT = datetime(2026, 7, 10, 18, 0, 0, 654321, tzinfo=UTC)
OOPS_INSTANT = datetime(2026, 7, 10, 18, 0, 0, tzinfo=UTC)


task_repo = make_task_repo_fixture(lambda path: _init_repo(path), actor=ACTOR_A)


class _FrozenClock:
    """Stand-in for tw.datetime pinning now() to one aware instant."""

    def __init__(self, instant: datetime) -> None:
        self.instant = instant

    def now(self, tz: object) -> datetime:
        return self.instant.astimezone(tz)


def _freeze(monkeypatch, instant: datetime) -> None:
    monkeypatch.setattr(tw, "datetime", _FrozenClock(instant))


def _stored_due(handle: str) -> str:
    return str(identity.resolve(handle)["due"])


def _expected_due(instant: datetime, priority_key: str) -> str:
    delta = timedelta(seconds=config.SLA_DUE_SECONDS[priority_key])
    return (instant + delta).strftime(tw.TW_DATETIME_FORMAT)


def test_canonical_utc_handles_utc_and_chicago_across_dst():
    chicago = ZoneInfo(TZ_CHICAGO)
    cst_noon = datetime(2026, 3, 7, 12, 0, 0, tzinfo=chicago)
    cdt_noon = datetime(2026, 3, 9, 12, 0, 0, tzinfo=chicago)

    assert tw.canonical_utc(cst_noon) == "20260307T180000Z"
    assert tw.canonical_utc(cdt_noon) == "20260309T170000Z"
    assert tw.canonical_utc(cst_noon.astimezone(UTC)) == "20260307T180000Z"
    assert tw.canonical_utc(cdt_noon.astimezone(UTC)) == "20260309T170000Z"


def test_oops_wait_is_exactly_one_week_from_canonical_inception(task_repo, monkeypatch):
    inception_millis = int(OOPS_INSTANT.timestamp() * identity.MILLIS_PER_SECOND)
    monkeypatch.setattr(
        identity,
        "epoch_millis",
        lambda when=None: inception_millis,
    )

    created = ops.oops("One-week oops wait", origin=ORIGIN)
    row = identity.resolve(created.split()[1])
    waiting_at = datetime.strptime(str(row["wait"]), tw.TW_DATETIME_FORMAT).replace(
        tzinfo=UTC
    )

    assert waiting_at - identity.incepted_datetime(str(row["incepted"])) == timedelta(
        days=7
    )
    assert identity.uuid_of(row) in {
        identity.uuid_of(oops_row) for oops_row in alloc.oops_rows()
    }


def test_sla_due_stores_creation_plus_interval_under_chicago(task_repo, monkeypatch):
    monkeypatch.setenv("TZ", TZ_CHICAGO)
    _freeze(monkeypatch, CDT_INSTANT)

    for priority, key in (
        ("critical", "C"),
        ("high", "H"),
        ("medium", "M"),
        ("low", "L"),
    ):
        handle = create.add(
            f"Deadline {priority}",
            project="task.unit",
            origin=ORIGIN,
            priority=priority,
        )
        assert _stored_due(handle) == _expected_due(CDT_INSTANT, key)


def test_sla_due_is_identical_under_utc_and_chicago(task_repo, monkeypatch):
    _freeze(monkeypatch, CDT_INSTANT)

    monkeypatch.setenv("TZ", TZ_UTC)
    utc_handle = create.add(
        "Deadline utc process", project="task.unit", origin=ORIGIN, priority="medium"
    )
    monkeypatch.setenv("TZ", TZ_CHICAGO)
    chicago_handle = create.add(
        "Deadline chicago process",
        project="task.unit",
        origin=ORIGIN,
        priority="medium",
    )

    assert _stored_due(utc_handle) == _expected_due(CDT_INSTANT, "M")
    assert _stored_due(chicago_handle) == _stored_due(utc_handle)


def test_sla_due_stays_exact_on_both_sides_of_dst_transition(task_repo, monkeypatch):
    monkeypatch.setenv("TZ", TZ_CHICAGO)

    # The high-priority interval from the CST side crosses the spring-forward
    # wall-clock jump and must still persist exactly 86400 real seconds.
    _freeze(monkeypatch, CST_INSTANT)
    cst_handle = create.add(
        "Deadline cst side", project="task.unit", origin=ORIGIN, priority="high"
    )
    _freeze(monkeypatch, CDT_INSTANT)
    cdt_handle = create.add(
        "Deadline cdt side", project="task.unit", origin=ORIGIN, priority="high"
    )

    assert _stored_due(cst_handle) == _expected_due(CST_INSTANT, "H")
    assert _stored_due(cdt_handle) == _expected_due(CDT_INSTANT, "H")


def test_explicit_due_values_retain_declared_semantics(task_repo, monkeypatch):
    monkeypatch.setenv("TZ", TZ_CHICAGO)

    utc_handle = create.add(
        "Explicit utc due",
        project="task.unit",
        origin=ORIGIN,
        priority="medium",
        due="20301231T235959Z",
    )
    local_handle = create.add(
        "Explicit local due",
        project="task.unit",
        origin=ORIGIN,
        priority="medium",
        due="2030-06-15T12:00:00",
    )

    assert _stored_due(utc_handle) == "20301231T235959Z"
    # Naive explicit values keep Taskwarrior's local-time reading: noon CDT.
    assert _stored_due(local_handle) == "20300615T170000Z"
    assert _stored_due(local_handle) != "20300615T120000Z"
