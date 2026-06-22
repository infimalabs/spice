"""Durable serve agent identity storage helpers."""

from __future__ import annotations

import sqlite3
import time
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Protocol

from spice.serve.team.ids import normalized_id as _normalized_id
from spice.serve.team.models import TeamAgentIdentity


@dataclass(frozen=True, slots=True)
class AgentIdentityRecordRequest:
    actor_id: str
    target_id: str = ""
    thread_id: str = ""
    actual_driver: str = ""
    actual_model: str = ""
    actual_effort: str = ""
    actual_service_tier: str = ""
    desired_driver: str = ""
    desired_model: str = ""
    desired_effort: str = ""
    transcript_owner: str = ""
    renewal_state: str = ""
    renewal_ancestor_thread_id: str = ""
    renewal_successor_thread_id: str = ""
    renewal_revision: int = 0
    updated_at: float | None = None


class _TeamIdentityStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...

    def _agent_identity_row_locked(
        self, connection: sqlite3.Connection, actor_id: str
    ) -> sqlite3.Row | None: ...

    def _record_agent_identity_locked(
        self,
        connection: sqlite3.Connection,
        request: AgentIdentityRecordRequest,
    ) -> TeamAgentIdentity: ...


def _identity_from_record_request(
    request: AgentIdentityRecordRequest,
) -> TeamAgentIdentity:
    return TeamAgentIdentity(
        actor_id=_normalized_id(request.actor_id, "actor_id"),
        target_id=_clean_record_text(request.target_id),
        thread_id=_clean_record_text(request.thread_id),
        actual_driver=_clean_record_text(request.actual_driver),
        actual_model=_clean_record_text(request.actual_model),
        actual_effort=_clean_record_text(request.actual_effort),
        actual_service_tier=_clean_record_text(request.actual_service_tier),
        desired_driver=_clean_record_text(request.desired_driver),
        desired_model=_clean_record_text(request.desired_model),
        desired_effort=_clean_record_text(request.desired_effort),
        transcript_owner=_clean_record_text(request.transcript_owner),
        renewal_state=_clean_record_text(request.renewal_state),
        renewal_ancestor_thread_id=_clean_record_text(
            request.renewal_ancestor_thread_id
        ),
        renewal_successor_thread_id=_clean_record_text(
            request.renewal_successor_thread_id
        ),
        renewal_revision=_nonnegative_record_int(request.renewal_revision),
        updated_at=_record_updated_at(request.updated_at),
    )


def _clean_record_text(value: str) -> str:
    return str(value or "").strip()


def _nonnegative_record_int(value: int) -> int:
    return max(0, int(value or 0))


def _record_updated_at(value: float | None) -> float:
    return time.time() if value is None else float(value)


class TeamIdentityStoreMixin:
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
        request: AgentIdentityRecordRequest,
    ) -> TeamAgentIdentity:
        identity = _identity_from_record_request(request)
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
                AgentIdentityRecordRequest(
                    actor_id=actor_id,
                    target_id=target_id_from_actor(actor_id),
                    thread_id=thread_id_from_actor(actor_id),
                    renewal_state=state,
                    renewal_ancestor_thread_id=ancestor_thread_id,
                    renewal_successor_thread_id=successor_thread_id,
                    renewal_revision=revision,
                ),
            )
            return
        identity = agent_identity_from_row(existing)
        self._record_agent_identity_locked(
            connection,
            AgentIdentityRecordRequest(
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
            ),
        )

    def record_agent_identity(
        self: _TeamIdentityStore,
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
                AgentIdentityRecordRequest(
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
                ),
            )

    def agent_identity_for_actor(
        self: _TeamIdentityStore, actor_id: str
    ) -> TeamAgentIdentity | None:
        actor_id = _normalized_id(actor_id, "actor_id")
        with self.connect() as connection:
            row = self._agent_identity_row_locked(connection, actor_id)
            return agent_identity_from_row(row) if row is not None else None


def target_id_from_actor(actor_id: str) -> str:
    actor = str(actor_id or "").strip()
    return actor[7:] if actor.startswith("target:") else ""


def thread_id_from_actor(actor_id: str) -> str:
    actor = str(actor_id or "").strip()
    if actor.startswith("thread:"):
        return actor[7:]
    return "" if actor.startswith("target:") else actor


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
