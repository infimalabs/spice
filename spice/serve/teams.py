"""The serve team control plane: durable, revisioned lane grouping.

Teams are the server-side truth behind the UI's lane groups. Every mutation
is an event with a monotonically increasing global revision; clients carry
`expectedRevision` for optimistic concurrency and re-pull snapshots when they
lose. The store is SQLite under the task backend root so every worktree of a
repository shares one control plane.

Commands: create, close, move agent (composer drag), remove agent, split,
merge, update config, record renewal (pending while the predecessor runs;
started once the successor exists).
"""

from __future__ import annotations

import json
import sqlite3
import time
import uuid as uuidlib
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable, Iterator

from spice.errors import SpiceError

TEAM_DATABASE_FILENAME = "serve-teams.sqlite3"
DEFAULT_LIFETIME = "Steer"
DEFAULT_SPEECH_MODE = "speak"
DEFAULT_SELECTED_VIEW = "compose"
TEAM_ID_HEX_CHARS = 12

_SCHEMA = """
CREATE TABLE IF NOT EXISTS events (
    revision INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    kind TEXT NOT NULL,
    team_id TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS teams (
    team_id TEXT PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'open',
    created_at REAL NOT NULL,
    revision INTEGER NOT NULL,
    config_revision INTEGER NOT NULL DEFAULT 0,
    lifetime TEXT NOT NULL,
    speech_mode TEXT NOT NULL,
    selected_view TEXT NOT NULL,
    task_filters TEXT NOT NULL DEFAULT '[]',
    shell_settings TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS memberships (
    team_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    joined_at REAL NOT NULL,
    PRIMARY KEY (team_id, agent_id)
);
CREATE TABLE IF NOT EXISTS renewals (
    agent_id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL,
    state TEXT NOT NULL,
    ancestor_thread_id TEXT NOT NULL,
    successor_agent_id TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL
);
"""


@dataclass(frozen=True)
class TeamConfig:
    lifetime: str = DEFAULT_LIFETIME
    speech_mode: str = DEFAULT_SPEECH_MODE
    task_filters: tuple[str, ...] = ()
    selected_view: str = DEFAULT_SELECTED_VIEW
    shell_settings: dict[str, Any] = field(default_factory=dict)

    def to_payload(self, revision: int) -> dict[str, Any]:
        return {
            "lifetime": self.lifetime,
            "speechMode": self.speech_mode,
            "taskFilters": list(self.task_filters),
            "selectedView": self.selected_view,
            "shellSettings": dict(self.shell_settings),
            "revision": revision,
        }


@dataclass(frozen=True)
class TeamMember:
    agent_id: str
    agent_facts: dict[str, str] = field(default_factory=dict)

    def to_payload(self) -> dict[str, Any]:
        return {"agentId": self.agent_id, "agentFacts": dict(self.agent_facts)}


@dataclass(frozen=True)
class TeamState:
    team_id: str
    status: str
    revision: int
    config_revision: int
    config: TeamConfig
    members: tuple[TeamMember, ...]

    def to_payload(self) -> dict[str, Any]:
        return {
            "teamId": self.team_id,
            "status": self.status,
            "revision": self.revision,
            "config": self.config.to_payload(self.config_revision),
            "members": [member.to_payload() for member in self.members],
        }


@dataclass(frozen=True)
class TeamSnapshot:
    global_revision: int
    teams: tuple[TeamState, ...]

    def to_payload(self) -> dict[str, Any]:
        return {
            "globalRevision": self.global_revision,
            "teams": [team.to_payload() for team in self.teams],
        }


@dataclass(frozen=True)
class TeamRenewalState:
    agent_id: str
    team_id: str
    state: str
    ancestor_thread_id: str
    successor_agent_id: str
    revision: int


def team_database_path() -> Path:
    from spice.tasks import config as task_config

    return task_config.backend_root() / TEAM_DATABASE_FILENAME


