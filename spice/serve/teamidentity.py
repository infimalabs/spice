"""Durable serve agent identity storage helpers."""

from __future__ import annotations

import sqlite3
import time

from spice.serve.teamids import normalized_id as _normalized_id
from spice.serve.teammodels import TeamAgentIdentity


class TeamIdentityStoreMixin:
    def _migrate_agent_identity_backfill_locked(
        self, connection: sqlite3.Connection
    ) -> None:
        agent_ids = {
            str(row["agent_id"])
            for row in connection.execute("SELECT agent_id FROM memberships")
        }
        agent_ids.update(
            str(row["agent_id"])
            for row in connection.execute("SELECT agent_id FROM renewals")
        )
        if not agent_ids:
            return
        now = time.time()
        for agent_id in sorted(agent_ids):
            if self._agent_identity_row_locked(connection, agent_id) is not None:
                continue
            renewal = self._renewal_state_locked(connection, agent_id)
            self._record_agent_identity_locked(
                connection,
                actor_id=agent_id,
                target_id=target_id_from_actor(agent_id),
                thread_id=thread_id_from_actor(agent_id),
                renewal_state=renewal.state if renewal is not None else "",
                renewal_ancestor_thread_id=(
                    renewal.ancestor_thread_id if renewal is not None else ""
                ),
                renewal_successor_thread_id=(
                    renewal.successor_agent_id if renewal is not None else ""
                ),
                renewal_revision=renewal.revision if renewal is not None else 0,
                updated_at=now,
            )

    def _agent_identity_row_locked(
        self, connection: sqlite3.Connection, actor_id: str
    ) -> sqlite3.Row | None:
        return connection.execute(
            "SELECT actor_id, target_id, thread_id, actual_driver, actual_model, "
            "actual_effort, actual_service_tier, desired_driver, desired_model, "
            "desired_effort, transcript_owner, renewal_state, "
            "renewal_ancestor_thread_id, renewal_successor_thread_id, "
            "renewal_revision, updated_at FROM agent_identities WHERE actor_id = ?",
            (actor_id,),
        ).fetchone()

    def _record_agent_identity_locked(
        self,
        connection: sqlite3.Connection,
        *,
        actor_id: str,
        target_id: str = "",
        thread_id: str = "",
        actual_driver: str = "",
        actual_model: str = "",
        actual_effort: str = "",
        actual_service_tier: str = "",
        desired_driver: str = "",
        desired_model: str = "",
        desired_effort: str = "",
        transcript_owner: str = "",
        renewal_state: str = "",
        renewal_ancestor_thread_id: str = "",
        renewal_successor_thread_id: str = "",
        renewal_revision: int = 0,
        updated_at: float | None = None,
    ) -> TeamAgentIdentity:
        identity = TeamAgentIdentity(
            actor_id=_normalized_id(actor_id, "actor_id"),
            target_id=str(target_id or "").strip(),
            thread_id=str(thread_id or "").strip(),
            actual_driver=str(actual_driver or "").strip(),
            actual_model=str(actual_model or "").strip(),
            actual_effort=str(actual_effort or "").strip(),
            actual_service_tier=str(actual_service_tier or "").strip(),
            desired_driver=str(desired_driver or "").strip(),
            desired_model=str(desired_model or "").strip(),
            desired_effort=str(desired_effort or "").strip(),
            transcript_owner=str(transcript_owner or "").strip(),
            renewal_state=str(renewal_state or "").strip(),
            renewal_ancestor_thread_id=str(renewal_ancestor_thread_id or "").strip(),
            renewal_successor_thread_id=str(renewal_successor_thread_id or "").strip(),
            renewal_revision=max(0, int(renewal_revision or 0)),
            updated_at=time.time() if updated_at is None else float(updated_at),
        )
        connection.execute(
            "INSERT INTO agent_identities (actor_id, target_id, thread_id, "
            "actual_driver, actual_model, actual_effort, actual_service_tier, "
            "desired_driver, desired_model, desired_effort, transcript_owner, "
            "renewal_state, renewal_ancestor_thread_id, "
            "renewal_successor_thread_id, renewal_revision, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) "
            "ON CONFLICT(actor_id) DO UPDATE SET "
            "target_id = excluded.target_id, "
            "thread_id = excluded.thread_id, "
            "actual_driver = excluded.actual_driver, "
            "actual_model = excluded.actual_model, "
            "actual_effort = excluded.actual_effort, "
            "actual_service_tier = excluded.actual_service_tier, "
            "desired_driver = excluded.desired_driver, "
            "desired_model = excluded.desired_model, "
            "desired_effort = excluded.desired_effort, "
            "transcript_owner = excluded.transcript_owner, "
            "renewal_state = excluded.renewal_state, "
            "renewal_ancestor_thread_id = excluded.renewal_ancestor_thread_id, "
            "renewal_successor_thread_id = excluded.renewal_successor_thread_id, "
            "renewal_revision = excluded.renewal_revision, "
            "updated_at = excluded.updated_at",
            (
                identity.actor_id,
                identity.target_id,
                identity.thread_id,
                identity.actual_driver,
                identity.actual_model,
                identity.actual_effort,
                identity.actual_service_tier,
                identity.desired_driver,
                identity.desired_model,
                identity.desired_effort,
                identity.transcript_owner,
                identity.renewal_state,
                identity.renewal_ancestor_thread_id,
                identity.renewal_successor_thread_id,
                identity.renewal_revision,
                identity.updated_at,
            ),
        )
        return identity

    def _update_agent_identity_renewal_locked(
        self,
        connection: sqlite3.Connection,
        *,
        actor_id: str,
        state: str = "",
        ancestor_thread_id: str = "",
        successor_thread_id: str = "",
        revision: int = 0,
    ) -> None:
        actor_id = _normalized_id(actor_id, "actor_id")
        existing = self._agent_identity_row_locked(connection, actor_id)
        if existing is None:
            self._record_agent_identity_locked(
                connection,
                actor_id=actor_id,
                target_id=target_id_from_actor(actor_id),
                thread_id=thread_id_from_actor(actor_id),
                renewal_state=state,
                renewal_ancestor_thread_id=ancestor_thread_id,
                renewal_successor_thread_id=successor_thread_id,
                renewal_revision=revision,
            )
            return
        identity = agent_identity_from_row(existing)
        self._record_agent_identity_locked(
            connection,
            actor_id=identity.actor_id,
            target_id=identity.target_id,
            thread_id=identity.thread_id,
            actual_driver=identity.actual_driver,
            actual_model=identity.actual_model,
            actual_effort=identity.actual_effort,
            actual_service_tier=identity.actual_service_tier,
            desired_driver=identity.desired_driver,
            desired_model=identity.desired_model,
            desired_effort=identity.desired_effort,
            transcript_owner=identity.transcript_owner,
            renewal_state=state,
            renewal_ancestor_thread_id=ancestor_thread_id,
            renewal_successor_thread_id=successor_thread_id,
            renewal_revision=revision,
        )

    def record_agent_identity(
        self,
        *,
        actor_id: str,
        target_id: str = "",
        thread_id: str = "",
        actual_driver: str = "",
        actual_model: str = "",
        actual_effort: str = "",
        actual_service_tier: str = "",
        desired_driver: str = "",
        desired_model: str = "",
        desired_effort: str = "",
        transcript_owner: str = "",
        renewal_state: str = "",
        renewal_ancestor_thread_id: str = "",
        renewal_successor_thread_id: str = "",
        renewal_revision: int = 0,
    ) -> TeamAgentIdentity:
        with self.connect() as connection:
            return self._record_agent_identity_locked(
                connection,
                actor_id=actor_id,
                target_id=target_id,
                thread_id=thread_id,
                actual_driver=actual_driver,
                actual_model=actual_model,
                actual_effort=actual_effort,
                actual_service_tier=actual_service_tier,
                desired_driver=desired_driver,
                desired_model=desired_model,
                desired_effort=desired_effort,
                transcript_owner=transcript_owner,
                renewal_state=renewal_state,
                renewal_ancestor_thread_id=renewal_ancestor_thread_id,
                renewal_successor_thread_id=renewal_successor_thread_id,
                renewal_revision=renewal_revision,
            )

    def agent_identity_for_actor(self, actor_id: str) -> TeamAgentIdentity | None:
        actor_id = _normalized_id(actor_id, "actor_id")
        with self.connect() as connection:
            row = self._agent_identity_row_locked(connection, actor_id)
            return agent_identity_from_row(row) if row is not None else None

    def _rewrite_agent_identity_alias_locked(
        self,
        connection: sqlite3.Connection,
        old_actor_id: str,
        new_actor_id: str,
    ) -> None:
        old_row = self._agent_identity_row_locked(connection, old_actor_id)
        if old_row is None:
            return
        new_row = self._agent_identity_row_locked(connection, new_actor_id)
        if new_row is not None:
            connection.execute(
                "DELETE FROM agent_identities WHERE actor_id = ?", (old_actor_id,)
            )
            return
        identity = agent_identity_from_row(old_row)
        self._record_agent_identity_locked(
            connection,
            actor_id=new_actor_id,
            target_id=identity.target_id or target_id_from_actor(old_actor_id),
            thread_id=thread_id_from_actor(new_actor_id) or identity.thread_id,
            actual_driver=identity.actual_driver,
            actual_model=identity.actual_model,
            actual_effort=identity.actual_effort,
            actual_service_tier=identity.actual_service_tier,
            desired_driver=identity.desired_driver,
            desired_model=identity.desired_model,
            desired_effort=identity.desired_effort,
            transcript_owner=identity.transcript_owner,
            renewal_state=identity.renewal_state,
            renewal_ancestor_thread_id=identity.renewal_ancestor_thread_id,
            renewal_successor_thread_id=identity.renewal_successor_thread_id,
            renewal_revision=identity.renewal_revision,
            updated_at=identity.updated_at,
        )
        connection.execute(
            "DELETE FROM agent_identities WHERE actor_id = ?", (old_actor_id,)
        )


