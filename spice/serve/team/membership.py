"""Membership-interval reconstruction from team event streams.

The team event log is the authority on who served which team when. Replaying
it yields closed membership intervals plus the still-open memberships, which
historical metric reads use to attribute per-agent activity to the team the
agent was on at the time. This module is pure event-stream computation: rows
arrive from the store's locked accessors, and no SQL runs here.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, Iterable, Protocol

from spice.errors import SpiceError


class StoreRow(Protocol):
    """Structural row shape shared by sqlite3 rows and plain mappings."""

    def __getitem__(self, key: str, /) -> Any: ...


@dataclass(frozen=True)
class MembershipInterval:
    team_id: str
    agent_id: str
    start: float
    end: float


def membership_intervals_from_events(
    rows: Iterable[StoreRow], *, end_time: float
) -> list[MembershipInterval]:
    """Replay team events (in revision order) into membership intervals.

    Memberships still open after the replay close at ``end_time``, so every
    interval is a concrete [start, end) span; events after ``end_time`` are
    outside the reconstruction horizon and do not contribute.
    """
    open_memberships: dict[str, tuple[str, float]] = {}
    intervals: list[MembershipInterval] = []
    for row in rows:
        timestamp = float(row["ts"] or 0.0)
        if timestamp > end_time:
            continue
        team_id = str(row["team_id"] or "")
        kind = str(row["kind"] or "")
        payload = event_payload(row)
        if kind == "createTeam":
            for agent_id in _event_agent_ids(payload, "members"):
                _move_membership(
                    open_memberships, intervals, agent_id, team_id, timestamp
                )
        elif kind == "assignAgent":
            for alias_id in _event_optional_agent_ids(payload, "aliases"):
                _close_membership_if_open(
                    open_memberships, intervals, alias_id, timestamp
                )
            _move_membership(
                open_memberships,
                intervals,
                event_agent_id(payload, "agentId"),
                team_id,
                timestamp,
            )
        elif kind == "renewalStarted":
            _close_membership_if_open(
                open_memberships,
                intervals,
                event_agent_id(payload, "predecessor"),
                timestamp,
            )
            _move_membership(
                open_memberships,
                intervals,
                event_agent_id(payload, "successor"),
                team_id,
                timestamp,
            )
        elif kind == "removeAgent":
            _close_membership(
                open_memberships,
                intervals,
                event_agent_id(payload, "agentId"),
                team_id,
                timestamp,
            )
        elif kind == "closeTeam":
            _close_team_memberships(open_memberships, intervals, team_id, timestamp)
        elif kind == "mergeTeams":
            source_team_id = _event_team_id(payload, "sourceTeamId")
            for agent_id in _event_agent_ids(payload, "agents"):
                _move_membership_from_team(
                    open_memberships,
                    intervals,
                    agent_id,
                    source_team_id,
                    team_id,
                    timestamp,
                )
        elif kind == "splitTeam":
            new_team_id = _event_team_id(payload, "newTeamId")
            for agent_id in _event_agent_ids(payload, "agents"):
                _move_membership_from_team(
                    open_memberships,
                    intervals,
                    agent_id,
                    team_id,
                    new_team_id,
                    timestamp,
                )
        elif kind == "splitTeamBack":
            restored_team_id = _event_team_id(payload, "restoredTeamId")
            for agent_id in _event_agent_ids(payload, "agents"):
                _move_membership_from_team(
                    open_memberships,
                    intervals,
                    agent_id,
                    team_id,
                    restored_team_id,
                    timestamp,
                )
    for agent_id, (team_id, start) in open_memberships.items():
        intervals.append(
            MembershipInterval(
                team_id=team_id,
                agent_id=agent_id,
                start=start,
                end=end_time,
            )
        )
    return intervals


def event_payload(row: StoreRow) -> dict[str, object]:
    payload = json.loads(str(row["payload"] or "{}"))
    if not isinstance(payload, dict):
        raise SpiceError("team event payload must be a JSON object")
    return payload


def event_agent_id(payload: dict[str, object], key: str) -> str:
    value = payload.get(key)
    if not isinstance(value, str) or not value:
        raise SpiceError(f"team event payload {key} must be a non-empty string")
    return value


def _event_team_id(payload: dict[str, object], key: str) -> str:
    value = payload.get(key)
    if not isinstance(value, str) or not value:
        raise SpiceError(f"team event payload {key} must be a non-empty string")
    return value


def _event_agent_ids(payload: dict[str, object], key: str) -> list[str]:
    value = payload.get(key)
    if not isinstance(value, list) or not all(
        isinstance(agent_id, str) and agent_id for agent_id in value
    ):
        raise SpiceError(f"team event payload {key} must be a list of agent ids")
    return [str(agent_id) for agent_id in value]


def _event_optional_agent_ids(payload: dict[str, object], key: str) -> list[str]:
    value = payload.get(key, [])
    if not isinstance(value, list) or not all(
        isinstance(agent_id, str) and agent_id for agent_id in value
    ):
        raise SpiceError(f"team event payload {key} must be a list of agent ids")
    return [str(agent_id) for agent_id in value]


def _move_membership(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[MembershipInterval],
    agent_id: str,
    team_id: str,
    timestamp: float,
) -> None:
    current = open_memberships.pop(agent_id, None)
    if current is not None:
        current_team_id, started_at = current
        intervals.append(
            MembershipInterval(
                team_id=current_team_id,
                agent_id=agent_id,
                start=started_at,
                end=timestamp,
            )
        )
    open_memberships[agent_id] = (team_id, timestamp)


def _move_membership_from_team(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[MembershipInterval],
    agent_id: str,
    source_team_id: str,
    destination_team_id: str,
    timestamp: float,
) -> None:
    _close_membership(open_memberships, intervals, agent_id, source_team_id, timestamp)
    open_memberships[agent_id] = (destination_team_id, timestamp)


def _close_membership(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[MembershipInterval],
    agent_id: str,
    team_id: str,
    timestamp: float,
) -> None:
    current = open_memberships.pop(agent_id, None)
    if current is None or current[0] != team_id:
        raise SpiceError(
            f"cannot reconstruct team metric interval for {agent_id} in {team_id}"
        )
    intervals.append(
        MembershipInterval(
            team_id=team_id,
            agent_id=agent_id,
            start=current[1],
            end=timestamp,
        )
    )


def _close_membership_if_open(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[MembershipInterval],
    agent_id: str,
    timestamp: float,
) -> None:
    current = open_memberships.get(agent_id)
    if current is None:
        return
    _close_membership(
        open_memberships,
        intervals,
        agent_id,
        current[0],
        timestamp,
    )


def _close_team_memberships(
    open_memberships: dict[str, tuple[str, float]],
    intervals: list[MembershipInterval],
    team_id: str,
    timestamp: float,
) -> None:
    for agent_id, (current_team_id, started_at) in tuple(open_memberships.items()):
        if current_team_id != team_id:
            continue
        intervals.append(
            MembershipInterval(
                team_id=team_id,
                agent_id=agent_id,
                start=started_at,
                end=timestamp,
            )
        )
        del open_memberships[agent_id]
