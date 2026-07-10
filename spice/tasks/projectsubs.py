"""Team-store project subscription upkeep driven by task mutations."""

from __future__ import annotations

from typing import Any

from spice.errors import SpiceError
from spice.tasks import config, tw


def _subscribe_claim_project(row: dict[str, Any], actor: str) -> None:
    from spice.serve.team.store import TASK_FILTER_SOURCE_AUTO_CLAIM

    # Steer never auto-subscribes: a manual claim, steal, or ownership repair
    # in Steer must not widen the team's filter set.
    _subscribe_auto_project(
        str(row.get("project") or ""),
        actor,
        allowed_lifetimes=("Drive", "Drain"),
        source=TASK_FILTER_SOURCE_AUTO_CLAIM,
    )


def _subscribe_created_project(project: str, actor: str) -> str:
    return _subscribe_auto_project(project, actor, allowed_lifetimes=("Drive",))


def _subscribe_woken_project(project: str, actor: str) -> str:
    return _subscribe_auto_project(
        project,
        actor,
        allowed_lifetimes=("Drive", "Drain"),
    )


def _subscribe_auto_project(
    project: str,
    actor: str,
    *,
    allowed_lifetimes: tuple[str, ...],
    source: str | None = None,
) -> str:
    project = str(project or "").strip()
    if not project or _project_is_subscription_excluded(project):
        return f"route_filter=skipped:{project or '-'}:excluded"

    from spice.serve.team.store import ServeTeamStore, TASK_FILTER_SOURCE_AUTO_CREATE
    from spice.tasks import lanes

    if source is None:
        source = TASK_FILTER_SOURCE_AUTO_CREATE
    store = ServeTeamStore()
    team_id = store.current_team_for_agent(lanes.route_actor_id(actor))
    if team_id is None:
        return f"route_filter=skipped:{project}:no_team"
    team_config = store.team_config(team_id)
    if team_config.lifetime not in allowed_lifetimes:
        return f"route_filter=skipped:{project}:lifetime:{team_config.lifetime}"
    before = {
        (entry.project, entry.source) for entry in team_config.task_filter_entries
    }
    store.add_task_filter(team_id, project, source=source)
    outcome = "present" if (project, source) in before else "added"
    return f"route_filter={outcome}:{project}:{source}"


def _project_is_subscription_excluded(project: str) -> bool:
    return _project_is_internal(project) or config.is_hidden_project(project)


def _gc_empty_project_task_filters(project: str) -> None:
    project = str(project or "").strip()
    if not project or _project_is_internal(project):
        return
    try:
        project = config.validate_assignable_project(project)
    except SpiceError:
        return

    from spice.serve.team.store import (
        TASK_FILTER_SOURCE_AUTO_CLAIM,
        TASK_FILTER_SOURCE_AUTO_CREATE,
        ServeTeamStore,
    )

    store = ServeTeamStore()
    # Provenance is modeled now: empty-project GC reclaims ephemeral
    # subscriptions without deleting manually curated Steer filters.
    for source in (TASK_FILTER_SOURCE_AUTO_CREATE, TASK_FILTER_SOURCE_AUTO_CLAIM):
        for filter_project in store.open_task_filter_projects(source=source):
            if not _project_filter_covers_project(filter_project, project):
                continue
            # Waiting (deferred) tasks keep the subscription alive: they wake
            # back into the project, so it is not empty yet.
            if tw.export(
                [
                    "(",
                    "status:pending",
                    "or",
                    "status:waiting",
                    ")",
                    f"project:{filter_project}",
                ]
            ):
                continue
            for team_id in store.open_team_ids_with_task_filter(
                filter_project, source=source
            ):
                store.remove_task_filter(team_id, filter_project, source=source)


def _project_is_internal(project: str) -> bool:
    return config.is_internal_or_hidden_project(project)


def _project_filter_covers_project(filter_project: str, project: str) -> bool:
    return project == filter_project or project.startswith(
        filter_project + config.PROJECT_DELIMITER
    )
