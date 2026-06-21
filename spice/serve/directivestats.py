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
from typing import Iterable, Protocol

from spice.serve.teamids import normalized_id as _normalized_id


@dataclass(frozen=True)
class DirectiveTotals:
    sends: int
    acked: int


class _DirectiveStatsStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...


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

    def directive_totals_for_agents(
        self: _DirectiveStatsStore, agent_ids: Iterable[str]
    ) -> DirectiveTotals:
        """Running send/ack totals summed over the given agents across every
        team they have been tagged under (per-agent lifetime). Team-scoped
        history is available by filtering directive_totals.team_id."""
        ids = tuple(dict.fromkeys(str(agent_id) for agent_id in agent_ids if agent_id))
        if not ids:
            return DirectiveTotals(sends=0, acked=0)
        placeholders = ",".join("?" for _ in ids)
        with self.connect() as connection:
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
