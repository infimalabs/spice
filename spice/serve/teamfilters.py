"""Task-filter storage helpers for serve teams."""

from __future__ import annotations

import json
import sqlite3
import time
from typing import Any, Iterable

from spice.errors import SpiceError
from spice.serve.teammodels import TeamConfig, TeamTaskFilter
from spice.serve.teamschema import (
    TASK_FILTER_SOURCE_MANUAL,
    TASK_FILTER_SOURCES,
)


class TeamFilterStoreMixin:
    def _migrate_task_filter_sources_locked(
        self, connection: sqlite3.Connection
    ) -> None:
        rows = connection.execute(
            "SELECT team_id, task_filters FROM teams ORDER BY created_at"
        ).fetchall()
        now = time.time()
        for row in rows:
            team_id = str(row["team_id"])
            existing = connection.execute(
                "SELECT COUNT(*) AS count FROM team_task_filters WHERE team_id = ?",
                (team_id,),
            ).fetchone()
            if existing and int(existing["count"] or 0) > 0:
                continue
            for project in task_filter_projects_from_json(row["task_filters"]):
                connection.execute(
                    "INSERT OR IGNORE INTO team_task_filters "
                    "(team_id, project, source, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, ?)",
                    (team_id, project, TASK_FILTER_SOURCE_MANUAL, now, now),
                )
            self._sync_task_filter_projection_locked(connection, team_id)

    def _task_filter_entries_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[TeamTaskFilter, ...]:
        rows = connection.execute(
            "SELECT project, source FROM team_task_filters "
            "WHERE team_id = ? ORDER BY project, source",
            (team_id,),
        ).fetchall()
        return tuple(
            TeamTaskFilter(project=str(row["project"]), source=str(row["source"]))
            for row in rows
        )

    def _task_filter_projects_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[str, ...]:
        rows = connection.execute(
            "SELECT DISTINCT project FROM team_task_filters "
            "WHERE team_id = ? ORDER BY project",
            (team_id,),
        ).fetchall()
        return tuple(str(row["project"]) for row in rows)

    def _sync_task_filter_projection_locked(
        self, connection: sqlite3.Connection, team_id: str
    ) -> tuple[str, ...]:
        projects = self._task_filter_projects_locked(connection, team_id)
        connection.execute(
            "UPDATE teams SET task_filters = ? WHERE team_id = ?",
            (json.dumps(list(projects)), team_id),
        )
        return projects

    def _replace_task_filters_locked(
        self, connection: sqlite3.Connection, team_id: str, projects: Iterable[str]
    ) -> tuple[str, ...]:
        validated = validated_task_filter_projects(projects)
        if validated:
            placeholders = ",".join("?" for _ in validated)
            connection.execute(
                "DELETE FROM team_task_filters "
                f"WHERE team_id = ? AND project NOT IN ({placeholders})",
                (team_id, *validated),
            )
        else:
            connection.execute(
                "DELETE FROM team_task_filters WHERE team_id = ?", (team_id,)
            )
        now = time.time()
        for project in validated:
            existing = connection.execute(
                "SELECT 1 FROM team_task_filters "
                "WHERE team_id = ? AND project = ? LIMIT 1",
                (team_id, project),
            ).fetchone()
            if existing:
                continue
            connection.execute(
                "INSERT INTO team_task_filters "
                "(team_id, project, source, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (team_id, project, TASK_FILTER_SOURCE_MANUAL, now, now),
            )
        return self._sync_task_filter_projection_locked(connection, team_id)

    def update_team_config(
        self,
        team_id: str,
        config: TeamConfig,
        *,
        replace_task_filters: bool = False,
    ) -> int:
        with self.connect() as connection:
            self._require_team(connection, team_id)
            connection.execute(
                "UPDATE teams SET lifetime = ?, speech_mode = ?, selected_view = ?, "
                "shell_settings = ?, "
                "config_revision = config_revision + 1 WHERE team_id = ?",
                (
                    config.lifetime,
                    config.speech_mode,
                    config.selected_view,
                    json.dumps(config.shell_settings),
                    team_id,
                ),
            )
            if replace_task_filters:
                self._replace_task_filters_locked(
                    connection, team_id, config.task_filters
                )
            task_filters = self._task_filter_projects_locked(connection, team_id)
            return self._record_event(
                connection,
                "updateTeamConfig",
                team_id,
                {"lifetime": config.lifetime, "taskFilters": list(task_filters)},
            )

    def add_task_filter(
        self,
        team_id: str,
        project: str,
        *,
        source: str = TASK_FILTER_SOURCE_MANUAL,
    ) -> int:
        project = validated_task_filter_project(project)
        source = validated_task_filter_source(source)
        with self.connect() as connection:
            self._require_team(connection, team_id)
            now = time.time()
            cursor = connection.execute(
                "INSERT OR IGNORE INTO team_task_filters "
                "(team_id, project, source, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (team_id, project, source, now, now),
            )
            if cursor.rowcount == 0:
                return self._current_revision_locked(connection)
            self._sync_task_filter_projection_locked(connection, team_id)
            connection.execute(
                "UPDATE teams SET config_revision = config_revision + 1 "
                "WHERE team_id = ?",
                (team_id,),
            )
            return self._record_event(
                connection,
                "addTaskFilter",
                team_id,
                {
                    "project": project,
                    "source": source,
                    "taskFilters": list(
                        self._task_filter_projects_locked(connection, team_id)
                    ),
                },
            )

    def remove_task_filter(
        self,
        team_id: str,
        project: str,
        *,
        source: str | None = None,
    ) -> int:
        project = validated_task_filter_project(project)
        if source is not None:
            source = validated_task_filter_source(source)
        with self.connect() as connection:
            self._require_team(connection, team_id)
            if source is None:
                cursor = connection.execute(
                    "DELETE FROM team_task_filters WHERE team_id = ? AND project = ?",
                    (team_id, project),
                )
            else:
                cursor = connection.execute(
                    "DELETE FROM team_task_filters "
                    "WHERE team_id = ? AND project = ? AND source = ?",
                    (team_id, project, source),
                )
            if cursor.rowcount == 0:
                return self._current_revision_locked(connection)
            self._sync_task_filter_projection_locked(connection, team_id)
            connection.execute(
                "UPDATE teams SET config_revision = config_revision + 1 "
                "WHERE team_id = ?",
                (team_id,),
            )
            return self._record_event(
                connection,
                "removeTaskFilter",
                team_id,
                {
                    "project": project,
                    "source": source or "",
                    "taskFilters": list(
                        self._task_filter_projects_locked(connection, team_id)
                    ),
                },
            )

    def team_config(self, team_id: str) -> TeamConfig:
        with self.connect() as connection:
            row = self._require_team(connection, team_id)
            entries = self._task_filter_entries_locked(connection, team_id)
            return config_from_row(row, entries)

    def open_team_ids_with_task_filter(
        self, project: str, *, source: str | None = None
    ) -> tuple[str, ...]:
        project = validated_task_filter_project(project)
        if source is not None:
            source = validated_task_filter_source(source)
        source_clause = "" if source is None else " AND team_task_filters.source = ?"
        params: tuple[str, ...] = (project,) if source is None else (project, source)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT DISTINCT teams.team_id FROM teams "
                "JOIN team_task_filters ON team_task_filters.team_id = teams.team_id "
                "WHERE teams.status = 'open' AND team_task_filters.project = ?"
                f"{source_clause} "
                "ORDER BY teams.created_at",
                params,
            ).fetchall()
        return tuple(str(row["team_id"]) for row in rows)

    def open_task_filter_projects(
        self, *, source: str | None = None
    ) -> tuple[str, ...]:
        if source is not None:
            source = validated_task_filter_source(source)
        source_clause = "" if source is None else " AND team_task_filters.source = ?"
        params: tuple[str, ...] = () if source is None else (source,)
        with self.connect() as connection:
            rows = connection.execute(
                "SELECT DISTINCT team_task_filters.project FROM team_task_filters "
                "JOIN teams ON teams.team_id = team_task_filters.team_id "
                f"WHERE teams.status = 'open'{source_clause} "
                "ORDER BY team_task_filters.project",
                params,
            ).fetchall()
        return tuple(str(row["project"]) for row in rows)


