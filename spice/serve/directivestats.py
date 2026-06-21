"""Per-directive send/ack accounting for lane metrics.

Each operator directive (an inbox steering item, identified by its inbox key)
is one *send*. The agent *acks* a directive when it acknowledges that key. The
two are the same fact recorded once and flipped on acknowledgement, so acked is
always a subset of sends (acked <= sends) — unlike the legacy counters, where
``sends`` counted lane messages and ``acked`` counted steering keys from a
different channel and the two could not be compared.

The directive rows are the stable, append-only, graphable series; per
(agent, team-at-capture) running totals are maintained incrementally so the
lane pane reads an O(1) total instead of recomputing the world on every render.
See docs/studies/serve-team-metric-attribution.md.
"""

from __future__ import annotations

import sqlite3
import time
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Iterable, Mapping, Protocol

from spice.serve.teamids import normalized_id as _normalized_id
from spice.serve.teamschema import METRIC_HISTORY_RETENTION_SECONDS


@dataclass(frozen=True)
class DirectiveTotals:
    sends: int
    acked: int


class _DirectiveStatsStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...

    def _directive_totals_for_agents_locked(
        self,
        connection: sqlite3.Connection,
        agent_ids: Iterable[str],
        *,
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> DirectiveTotals: ...


class DirectiveStatsStoreMixin:
    def record_directive_sent(
        self: _DirectiveStatsStore,
        directive_key: str,
        *,
        agent_id: str,
        team_id: str,
        sent_at: float | None = None,
    ) -> None:
        """Record one sent directive. Idempotent on ``directive_key`` so a
        replayed inbox item never double-counts; the running total is bumped
        only when the row is genuinely new."""
        directive_key = _normalized_id(directive_key, "directive_key")
        agent_id = _normalized_id(agent_id, "agent_id")
        team_id = _normalized_id(team_id, "team_id")
        when = time.time() if sent_at is None else max(0.0, float(sent_at))
        with self.connect() as connection:
            cursor = connection.execute(
                "INSERT OR IGNORE INTO directives "
                "(directive_key, agent_id, team_id, sent_at, acked, acked_at) "
                "VALUES (?, ?, ?, ?, 0, NULL)",
                (directive_key, agent_id, team_id, when),
            )
            if cursor.rowcount:
                connection.execute(
                    "INSERT INTO directive_totals (agent_id, team_id, sends, acked) "
                    "VALUES (?, ?, 1, 0) "
                    "ON CONFLICT(agent_id, team_id) DO UPDATE SET "
                    "sends = directive_totals.sends + 1",
                    (agent_id, team_id),
                )

    def mark_directive_acked(
        self: _DirectiveStatsStore,
        directive_key: str,
        *,
        acked_at: float | None = None,
    ) -> bool:
        """Flip a directive to acknowledged. No-op (returns False) if the key is
        unknown or already acked, so acked can only ever rise toward sends and
        never past it."""
        directive_key = _normalized_id(directive_key, "directive_key")
        when = time.time() if acked_at is None else max(0.0, float(acked_at))
        with self.connect() as connection:
            row = connection.execute(
                "SELECT agent_id, team_id, acked FROM directives "
                "WHERE directive_key = ?",
                (directive_key,),
            ).fetchone()
            if row is None or int(row["acked"]):
                return False
            connection.execute(
                "UPDATE directives SET acked = 1, acked_at = ? WHERE directive_key = ?",
                (when, directive_key),
            )
            connection.execute(
                "UPDATE directive_totals SET acked = directive_totals.acked + 1 "
                "WHERE agent_id = ? AND team_id = ?",
                (str(row["agent_id"]), str(row["team_id"])),
            )
            return True

    def _prune_directive_history_locked(
        self,
        connection: sqlite3.Connection,
        *,
        now: float,
        retention_seconds: int = METRIC_HISTORY_RETENTION_SECONDS,
    ) -> None:
        # Drop directive ROWS past the retention horizon; the running totals
        # (directive_totals) are the durable aggregate and stay. A later ack of a
        # pruned key is a harmless no-op (the send was already counted).
        floor = float(now) - max(0, int(retention_seconds))
        connection.execute("DELETE FROM directives WHERE sent_at < ?", (floor,))

    def _rewrite_directive_stats_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
    ) -> None:
        # Renewal folds the predecessor's directives into the successor (the
        # canonical actor) so sends/acks accumulate across the lineage and only
        # one id survives — same id-unification as the other per-agent stores.
        old_agent_id = _normalized_id(old_agent_id, "old_agent_id")
        new_agent_id = _normalized_id(new_agent_id, "new_agent_id")
        if old_agent_id == new_agent_id:
            return
        connection.execute(
            "UPDATE directives SET agent_id = ? WHERE agent_id = ?",
            (new_agent_id, old_agent_id),
        )
        connection.execute(
            "INSERT INTO directive_totals (agent_id, team_id, sends, acked) "
            "SELECT ?, team_id, sends, acked FROM directive_totals "
            "WHERE agent_id = ? "
            "ON CONFLICT(agent_id, team_id) DO UPDATE SET "
            "sends = directive_totals.sends + excluded.sends, "
            "acked = directive_totals.acked + excluded.acked",
            (new_agent_id, old_agent_id),
        )
        connection.execute(
            "DELETE FROM directive_totals WHERE agent_id = ?", (old_agent_id,)
        )

    def directive_totals_for_agents(
        self: _DirectiveStatsStore, agent_ids: Iterable[str]
    ) -> DirectiveTotals:
        """Running send/ack totals summed over the given agents across every
        team they have been tagged under (per-agent lifetime). Team-scoped
        history is available by filtering directive_totals.team_id."""
        with self.connect() as connection:
            return self._directive_totals_for_agents_locked(connection, agent_ids)

    def _directive_totals_for_agents_locked(
        self,
        connection: sqlite3.Connection,
        agent_ids: Iterable[str],
        *,
        start_time_by_agent: Mapping[str, float] | None = None,
    ) -> DirectiveTotals:
        ids = tuple(dict.fromkeys(str(agent_id) for agent_id in agent_ids if agent_id))
        if not ids:
            return DirectiveTotals(sends=0, acked=0)
        start_times = _directive_start_times(ids, start_time_by_agent)
        if start_times:
            return _directive_totals_since_locked(connection, ids, start_times)
        placeholders = ",".join("?" for _ in ids)
        row = connection.execute(
            "SELECT COALESCE(SUM(sends), 0) AS sends, "
            "COALESCE(SUM(acked), 0) AS acked "
            f"FROM directive_totals WHERE agent_id IN ({placeholders})",
            ids,
        ).fetchone()
        return DirectiveTotals(
            sends=int(row["sends"] or 0) if row else 0,
            acked=int(row["acked"] or 0) if row else 0,
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


def _directive_totals_since_locked(
    connection: sqlite3.Connection,
    agent_ids: tuple[str, ...],
    start_time_by_agent: Mapping[str, float],
) -> DirectiveTotals:
    placeholders = ",".join("?" for _ in agent_ids)
    rows = connection.execute(
        "SELECT agent_id, sent_at, acked FROM directives "
        f"WHERE agent_id IN ({placeholders}) ORDER BY sent_at",
        agent_ids,
    ).fetchall()
    sends = 0
    acked = 0
    for row in rows:
        agent_id = str(row["agent_id"])
        sent_at = float(row["sent_at"] or 0.0)
        if sent_at < start_time_by_agent.get(agent_id, 0.0):
            continue
        sends += 1
        acked += 1 if int(row["acked"] or 0) else 0
    return DirectiveTotals(sends=sends, acked=acked)