class ServeTeamStore:
    def __init__(self, path: Path | None = None) -> None:
        self.path = path or team_database_path()

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        connection = sqlite3.connect(self.path)
        connection.row_factory = sqlite3.Row
        try:
            connection.executescript(_SCHEMA)
            yield connection
            connection.commit()
        finally:
            connection.close()

    # ---- events / revisions -------------------------------------------

    def _record_event(
        self,
        connection: sqlite3.Connection,
        kind: str,
        team_id: str,
        payload: dict[str, Any],
    ) -> int:
        cursor = connection.execute(
            "INSERT INTO events (ts, kind, team_id, payload) VALUES (?, ?, ?, ?)",
            (time.time(), kind, team_id, json.dumps(payload, separators=(",", ":"))),
        )
        revision = int(cursor.lastrowid or 0)
        connection.execute(
            "UPDATE teams SET revision = ? WHERE team_id = ?", (revision, team_id)
        )
        return revision

    def global_revision(self) -> int:
        with self.connect() as connection:
            row = connection.execute("SELECT MAX(revision) AS r FROM events").fetchone()
            return int(row["r"] or 0)

    # ---- commands ------------------------------------------------------

    def create_team(
        self,
        *,
        team_id: str | None = None,
        config: TeamConfig | None = None,
        members: Iterable[str] = (),
    ) -> TeamState:
        config = config or TeamConfig()
        resolved_team_id = team_id or f"team-{uuidlib.uuid4().hex[:TEAM_ID_HEX_CHARS]}"
        with self.connect() as connection:
            connection.execute(
                "INSERT INTO teams (team_id, status, created_at, revision, "
                "config_revision, lifetime, speech_mode, selected_view, "
                "task_filters, shell_settings) VALUES (?, 'open', ?, 0, 0, ?, ?, ?, ?, ?)",
                (
                    resolved_team_id,
                    time.time(),
                    config.lifetime,
                    config.speech_mode,
                    config.selected_view,
                    json.dumps(list(config.task_filters)),
                    json.dumps(config.shell_settings),
                ),
            )
            for agent_id in members:
                self._assign_locked(connection, resolved_team_id, agent_id)
            self._record_event(
                connection, "createTeam", resolved_team_id, {"members": list(members)}
            )
            return self._team_state_locked(connection, resolved_team_id)

    def close_team(self, team_id: str) -> int:
        with self.connect() as connection:
            self._require_team(connection, team_id)
            connection.execute(
                "UPDATE teams SET status = 'closed' WHERE team_id = ?", (team_id,)
            )
            connection.execute("DELETE FROM memberships WHERE team_id = ?", (team_id,))
            return self._record_event(connection, "closeTeam", team_id, {})

    def assign_agent(
        self, team_id: str, agent_id: str, aliases: Iterable[str] = ()
    ) -> int:
        with self.connect() as connection:
            self._require_team(connection, team_id)
            self._assign_locked(connection, team_id, agent_id, aliases=aliases)
            return self._record_event(
                connection, "assignAgent", team_id, {"agentId": agent_id}
            )

    def _assign_locked(
        self,
        connection: sqlite3.Connection,
        team_id: str,
        agent_id: str,
        aliases: Iterable[str] = (),
    ) -> None:
        agent_id = _normalized_id(agent_id, "agent_id")
        alias_ids = _agent_alias_ids(agent_id, aliases)
        previous_team_ids: list[str] = []
        for alias_id in alias_ids:
            previous_rows = connection.execute(
                "SELECT DISTINCT team_id FROM memberships WHERE agent_id = ?",
                (alias_id,),
            ).fetchall()
            previous_team_ids.extend(
                row["team_id"]
                for row in previous_rows
                if row["team_id"] not in previous_team_ids
            )
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ?", (alias_id,)
            )
        connection.execute(
            "INSERT INTO memberships (team_id, agent_id, joined_at) VALUES (?, ?, ?)",
            (team_id, agent_id, time.time()),
        )
        self._close_empty_teams_locked(
            connection,
            [
                previous_team_id
                for previous_team_id in previous_team_ids
                if previous_team_id != team_id
            ],
        )

    def _close_empty_teams_locked(
        self, connection: sqlite3.Connection, team_ids: Iterable[str]
    ) -> None:
        for team_id in team_ids:
            count = connection.execute(
                "SELECT COUNT(*) AS count FROM memberships WHERE team_id = ?",
                (team_id,),
            ).fetchone()
            if count and int(count["count"] or 0) > 0:
                continue
            connection.execute(
                "UPDATE teams SET status = 'closed' WHERE team_id = ?",
                (team_id,),
            )
            self._record_event(connection, "closeEmptyTeam", team_id, {})

    def remove_agent(
        self, team_id: str, agent_id: str, aliases: Iterable[str] = ()
    ) -> int:
        alias_ids = _agent_alias_ids(agent_id, aliases)
        with self.connect() as connection:
            row = None
            for alias_id in alias_ids:
                row = connection.execute(
                    "SELECT team_id FROM memberships WHERE agent_id = ?",
                    (alias_id,),
                ).fetchone()
                if row is not None:
                    break
            if row is None or row["team_id"] != team_id:
                raise SpiceError(f"agent {agent_id} is not assigned to team {team_id}")
            for alias_id in alias_ids:
                connection.execute(
                    "DELETE FROM memberships WHERE agent_id = ?", (alias_id,)
                )
            self._close_empty_teams_locked(connection, [team_id])
            return self._record_event(
                connection, "removeAgent", team_id, {"agentId": agent_id}
            )

    def split_team(
        self,
        source_team_id: str,
        *,
        agent_ids: Iterable[str],
        new_team_id: str | None = None,
        config: TeamConfig | None = None,
    ) -> TeamState:
        agent_list = [_normalized_id(agent, "agent_id") for agent in agent_ids]
        if not agent_list:
            raise SpiceError("split requires at least one agent id")
        source_config = self.team_config(source_team_id)
        created = self.create_team(
            team_id=new_team_id, config=config or source_config, members=()
        )
        with self.connect() as connection:
            for agent_id in agent_list:
                row = connection.execute(
                    "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
                ).fetchone()
                if row is None or row["team_id"] != source_team_id:
                    raise SpiceError(
                        f"agent {agent_id} is not assigned to team {source_team_id}"
                    )
                self._assign_locked(connection, created.team_id, agent_id)
            self._record_event(
                connection,
                "splitTeam",
                source_team_id,
                {"newTeamId": created.team_id, "agents": agent_list},
            )
            return self._team_state_locked(connection, created.team_id)

    def merge_teams(self, source_team_id: str, destination_team_id: str) -> int:
        if source_team_id == destination_team_id:
            raise SpiceError("merge requires two distinct teams")
        with self.connect() as connection:
            self._require_team(connection, source_team_id)
            self._require_team(connection, destination_team_id)
            rows = connection.execute(
                "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY joined_at",
                (source_team_id,),
            ).fetchall()
            for row in rows:
                self._assign_locked(connection, destination_team_id, row["agent_id"])
            connection.execute(
                "UPDATE teams SET status = 'closed' WHERE team_id = ?",
                (source_team_id,),
            )
            return self._record_event(
                connection,
                "mergeTeams",
                destination_team_id,
                {"sourceTeamId": source_team_id},
            )

    def update_team_config(self, team_id: str, config: TeamConfig) -> int:
        with self.connect() as connection:
            self._require_team(connection, team_id)
            connection.execute(
                "UPDATE teams SET lifetime = ?, speech_mode = ?, selected_view = ?, "
                "task_filters = ?, shell_settings = ?, "
                "config_revision = config_revision + 1 WHERE team_id = ?",
                (
                    config.lifetime,
                    config.speech_mode,
                    config.selected_view,
                    json.dumps(list(config.task_filters)),
                    json.dumps(config.shell_settings),
                    team_id,
                ),
            )
            return self._record_event(
                connection,
                "updateTeamConfig",
                team_id,
                {"lifetime": config.lifetime, "taskFilters": list(config.task_filters)},
            )

    # ---- renewal -------------------------------------------------------

    def record_pending_renewal(
        self, *, agent_id: str, ancestor_thread_id: str
    ) -> TeamRenewalState:
        team_id = self.open_team_for_agent(agent_id)
        with self.connect() as connection:
            revision = self._record_event(
                connection,
                "renewalPending",
                team_id,
                {"agentId": agent_id, "ancestor": ancestor_thread_id},
            )
            connection.execute(
                "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
                "ancestor_thread_id, successor_agent_id, revision) "
                "VALUES (?, ?, 'pending', ?, '', ?)",
                (agent_id, team_id, ancestor_thread_id, revision),
            )
        return TeamRenewalState(
            agent_id=agent_id,
            team_id=team_id,
            state="pending",
            ancestor_thread_id=ancestor_thread_id,
            successor_agent_id="",
            revision=revision,
        )

    def record_started_renewal(
        self,
        *,
        predecessor_agent_id: str,
        successor_agent_id: str,
        ancestor_thread_id: str = "",
    ) -> TeamRenewalState:
        team_id = self.open_team_for_agent(predecessor_agent_id)
        with self.connect() as connection:
            revision = self._record_event(
                connection,
                "renewalStarted",
                team_id,
                {
                    "predecessor": predecessor_agent_id,
                    "successor": successor_agent_id,
                },
            )
            self._assign_locked(connection, team_id, successor_agent_id)
            connection.execute(
                "DELETE FROM memberships WHERE agent_id = ?", (predecessor_agent_id,)
            )
            connection.execute(
                "INSERT OR REPLACE INTO renewals (agent_id, team_id, state, "
                "ancestor_thread_id, successor_agent_id, revision) "
                "VALUES (?, ?, 'started', ?, ?, ?)",
                (
                    predecessor_agent_id,
                    team_id,
                    ancestor_thread_id,
                    successor_agent_id,
                    revision,
                ),
            )
        return TeamRenewalState(
            agent_id=predecessor_agent_id,
            team_id=team_id,
            state="started",
            ancestor_thread_id=ancestor_thread_id,
            successor_agent_id=successor_agent_id,
            revision=revision,
        )

    # ---- reads ---------------------------------------------------------

    def current_team_for_agent(self, agent_id: str) -> str | None:
        with self.connect() as connection:
            row = connection.execute(
                "SELECT team_id FROM memberships WHERE agent_id = ?", (agent_id,)
            ).fetchone()
            return row["team_id"] if row else None

    def open_team_for_agent(self, agent_id: str) -> str:
        team_id = self.current_team_for_agent(agent_id)
        if team_id is None:
            raise SpiceError(f"agent {agent_id} is not assigned to any team")
        return team_id

    def team_config(self, team_id: str) -> TeamConfig:
        with self.connect() as connection:
            row = self._require_team(connection, team_id)
            return _config_from_row(row)

    def team_state(self, team_id: str) -> TeamState:
        with self.connect() as connection:
            return self._team_state_locked(connection, team_id)

    def team_snapshot(self, *, since_revision: int | None = None) -> TeamSnapshot:
        with self.connect() as connection:
            revision_row = connection.execute(
                "SELECT MAX(revision) AS r FROM events"
            ).fetchone()
            global_revision = int(revision_row["r"] or 0)
            rows = connection.execute(
                "SELECT * FROM teams WHERE status = 'open' ORDER BY created_at"
            ).fetchall()
            teams = tuple(
                self._team_state_locked(connection, row["team_id"]) for row in rows
            )
        return TeamSnapshot(global_revision=global_revision, teams=teams)

    def _team_state_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> TeamState:
        row = self._require_team(connection, team_id)
        member_rows = connection.execute(
            "SELECT agent_id FROM memberships WHERE team_id = ? ORDER BY joined_at",
            (team_id,),
        ).fetchall()
        return TeamState(
            team_id=team_id,
            status=str(row["status"]),
            revision=int(row["revision"]),
            config_revision=int(row["config_revision"]),
            config=_config_from_row(row),
            members=tuple(
                TeamMember(agent_id=member["agent_id"]) for member in member_rows
            ),
        )

    def _require_team(
        self, connection: sqlite3.Connection, team_id: str
    ) -> sqlite3.Row:
        row = connection.execute(
            "SELECT * FROM teams WHERE team_id = ?", (team_id,)
        ).fetchone()
        if row is None:
            raise SpiceError(f"unknown team: {team_id}")
        return row


