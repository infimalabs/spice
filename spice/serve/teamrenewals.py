"""Serve team renewal storage helpers."""

from __future__ import annotations

import json
import sqlite3
import time
from collections.abc import Iterable, Mapping
from contextlib import AbstractContextManager
from typing import Any, Protocol

from spice.errors import SpiceError
from spice.serve.teamids import normalized_id as _normalized_id
from spice.serve.teamidentity import agent_identity_from_row, thread_id_from_actor
from spice.serve.teammodels import TeamRenewalState
from spice.serve.teamschema import (
    RENEWAL_STATE_PENDING,
    RENEWAL_STATE_REQUESTED,
    RENEWAL_STATE_STARTED,
)

RENEWAL_IDENTITY_COLUMNS = (
    ("successor_thread_id", "TEXT NOT NULL DEFAULT ''"),
    ("team_slot", "INTEGER"),
    ("predecessor_identity", "TEXT NOT NULL DEFAULT '{}'"),
    ("successor_identity", "TEXT NOT NULL DEFAULT '{}'"),
)


class _TeamRenewalStore(Protocol):
    def connect(self) -> AbstractContextManager[sqlite3.Connection]: ...

    def _team_member_ids_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[str, ...]: ...

    def _agent_identity_row_locked(
        self, connection: sqlite3.Connection, actor_id: str
    ) -> sqlite3.Row | None: ...

    def open_team_for_agent(self, agent_id: str) -> str: ...

    def _record_event(
        self,
        connection: sqlite3.Connection,
        kind: str,
        team_id: str,
        payload: dict[str, Any],
    ) -> int: ...

    def _update_agent_identity_renewal_locked(
        self,
        connection: sqlite3.Connection,
        *,
        actor_id: str,
        state: str = "",
        ancestor_thread_id: str = "",
        successor_thread_id: str = "",
        revision: int = 0,
    ) -> None: ...

    def _assign_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> None: ...

    def _team_slot_for_agent_locked(
        self, connection: sqlite3.Connection, team_id: str, agent_id: str
    ) -> int | None: ...

    def _renewal_predecessor_identity_locked(
        self, connection: sqlite3.Connection, actor_id: str
    ) -> dict[str, Any]: ...

    def _renewal_successor_identity(
        self,
        predecessor_identity: Mapping[str, Any],
        *,
        successor_agent_id: str = "",
        successor_thread_id: str = "",
    ) -> dict[str, Any]: ...

    def renewal_state_for_agent(self, agent_id: str) -> TeamRenewalState | None: ...

    def _renewal_state_locked(
        self, connection: sqlite3.Connection, agent_id: str
    ) -> TeamRenewalState | None: ...

    def _replace_team_slot_locked(
        self,
        connection: sqlite3.Connection,
        *,
        team_id: str,
        predecessor_agent_id: str,
        successor_agent_id: str,
        team_slot: int | None,
    ) -> None: ...

    def _started_renewal_facts_locked(
        self,
        connection: sqlite3.Connection,
        *,
        team_id: str,
        predecessor_agent_id: str,
        successor_agent_id: str,
    ) -> tuple[int | None, dict[str, Any], str, dict[str, Any]]: ...

    def _persist_started_renewal_locked(
        self, connection: sqlite3.Connection, renewal: TeamRenewalState
    ) -> None: ...

    def _started_renewal_record(
        self,
        *,
        predecessor_agent_id: str,
        team_id: str,
        ancestor_thread_id: str,
        successor_agent_id: str,
        successor_thread_id: str,
        team_slot: int | None,
        predecessor_identity: dict[str, Any],
        successor_identity: dict[str, Any],
        revision: int,
    ) -> TeamRenewalState: ...