def target_id_from_actor(actor_id: str) -> str:
    actor = str(actor_id or "").strip()
    return actor[7:] if actor.startswith("target:") else ""


def thread_id_from_actor(actor_id: str) -> str:
    actor = str(actor_id or "").strip()
    if actor.startswith("thread:"):
        return actor[7:]
    return "" if actor.startswith("target:") else actor


def agent_identity_lookup_ids(agent_id: str) -> tuple[str, ...]:
    agent = str(agent_id or "").strip()
    if not agent or agent.startswith("thread:") or agent.startswith("target:"):
        return (agent,) if agent else ()
    return (f"thread:{agent}", f"target:{agent}", agent)


def identity_for_member(
    identity_by_actor: dict[str, TeamAgentIdentity], member: sqlite3.Row
) -> TeamAgentIdentity | None:
    for actor_id in agent_identity_lookup_ids(str(member["agent_id"])):
        identity = identity_by_actor.get(actor_id)
        if identity is not None:
            return identity
    return None


def agent_identity_from_row(row: sqlite3.Row) -> TeamAgentIdentity:
    return TeamAgentIdentity(
        actor_id=str(row["actor_id"]),
        target_id=str(row["target_id"]),
        thread_id=str(row["thread_id"]),
        actual_driver=str(row["actual_driver"]),
        actual_model=str(row["actual_model"]),
        actual_effort=str(row["actual_effort"]),
        actual_service_tier=str(row["actual_service_tier"]),
        desired_driver=str(row["desired_driver"]),
        desired_model=str(row["desired_model"]),
        desired_effort=str(row["desired_effort"]),
        transcript_owner=str(row["transcript_owner"]),
        renewal_state=str(row["renewal_state"]),
        renewal_ancestor_thread_id=str(row["renewal_ancestor_thread_id"]),
        renewal_successor_thread_id=str(row["renewal_successor_thread_id"]),
        renewal_revision=int(row["renewal_revision"]),
        updated_at=float(row["updated_at"]),
    )


def select_agent_identity_rows(
    connection: sqlite3.Connection, actor_ids: tuple[str, ...]
) -> list[sqlite3.Row]:
    if not actor_ids:
        return []
    placeholders = ",".join("?" for _ in actor_ids)
    return connection.execute(
        "SELECT actor_id, target_id, thread_id, actual_driver, actual_model, "
        "actual_effort, actual_service_tier, desired_driver, desired_model, "
        "desired_effort, transcript_owner, renewal_state, "
        "renewal_ancestor_thread_id, renewal_successor_thread_id, "
        "renewal_revision, updated_at FROM agent_identities "
        f"WHERE actor_id IN ({placeholders})",
        actor_ids,
    ).fetchall()