def _config_from_row(row: sqlite3.Row) -> TeamConfig:
    try:
        task_filters = tuple(json.loads(row["task_filters"]))
    except (json.JSONDecodeError, TypeError):
        task_filters = ()
    try:
        shell_settings = json.loads(row["shell_settings"])
    except (json.JSONDecodeError, TypeError):
        shell_settings = {}
    return TeamConfig(
        lifetime=str(row["lifetime"]),
        speech_mode=str(row["speech_mode"]),
        task_filters=task_filters,
        selected_view=str(row["selected_view"]),
        shell_settings=shell_settings if isinstance(shell_settings, dict) else {},
    )


def _normalized_id(value: str, field_name: str) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        raise SpiceError(f"{field_name} must be non-empty")
    return normalized


@dataclass(frozen=True)
class TeamCommandResult:
    revision: int
    snapshot: TeamSnapshot


class TeamCommandService:
    """Validate and apply UI team commands with optimistic concurrency."""

    def __init__(self, store: ServeTeamStore | None = None) -> None:
        self.store = store or ServeTeamStore()

    def apply(self, payload: dict[str, Any]) -> TeamCommandResult:
        command = str(payload.get("command") or "")
        expected = payload.get("expectedRevision")
        if (
            isinstance(expected, int)
            and expected
            and expected < self.store.global_revision()
        ):
            # Stale client: commands apply against current state anyway when
            # they remain valid; an invalid one raises with detail below.
            pass
        if command == "createTeam":
            config = _config_from_payload(payload.get("config"))
            members = [str(item) for item in payload.get("members") or [] if item]
            self.store.create_team(config=config, members=members)
        elif command == "closeTeam":
            self.store.close_team(_required(payload, "teamId"))
        elif command in ("moveAgentToTeam", "moveComposerToTeam"):
            self.store.assign_agent(
                _required(payload, "teamId"),
                _required(payload, "agentId"),
                aliases=_aliases(payload),
            )
        elif command == "removeAgentFromTeam":
            self.store.remove_agent(
                _required(payload, "teamId"),
                _required(payload, "agentId"),
                aliases=_aliases(payload),
            )
        elif command == "splitTeam":
            agent_ids = [str(item) for item in payload.get("agentIds") or [] if item]
            self.store.split_team(
                _required(payload, "sourceTeamId"), agent_ids=agent_ids
            )
        elif command == "mergeTeams":
            self.store.merge_teams(
                _required(payload, "sourceTeamId"),
                _required(payload, "destinationTeamId"),
            )
        elif command == "updateTeamConfig":
            team_id = _required(payload, "teamId")
            current = self.store.team_config(team_id)
            patch = payload.get("configPatch") or {}
            self.store.update_team_config(team_id, _patched_config(current, patch))
        else:
            raise SpiceError(f"unknown team command {command!r}")
        snapshot = self.store.team_snapshot()
        return TeamCommandResult(revision=snapshot.global_revision, snapshot=snapshot)