class TeamRenewalStoreMixin:
    def _migrate_renewal_identity_columns_locked(
        self, connection: sqlite3.Connection
    ) -> None:
        columns = {
            str(row["name"])
            for row in connection.execute("PRAGMA table_info(renewals)")
        }
        for column, definition in RENEWAL_IDENTITY_COLUMNS:
            if column not in columns:
                connection.execute(
                    f"ALTER TABLE renewals ADD COLUMN {column} {definition}"
                )
        connection.execute(
            "UPDATE renewals SET successor_thread_id = substr(successor_agent_id, 8) "
            "WHERE successor_thread_id = '' AND successor_agent_id LIKE 'thread:%'"
        )

    def _rewrite_renewal_agent_locked(
        self,
        connection: sqlite3.Connection,
        old_agent_id: str,
        new_agent_id: str,
        team_id: str,
    ) -> None:
        old_row = connection.execute(
            "SELECT agent_id FROM renewals WHERE agent_id = ?", (old_agent_id,)
        ).fetchone()
        if old_row is None:
            return
        new_row = connection.execute(
            "SELECT agent_id FROM renewals WHERE agent_id = ?", (new_agent_id,)
        ).fetchone()
        if new_row is not None:
            connection.execute(
                "DELETE FROM renewals WHERE agent_id = ?", (old_agent_id,)
            )
            return
        connection.execute(
            "UPDATE renewals SET agent_id = ?, team_id = ? WHERE agent_id = ?",
            (new_agent_id, team_id, old_agent_id),
        )

    def _team_slot_for_agent_locked(
        self: _TeamRenewalStore,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
    ) -> int | None:
        member_ids = self._team_member_ids_locked(connection, team_id)
        try:
            return member_ids.index(agent_id)
        except ValueError:
            return None

    def _renewal_predecessor_identity_locked(
        self: _TeamRenewalStore, connection: sqlite3.Connection, actor_id: str
    ) -> dict[str, Any]:
        row = self._agent_identity_row_locked(connection, actor_id)
        if row is None:
            raise SpiceError(
                f"renewal requires stored identity facts for agent {actor_id}"
            )
        return agent_identity_from_row(row).to_payload()

    def _renewal_successor_identity(
        self,
        predecessor_identity: Mapping[str, Any],
        *,
        successor_agent_id: str = "",
        successor_thread_id: str = "",
    ) -> dict[str, Any]:
        thread_id = successor_thread_id or thread_id_from_actor(successor_agent_id)
        desired_driver = str(predecessor_identity.get("desiredDriver") or "")
        desired_model = str(predecessor_identity.get("desiredModel") or "")
        desired_effort = str(predecessor_identity.get("desiredEffort") or "")
        return {
            "actorId": successor_agent_id,
            "targetId": str(predecessor_identity.get("targetId") or ""),
            "threadId": thread_id,
            "driverName": desired_driver,
            "driverModel": desired_model,
            "driverEffort": desired_effort,
            "actualDriver": "",
            "actualModel": "",
            "actualEffort": "",
            "actualServiceTier": "",
            "desiredDriver": desired_driver,
            "desiredModel": desired_model,
            "desiredEffort": desired_effort,
            "transcriptOwner": "",
        }

    def _replace_team_slot_locked(
        self: _TeamRenewalStore,
        connection: sqlite3.Connection,
        *,
        team_id: str,
        predecessor_agent_id: str,
        successor_agent_id: str,
        team_slot: int | None,
    ) -> None:
        member_ids = [
            member_id
            for member_id in self._team_member_ids_locked(connection, team_id)
            if member_id not in {predecessor_agent_id, successor_agent_id}
        ]
        if team_slot is None:
            team_slot = len(member_ids)
        insert_at = max(0, min(team_slot, len(member_ids)))
        member_ids.insert(insert_at, successor_agent_id)
        now = time.time()
        for index, member_id in enumerate(member_ids):
            connection.execute(
                "UPDATE memberships SET joined_at = ? "
                "WHERE team_id = ? AND agent_id = ?",
                (now + index * 0.000001, team_id, member_id),
            )

    def set_agent_renewal_request(
        self: _TeamRenewalStore, agent_id: str, *, requested: bool
    ) -> TeamRenewalState | None:
        agent_id = _normalized_id(agent_id, "agent_id")
        team_id = self.open_team_for_agent(agent_id)
        with self.connect() as connection:
            current = self._renewal_state_locked(connection, agent_id)
            if requested:
                if current is not None:
                    return current
                team_slot = self._team_slot_for_agent_locked(
                    connection, team_id, agent_id
                )
                predecessor_identity = self._renewal_predecessor_identity_locked(
                    connection, agent_id
                )
                successor_identity = self._renewal_successor_identity(
                    predecessor_identity
                )
                revision = self._record_event(
                    connection,
                    "renewalRequested",
                    team_id,
                    {"agentId": agent_id, "teamSlot": team_slot},
                )
                connection.execute(
                    "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
                    "ancestor_thread_id, successor_agent_id, successor_thread_id, "
                    "team_slot, predecessor_identity, successor_identity, revision) "
                    "VALUES (?, ?, ?, '', '', '', ?, ?, ?, ?)",
                    (
                        agent_id,
                        team_id,
                        RENEWAL_STATE_REQUESTED,
                        team_slot,
                        _renewal_identity_json(predecessor_identity),
                        _renewal_identity_json(successor_identity),
                        revision,
                    ),
                )
                self._update_agent_identity_renewal_locked(
                    connection,
                    actor_id=agent_id,
                    state=RENEWAL_STATE_REQUESTED,
                    revision=revision,
                )
                return TeamRenewalState(
                    agent_id=agent_id,
                    team_id=team_id,
                    state=RENEWAL_STATE_REQUESTED,
                    ancestor_thread_id="",
                    successor_agent_id="",
                    successor_thread_id="",
                    team_slot=team_slot,
                    predecessor_identity=predecessor_identity,
                    successor_identity=successor_identity,
                    revision=revision,
                )
            if current is None or current.state != RENEWAL_STATE_REQUESTED:
                return current
            self._record_event(
                connection,
                "renewalRequestCleared",
                current.team_id,
                {"agentId": agent_id},
            )
            connection.execute(
                "DELETE FROM renewals WHERE agent_id = ? AND state = ?",
                (agent_id, RENEWAL_STATE_REQUESTED),
            )
            self._update_agent_identity_renewal_locked(connection, actor_id=agent_id)
            return None

    def agent_renewal_requested(self: _TeamRenewalStore, agent_id: str) -> bool:
        renewal = self.renewal_state_for_agent(agent_id)
        return bool(renewal and renewal.requested)

    def agent_renewal_active(self: _TeamRenewalStore, agent_id: str) -> bool:
        renewal = self.renewal_state_for_agent(agent_id)
        return bool(
            renewal
            and renewal.state in {RENEWAL_STATE_REQUESTED, RENEWAL_STATE_PENDING}
        )

    def renewal_state_for_agent(
        self: _TeamRenewalStore, agent_id: str
    ) -> TeamRenewalState | None:
        agent_id = _normalized_id(agent_id, "agent_id")
        with self.connect() as connection:
            return self._renewal_state_locked(connection, agent_id)

    def record_pending_renewal(
        self: _TeamRenewalStore, *, agent_id: str, ancestor_thread_id: str
    ) -> TeamRenewalState:
        agent_id = _normalized_id(agent_id, "agent_id")
        team_id = self.open_team_for_agent(agent_id)
        with self.connect() as connection:
            team_slot = self._team_slot_for_agent_locked(connection, team_id, agent_id)
            predecessor_identity = self._renewal_predecessor_identity_locked(
                connection, agent_id
            )
            successor_identity = self._renewal_successor_identity(predecessor_identity)
            revision = self._record_event(
                connection,
                "renewalPending",
                team_id,
                {
                    "agentId": agent_id,
                    "ancestor": ancestor_thread_id,
                    "teamSlot": team_slot,
                },
            )
            connection.execute(
                "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
                "ancestor_thread_id, successor_agent_id, successor_thread_id, "
                "team_slot, predecessor_identity, successor_identity, revision) "
                "VALUES (?, ?, ?, ?, '', '', ?, ?, ?, ?)",
                (
                    agent_id,
                    team_id,
                    RENEWAL_STATE_PENDING,
                    ancestor_thread_id,
                    team_slot,
                    _renewal_identity_json(predecessor_identity),
                    _renewal_identity_json(successor_identity),
                    revision,
                ),
            )
            self._update_agent_identity_renewal_locked(
                connection,
                actor_id=agent_id,
                state=RENEWAL_STATE_PENDING,
                ancestor_thread_id=ancestor_thread_id,
                revision=revision,
            )
        return TeamRenewalState(
            agent_id=agent_id,
            team_id=team_id,
            state=RENEWAL_STATE_PENDING,
            ancestor_thread_id=ancestor_thread_id,
            successor_agent_id="",
            successor_thread_id="",
            team_slot=team_slot,
            predecessor_identity=predecessor_identity,
            successor_identity=successor_identity,
            revision=revision,
        )

    def record_started_renewal(
        self: _TeamRenewalStore,
        *,
        predecessor_agent_id: str,
        successor_agent_id: str,
        ancestor_thread_id: str = "",
    ) -> TeamRenewalState:
        predecessor_agent_id = _normalized_id(
            predecessor_agent_id, "predecessor_agent_id"
        )
        successor_agent_id = _normalized_id(successor_agent_id, "successor_agent_id")
        team_id = self.open_team_for_agent(predecessor_agent_id)
        with self.connect() as connection:
            (
                team_slot,
                predecessor_identity,
                successor_thread_id,
                successor_identity,
            ) = self._started_renewal_facts_locked(
                connection,
                team_id=team_id,
                predecessor_agent_id=predecessor_agent_id,
                successor_agent_id=successor_agent_id,
            )
            revision = self._record_event(
                connection,
                "renewalStarted",
                team_id,
                {
                    "predecessor": predecessor_agent_id,
                    "successor": successor_agent_id,
                    "successorThreadId": successor_thread_id,
                    "teamSlot": team_slot,
                },
            )
            self._assign_locked(connection, team_id, successor_agent_id)
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ?", (predecessor_agent_id,)
            )
            self._replace_team_slot_locked(
                connection,
                team_id=team_id,
                predecessor_agent_id=predecessor_agent_id,
                successor_agent_id=successor_agent_id,
                team_slot=team_slot,
            )
            renewal = self._started_renewal_record(
                predecessor_agent_id=predecessor_agent_id,
                team_id=team_id,
                ancestor_thread_id=ancestor_thread_id,
                successor_agent_id=successor_agent_id,
                successor_thread_id=successor_thread_id,
                team_slot=team_slot,
                predecessor_identity=predecessor_identity,
                successor_identity=successor_identity,
                revision=revision,
            )
            self._persist_started_renewal_locked(connection, renewal)
        return renewal

    def _started_renewal_facts_locked(
        self: _TeamRenewalStore,
        connection: sqlite3.Connection,
        *,
        team_id: str,
        predecessor_agent_id: str,
        successor_agent_id: str,
    ) -> tuple[int | None, dict[str, Any], str, dict[str, Any]]:
        current = self._renewal_state_locked(connection, predecessor_agent_id)
        team_slot = (
            current.team_slot
            if current is not None and current.team_slot is not None
            else self._team_slot_for_agent_locked(
                connection, team_id, predecessor_agent_id
            )
        )
        predecessor_identity = (
            current.predecessor_identity
            if current is not None and current.predecessor_identity
            else self._renewal_predecessor_identity_locked(
                connection, predecessor_agent_id
            )
        )
        successor_thread_id = thread_id_from_actor(successor_agent_id)
        successor_identity = self._renewal_successor_identity(
            predecessor_identity,
            successor_agent_id=successor_agent_id,
            successor_thread_id=successor_thread_id,
        )
        return (
            team_slot,
            predecessor_identity,
            successor_thread_id,
            successor_identity,
        )

    def _persist_started_renewal_locked(
        self: _TeamRenewalStore,
        connection: sqlite3.Connection,
        renewal: TeamRenewalState,
    ) -> None:
        connection.execute(
            "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
            "ancestor_thread_id, successor_agent_id, successor_thread_id, "
            "team_slot, predecessor_identity, successor_identity, revision) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                renewal.agent_id,
                renewal.team_id,
                RENEWAL_STATE_STARTED,
                renewal.ancestor_thread_id,
                renewal.successor_agent_id,
                renewal.successor_thread_id,
                renewal.team_slot,
                _renewal_identity_json(renewal.predecessor_identity),
                _renewal_identity_json(renewal.successor_identity),
                renewal.revision,
            ),
        )
        self._update_agent_identity_renewal_locked(
            connection,
            actor_id=renewal.agent_id,
            state=RENEWAL_STATE_STARTED,
            ancestor_thread_id=renewal.ancestor_thread_id,
            successor_thread_id=renewal.successor_thread_id,
            revision=renewal.revision,
        )

    def _started_renewal_record(
        self,
        *,
        predecessor_agent_id: str,
        team_id: str,
        ancestor_thread_id: str,
        successor_agent_id: str,
        successor_thread_id: str,
        team_slot: int | None,
        predecessor_identity: dict[str, Any],
        successor_identity: dict[str, Any],
        revision: int,
    ) -> TeamRenewalState:
        return TeamRenewalState(
            agent_id=predecessor_agent_id,
            team_id=team_id,
            state=RENEWAL_STATE_STARTED,
            ancestor_thread_id=ancestor_thread_id,
            successor_agent_id=successor_agent_id,
            successor_thread_id=successor_thread_id,
            team_slot=team_slot,
            predecessor_identity=predecessor_identity,
            successor_identity=successor_identity,
            revision=revision,
        )

    def _renewal_state_locked(
        self, connection: sqlite3.Connection, agent_id: str
    ) -> TeamRenewalState | None:
        row = connection.execute(
            "SELECT agent_id, team_id, state, ancestor_thread_id, "
            "successor_agent_id, successor_thread_id, team_slot, "
            "predecessor_identity, successor_identity, revision "
            "FROM renewals WHERE agent_id = ?",
            (agent_id,),
        ).fetchone()
        return renewal_state_from_row(row) if row is not None else None


def _renewal_identity_json(identity: Mapping[str, Any]) -> str:
    return json.dumps(dict(identity), sort_keys=True, separators=(",", ":"))


def _renewal_identity_from_json(raw: str) -> dict[str, Any]:
    if not raw:
        return {}
    loaded = json.loads(raw)
    return dict(loaded) if isinstance(loaded, dict) else {}


def renewal_state_from_row(row: sqlite3.Row) -> TeamRenewalState:
    return TeamRenewalState(
        agent_id=str(row["agent_id"]),
        team_id=str(row["team_id"]),
        state=str(row["state"]),
        ancestor_thread_id=str(row["ancestor_thread_id"]),
        successor_agent_id=str(row["successor_agent_id"]),
        successor_thread_id=str(row["successor_thread_id"]),
        team_slot=(
            int(row["team_slot"])
            if row["team_slot"] is not None and str(row["team_slot"]) != ""
            else None
        ),
        predecessor_identity=_renewal_identity_from_json(
            str(row["predecessor_identity"])
        ),
        successor_identity=_renewal_identity_from_json(str(row["successor_identity"])),
        revision=int(row["revision"]),
    )
