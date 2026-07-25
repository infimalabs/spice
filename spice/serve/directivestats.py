"""Read-only Serve projections over canonical steering/ACK lifecycle facts.

Directive publication and retirement are owned by ``spice.mail.ackstate``.
Serve joins those immutable actor/team-at-send facts with team lineage at read
time; it owns no directive rows, counters, ACK mutation, or retention policy.
"""

from __future__ import annotations

import sqlite3
from contextlib import AbstractContextManager
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping, Protocol

from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_PENDING,
    ACK_DISPOSITION_REFUSED,
    DirectiveHistoryRecord,
    directive_history_records_from_database,
)


@dataclass(frozen=True)
class DirectiveTotals:
    sends: int
    acked: int


@dataclass(frozen=True)
class DirectiveLifecycleSummary:
    sends: int
    acked: int
    refused: int
    pending: int
    minimum_latency_seconds: float | None
    maximum_latency_seconds: float | None

    @property
    def totals(self) -> DirectiveTotals:
        return DirectiveTotals(sends=self.sends, acked=self.acked)


class _DirectiveStatsStore(Protocol):
    directive_state_path: Path

    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...

    def _directive_totals_for_agents_locked(
        self,
        connection: sqlite3.Connection,
        agent_ids: Iterable[str],
        *,
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> DirectiveTotals: ...


class DirectiveStatsStoreMixin:
    """Serve read projections over native directive facts."""

    def directive_totals_for_agents(
        self: _DirectiveStatsStore, agent_ids: Iterable[str]
    ) -> DirectiveTotals:
        return directive_lifecycle_summary_for_agents(
            self.directive_state_path, agent_ids
        ).totals

    def directive_lifecycle_summary_for_agents(
        self: _DirectiveStatsStore,
        agent_ids: Iterable[str],
        *,
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> DirectiveLifecycleSummary:
        return directive_lifecycle_summary_for_agents(
            self.directive_state_path,
            agent_ids,
            start_time_by_agent=start_time_by_agent,
        )

    def _directive_totals_for_agents_locked(
        self: _DirectiveStatsStore,
        connection: sqlite3.Connection,
        agent_ids: Iterable[str],
        *,
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> DirectiveTotals:
        del connection
        return directive_lifecycle_summary_for_agents(
            self.directive_state_path,
            agent_ids,
            start_time_by_agent=start_time_by_agent,
        ).totals


def directive_lifecycle_summary_for_agents(
    path: str | Path,
    agent_ids: Iterable[str],
    *,
    start_time_by_agent: Mapping[str, float] | None = None,
) -> DirectiveLifecycleSummary:
    ids = tuple(dict.fromkeys(str(agent_id) for agent_id in agent_ids if agent_id))
    if not ids:
        return _empty_lifecycle_summary()
    starts = _directive_start_times(ids, start_time_by_agent)
    records = (
        record
        for record in directive_history_records_from_database(path)
        if record.target_actor in ids and _record_is_in_session(record, starts)
    )
    return _lifecycle_summary(records)


def directive_history_for_subject(
    path: str | Path,
    *,
    agent_ids: Iterable[str] = (),
    team_ids: Iterable[str] = (),
    start: float,
    end: float,
) -> tuple[DirectiveHistoryRecord, ...]:
    agents = frozenset(str(agent_id) for agent_id in agent_ids if agent_id)
    teams = frozenset(str(team_id) for team_id in team_ids if team_id)
    floor = max(0.0, float(start))
    ceiling = max(floor, float(end))
    return tuple(
        record
        for record in directive_history_records_from_database(path)
        if (
            (record.sent_at is not None and floor <= record.sent_at <= ceiling)
            or (
                record.acknowledged_at is not None
                and floor <= record.acknowledged_at <= ceiling
            )
        )
        and (
            (agents and record.target_actor in agents)
            or (teams and record.team_id in teams)
        )
    )


def _lifecycle_summary(
    records: Iterable[DirectiveHistoryRecord],
) -> DirectiveLifecycleSummary:
    sends = 0
    acked = 0
    refused = 0
    pending = 0
    latencies: list[float] = []
    for record in records:
        sends += 1
        if record.disposition == ACK_DISPOSITION_ACKED:
            acked += 1
        elif record.disposition == ACK_DISPOSITION_REFUSED:
            refused += 1
        elif record.disposition == ACK_DISPOSITION_PENDING:
            pending += 1
        if record.acknowledged_at is not None and record.sent_at is not None:
            latencies.append(max(0.0, record.acknowledged_at - record.sent_at))
    return DirectiveLifecycleSummary(
        sends=sends,
        acked=acked,
        refused=refused,
        pending=pending,
        minimum_latency_seconds=min(latencies) if latencies else None,
        maximum_latency_seconds=max(latencies) if latencies else None,
    )


def _empty_lifecycle_summary() -> DirectiveLifecycleSummary:
    return DirectiveLifecycleSummary(
        sends=0,
        acked=0,
        refused=0,
        pending=0,
        minimum_latency_seconds=None,
        maximum_latency_seconds=None,
    )


def _directive_start_times(
    agent_ids: tuple[str, ...],
    start_time_by_agent: Mapping[str, float] | None,
) -> dict[str, float]:
    if not start_time_by_agent:
        return {}
    return {
        agent_id: max(0.0, float(start_time_by_agent[agent_id]))
        for agent_id in agent_ids
        if agent_id in start_time_by_agent
    }


def _record_is_in_session(
    record: DirectiveHistoryRecord, start_time_by_agent: Mapping[str, float]
) -> bool:
    if not start_time_by_agent:
        return True
    if record.sent_at is None:
        return False
    return record.sent_at >= start_time_by_agent.get(record.target_actor, 0.0)