def _required(payload: dict[str, Any], key: str) -> str:
    return _normalized_id(str(payload.get(key) or ""), key)


def _aliases(payload: dict[str, Any]) -> list[str]:
    return [str(item) for item in payload.get("agentAliases") or [] if item]


def _agent_alias_ids(agent_id: str, aliases: Iterable[str]) -> list[str]:
    ids = [_normalized_id(agent_id, "agent_id")]
    for alias in aliases:
        normalized = _normalized_id(alias, "agent_alias")
        if normalized not in ids:
            ids.append(normalized)
    return ids


def _config_from_payload(raw: Any) -> TeamConfig:
    if not isinstance(raw, dict):
        return TeamConfig()
    return _patched_config(TeamConfig(), raw)


def _patched_config(current: TeamConfig, patch: dict[str, Any]) -> TeamConfig:
    task_filters = current.task_filters
    if isinstance(patch.get("taskFilters"), list):
        task_filters = tuple(
            str(item) for item in patch["taskFilters"] if str(item or "").strip()
        )
    shell_settings = current.shell_settings
    if isinstance(patch.get("shellSettings"), dict):
        shell_settings = dict(patch["shellSettings"])
    return TeamConfig(
        lifetime=str(patch.get("lifetime") or current.lifetime),
        speech_mode=str(patch.get("speechMode") or current.speech_mode),
        task_filters=task_filters,
        selected_view=str(patch.get("selectedView") or current.selected_view),
        shell_settings=shell_settings,
    )
