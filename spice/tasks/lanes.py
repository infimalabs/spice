"""Team-derived task routing helpers.

Task routing is owned by the serve team control plane. This module exposes
the small Taskwarrior-facing projection used by status, next-task selection,
and diagnostics; it does not persist a separate route registry.
"""

from __future__ import annotations

from dataclasses import dataclass
import re
from typing import Any

from spice.errors import SpiceError
from spice.tasks import config

FILTER_TERM_RE = re.compile(r"^[^\s()]+$")
TAG_TERM_RE = re.compile(r"^\+[A-Za-z0-9_][A-Za-z0-9_]*$")
ROUTE_LIFETIMES = frozenset({"Steer", "Drive", "Drain"})
RouteEntryValue = list[str] | str
RouteEntry = dict[str, RouteEntryValue]


@dataclass(frozen=True)
class TaskContinuationContract:
    lifetime: str
    drain_after_phase_boundary: bool


def _validate_rc(tokens: list[str]) -> list[str]:
    out: list[str] = []
    for token in tokens:
        value = token.strip()
        if not value:
            continue
        if not value.startswith("rc."):
            raise SpiceError(f"route rc override must be a literal rc.* arg: {value}")
        out.append(value)
    return out


def _filter_term(raw: Any) -> str:
    value = str(raw or "").strip()
    if not value:
        return ""
    if value.lower() == "or" or not FILTER_TERM_RE.match(value):
        raise SpiceError(
            f"route filter term must be a simple Taskwarrior filter object: {value!r}"
        )
    if value.startswith("project:"):
        project = value.split(":", 1)[1]
        return f"project:{config.validate_project(project)}"
    if value.startswith("phase:"):
        phase = value.split(":", 1)[1]
        if phase not in config.APPROVED_PHASES:
            raise SpiceError(f"route phase filter term has unknown phase: {value!r}")
        return value
    if TAG_TERM_RE.match(value):
        return value
    raise SpiceError(
        f"route filter term must be project:<name>, phase:<phase>, or +tag: {value!r}"
    )


def _clean_filter(raw: Any) -> list[str]:
    if not isinstance(raw, list):
        return []
    terms = {_filter_term(item) for item in raw}
    terms.discard("")
    return sorted(terms)


def _lifetime(raw: Any) -> str:
    value = str(raw or "").strip()
    if not value:
        return ""
    if value not in ROUTE_LIFETIMES:
        raise SpiceError(f"route lifetime must be Steer, Drive, or Drain: {value!r}")
    return value


def _route_list(entry: RouteEntry, key: str) -> list[str]:
    value = entry.get(key)
    return value if isinstance(value, list) else []


def route_actor_id(actor: str) -> str:
    from spice.serve.team.ids import normalize_actor_id

    return normalize_actor_id(actor)


def team_route_for_actor(actor: str) -> RouteEntry | None:
    actor = str(actor or "").strip()
    if not actor:
        return None
    lookup_id = route_actor_id(actor)
    from spice.serve.team.store import ServeTeamStore

    for team in ServeTeamStore().team_snapshot().teams:
        member_ids = [member.agent_id for member in team.members]
        if lookup_id not in member_ids:
            continue
        filter_terms, manual_terms = _route_filters_from_team(team)
        entry: RouteEntry = {
            "agents": member_ids,
            "filter": filter_terms,
            "manual": manual_terms,
            "lifetime": team.config.lifetime,
        }
        rc_tokens = _team_rc_overrides(team.config.shell_settings)
        if rc_tokens:
            entry["rc"] = rc_tokens
        return entry
    return None


def _route_filters_from_team(team: Any) -> tuple[list[str], list[str]]:
    """Build the (filter, manual) term lists for a team's stored filters.

    Shared by the actor route lookup and the team-driven effective-term helper
    so the payload the UI renders is constructed identically to the route the
    allocator selects on -- one encoding, no drift.
    """
    from spice.serve.team.schema import TASK_FILTER_SOURCE_MANUAL

    filter_terms = [
        f"project:{config.validate_assignable_project(task_filter)}"
        for task_filter in team.config.task_filters
    ]
    manual_terms = [
        f"project:{config.validate_assignable_project(entry.project)}"
        for entry in team.config.task_filter_entries
        if entry.source == TASK_FILTER_SOURCE_MANUAL
    ]
    return _clean_filter(filter_terms), _clean_filter(manual_terms)


def _team_rc_overrides(shell_settings: dict[str, Any]) -> list[str]:
    raw = shell_settings.get("taskRc", shell_settings.get("rc", []))
    if isinstance(raw, str):
        return _validate_rc([raw])
    if isinstance(raw, list):
        return _validate_rc([str(token) for token in raw])
    return []


def effective_filter_terms(route: RouteEntry | None) -> list[str]:
    """Interpret stored filters through the lifetime lens.

    Filter rows are durable state; the lifetime only changes how they are
    read, so flipping the slider never mutates the store. Drain dissolves the
    boundary (all assignable stems), Steer narrows to the operator's manual
    pins, and Drive uses everything (pins + auto subscriptions).
    """
    if not route:
        return []
    lifetime = _lifetime(route.get("lifetime")) if route.get("lifetime") else ""
    if lifetime == "Drain":
        return _clean_filter([f"project:{stem}" for stem in config.assignable_stems()])
    if lifetime == "Steer":
        return _clean_filter(_route_list(route, "manual"))
    return _clean_filter(_route_list(route, "filter"))


def effective_filter_terms_for_team(team: Any) -> list[str]:
    """Lifetime-lensed effective filter terms for an already-loaded team.

    Equivalent to ``effective_filter_terms(team_route_for_actor(actor))`` but
    driven by a team state the caller already holds, so display/poll paths that
    render the filter set avoid a second team snapshot.
    """
    filter_terms, manual_terms = _route_filters_from_team(team)
    route: RouteEntry = {
        "filter": filter_terms,
        "manual": manual_terms,
        "lifetime": team.config.lifetime,
    }
    return effective_filter_terms(route)


def effective_filter_projects_for_team(team: Any) -> list[str]:
    """Bare assignable project names for a team's effective filter set.

    The presentation form the UI renders: the lifetime-lensed predicate the
    allocator selects on, stripped of the ``project:`` prefix to match the
    stored ``taskFilters`` naming. In Drain this is every assignable stem.
    """
    return [
        term.split(":", 1)[1]
        for term in effective_filter_terms_for_team(team)
        if term.startswith("project:")
    ]


def filter_args(route: RouteEntry | None) -> list[str] | None:
    if not route:
        return None
    return filter_terms_args(effective_filter_terms(route))


def filter_terms_args(filters: list[str]) -> list[str]:
    values = _clean_filter(filters)
    if not values:
        return []
    if len(values) == 1:
        return [values[0]]
    args = ["("]
    for index, value in enumerate(values):
        if index:
            args.append("or")
        args.append(value)
    args.append(")")
    return args


def rc_overrides(route: dict[str, Any] | None) -> list[str]:
    if not route:
        return []
    return [str(token) for token in (route.get("rc") or []) if str(token)]


def task_continuation_contract(
    route: RouteEntry | None,
) -> TaskContinuationContract:
    lifetime = (
        _lifetime(route.get("lifetime")) if route and route.get("lifetime") else ""
    )
    return TaskContinuationContract(
        lifetime=lifetime,
        drain_after_phase_boundary=lifetime != "Steer",
    )
