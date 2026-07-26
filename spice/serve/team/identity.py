"""Durable serve agent identity storage helpers."""

from __future__ import annotations

import sqlite3
import time
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Protocol

from spice.serve.team.ids import normalized_id as _normalized_id
from spice.serve.team.ids import target_actor_id as _target_actor_id
from spice.serve.team.models import TeamAgentIdentity


class _OmittedIdentityField:
    """Marker for a public identity field the caller did not supply."""


_OMITTED_IDENTITY_FIELD = _OmittedIdentityField()
type _IdentityTextArgument = str | None | _OmittedIdentityField
type _IdentityRevisionArgument = int | None | _OmittedIdentityField


@dataclass(frozen=True, slots=True)
class AgentIdentityRecordRequest:
    """One requested identity write, before anything about it is trusted.

    Field-for-field this looks like ``TeamAgentIdentity`` and the resemblance
    is the point: this is what a caller asks the row to become, and that is a
    different thing from what the row is. Text here may carry surrounding
    whitespace, the actor id may not be canonical, and the revision may be
    negative, because none of it has been through the store yet.

    ``updated_at`` is where the two records genuinely part. None means "stamp
    this write at the time it lands", which only a request can express; the
    stored record always holds a settled float. That is why the stored record
    cannot stand in here, and why folding the two would erase the distinction
    between an unset timestamp and the epoch.

    ``_identity_from_record_request`` is the only crossing between the two, so
    normalization happens once for every writer instead of at each call site.
    """

    actor_id: str
    target_id: str | None = ""
    thread_id: str | None = ""
    actual_driver: str | None = ""
    actual_model: str | None = ""
    actual_effort: str | None = ""
    actual_service_tier: str | None = ""
    desired_driver: str | None = ""
    desired_model: str | None = ""
    desired_effort: str | None = ""
    transcript_owner: str | None = ""
    renewal_state: str | None = ""
    renewal_ancestor_thread_id: str | None = ""
    renewal_successor_thread_id: str | None = ""
    renewal_revision: int | None = 0
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


def _clean_record_text(value: str | None) -> str:
    return str(value or "").strip()


def _nonnegative_record_int(value: int | None) -> int:
    return max(0, int(value or 0))


def _record_updated_at(value: float | None) -> float:
    return time.time() if value is None else float(value)


def _preserved_identity_text(value: _IdentityTextArgument, current: str) -> str | None:
    return current if isinstance(value, _OmittedIdentityField) else value


def _preserved_identity_revision(
    value: _IdentityRevisionArgument, current: int
) -> int | None:
    return current if isinstance(value, _OmittedIdentityField) else value


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
        """Write the requested identity, replacing every column of the row.

        The upsert sets all sixteen columns from the request, so this states
        the whole row rather than the part of it a caller happens to care
        about. A request built from a subset of the fields therefore erases
        the rest instead of leaving them alone, which is silent -- the write
        succeeds and the omitted facts are simply gone. Callers holding an
        existing row must carry its fields forward, as
        ``_update_agent_identity_renewal_locked`` does.
        """
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
        target_id: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        thread_id: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        actual_driver: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        actual_model: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        actual_effort: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        actual_service_tier: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        desired_driver: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        desired_model: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        desired_effort: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        transcript_owner: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        renewal_state: _IdentityTextArgument = _OMITTED_IDENTITY_FIELD,
        renewal_ancestor_thread_id: _IdentityTextArgument = (_OMITTED_IDENTITY_FIELD),
        renewal_successor_thread_id: _IdentityTextArgument = (_OMITTED_IDENTITY_FIELD),
        renewal_revision: _IdentityRevisionArgument = _OMITTED_IDENTITY_FIELD,
    ) -> TeamAgentIdentity:
        """Merge a public identity write without confusing omission with clearing.

        A keyword the caller omits preserves that column from the existing row;
        a new actor starts from canonical empty text and revision zero. Any
        explicitly supplied value remains a write: None or blank text clears a
        text column, and zero clears the revision. The private full-record
        request and locked writer continue to replace every column.
        """
        actor_id = _normalized_id(actor_id, "actor_id")
        with self.connect() as connection:
            # Preserve and replace are one atomic write decision: a renewal
            # writer cannot land after this read and be erased by the merged
            # full-row upsert below.
            connection.execute("BEGIN IMMEDIATE")
            row = self._agent_identity_row_locked(connection, actor_id)
            current = (
                agent_identity_from_row(row)
                if row is not None
                else TeamAgentIdentity(actor_id=actor_id)
            )
            return self._record_agent_identity_locked(
                connection,
                AgentIdentityRecordRequest(
                    actor_id=actor_id,
                    target_id=_preserved_identity_text(target_id, current.target_id),
                    thread_id=_preserved_identity_text(thread_id, current.thread_id),
                    actual_driver=_preserved_identity_text(
                        actual_driver, current.actual_driver
                    ),
                    actual_model=_preserved_identity_text(
                        actual_model, current.actual_model
                    ),
                    actual_effort=_preserved_identity_text(
                        actual_effort, current.actual_effort
                    ),
                    actual_service_tier=_preserved_identity_text(
                        actual_service_tier, current.actual_service_tier
                    ),
                    desired_driver=_preserved_identity_text(
                        desired_driver, current.desired_driver
                    ),
                    desired_model=_preserved_identity_text(
                        desired_model, current.desired_model
                    ),
                    desired_effort=_preserved_identity_text(
                        desired_effort, current.desired_effort
                    ),
                    transcript_owner=_preserved_identity_text(
                        transcript_owner, current.transcript_owner
                    ),
                    renewal_state=_preserved_identity_text(
                        renewal_state, current.renewal_state
                    ),
                    renewal_ancestor_thread_id=_preserved_identity_text(
                        renewal_ancestor_thread_id,
                        current.renewal_ancestor_thread_id,
                    ),
                    renewal_successor_thread_id=_preserved_identity_text(
                        renewal_successor_thread_id,
                        current.renewal_successor_thread_id,
                    ),
                    renewal_revision=_preserved_identity_revision(
                        renewal_revision, current.renewal_revision
                    ),
                ),
            )

    def agent_identity_for_actor(
        self: _TeamIdentityStore, actor_id: str
    ) -> TeamAgentIdentity | None:
        actor_id = _normalized_id(actor_id, "actor_id")
        with self.connect() as connection:
            row = self._agent_identity_row_locked(connection, actor_id)
            return agent_identity_from_row(row) if row is not None else None

    def team_membership_actors_for_target(
        self: _TeamIdentityStore, target_id: str
    ) -> list[str]:
        """The actor(s) that CURRENTLY hold a team slot for a target.

        A driver switch is an implicit renewal: the new thread inherits the
        one slot the target already occupies, whatever actor form holds it
        (the target actor, or an earlier thread of the same target). Bounded
        by the current roster -- never the target's full thread history -- so
        the alias set a successor carries cannot grow with each switch. A
        clean roster yields exactly one; the query also surfaces any stale
        duplicate so a single bind collapses it.
        """
        target = str(target_id or "").strip()
        if not target:
            return []
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT m.agent_id FROM memberships m "
                "LEFT JOIN agent_identities i ON i.actor_id = m.agent_id "
                "WHERE m.agent_id = ? OR i.target_id = ?",
                (_target_actor_id(target), target),
            ).fetchall()
        return [str(row["agent_id"]) for row in rows]


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