def config_from_row(
    row: sqlite3.Row, entries: Iterable[TeamTaskFilter] = ()
) -> TeamConfig:
    task_filter_entries = tuple(entries)
    task_filters = tuple(dict.fromkeys(entry.project for entry in task_filter_entries))
    if not task_filters:
        task_filters = task_filter_projects_from_json(row["task_filters"])
    try:
        shell_settings = json.loads(row["shell_settings"])
    except (json.JSONDecodeError, TypeError):
        shell_settings = {}
    return TeamConfig(
        lifetime=str(row["lifetime"]),
        speech_mode=str(row["speech_mode"]),
        task_filters=task_filters,
        task_filter_entries=task_filter_entries,
        selected_view=str(row["selected_view"]),
        shell_settings=shell_settings if isinstance(shell_settings, dict) else {},
    )


def task_filter_projects_from_json(raw: object) -> tuple[str, ...]:
    try:
        values = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return ()
    if not isinstance(values, list):
        return ()
    return validated_task_filter_projects(str(item) for item in values)


def shell_settings_from_json(raw: object) -> dict[str, Any]:
    try:
        values = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return {}
    return values if isinstance(values, dict) else {}


def validated_task_filter_projects(projects: Iterable[str]) -> tuple[str, ...]:
    seen: dict[str, None] = {}
    for project in projects:
        value = str(project or "").strip()
        if not value:
            continue
        seen.setdefault(validated_task_filter_project(value), None)
    return tuple(sorted(seen))


def validated_task_filter_project(project: str) -> str:
    from spice.tasks import config as task_config

    return task_config.validate_assignable_project(str(project or "").strip())


def validated_task_filter_source(source: str) -> str:
    value = str(source or "").strip()
    if value not in TASK_FILTER_SOURCES:
        raise SpiceError(
            "task filter source must be one of "
            + ", ".join(sorted(TASK_FILTER_SOURCES))
        )
    return value
