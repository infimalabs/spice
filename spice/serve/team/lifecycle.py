"""Task lifecycle transitions credited to the team that held their actor.

The task plane records which actor moved a task and when, and knows nothing
about teams. The team plane records which team an actor belonged to across
time, and knows nothing about tasks. A team-scoped lifecycle fact is the join
of the two, and making it here — at read time, from both authorities — is what
lets neither plane carry the other's bookkeeping and no third series be
written to remember what the join already says.

An actor outside every team is credited to itself, which is the same lane
identity Serve shows for an unteamed agent.
"""

from __future__ import annotations

import sqlite3
from bisect import bisect_right
from collections.abc import Iterable
from dataclasses import dataclass

from spice.serve.team.ids import thread_actor_id
from spice.serve.team.membership import (
    MembershipInterval,
    membership_intervals_from_events,
)
from spice.tasks.transitions import TaskTransitionKind, task_transitions


@dataclass(frozen=True, slots=True)
class TeamTaskTransition:
    """One lifecycle movement, credited to an actor and that actor's team.

    ``id`` is the task plane's own identity for the transition, so ordering by
    it replays the movements in the order the plane committed them even when
    two land inside the same clock tick.
    """

    id: int
    ts: float
    kind: TaskTransitionKind
    task_id: str
    agent_id: str
    team_id: str


def team_event_rows(connection: sqlite3.Connection) -> list[sqlite3.Row]:
    """Every team event in the order the team plane recorded it."""
    return connection.execute(
        "SELECT ts, kind, team_id, payload FROM events ORDER BY revision"
    ).fetchall()


def team_task_transitions(
    connection: sqlite3.Connection,
    *,
    task_ids: Iterable[str] = (),
    end_time: float,
) -> tuple[TeamTaskTransition, ...]:
    """Derived lifecycle transitions, each credited to a team at its own time.

    Naming ``task_ids`` scopes the task-plane read to those tasks; naming none
    reads the whole plane. Transitions after ``end_time`` are outside the
    reconstruction horizon the membership replay covers, so they are dropped
    rather than credited to a team that had not formed yet.
    """
    transitions = task_transitions(task_ids)
    if not transitions:
        return ()
    intervals = _intervals_by_agent(
        membership_intervals_from_events(team_event_rows(connection), end_time=end_time)
    )
    credited = [
        (transition, _serve_actor(transition.actor))
        for transition in transitions
        if transition.at <= end_time
    ]
    return tuple(
        TeamTaskTransition(
            id=transition.id,
            ts=transition.at,
            kind=transition.kind,
            task_id=transition.task_id,
            agent_id=actor,
            team_id=_team_at(intervals, actor, transition.at),
        )
        for transition, actor in credited
    )


def _serve_actor(actor: str) -> str:
    """The team plane's name for the actor the task plane recorded.

    The task plane stores a bare canonical thread id, because that is what a
    Taskwarrior UDA value may hold; the team plane keys every membership,
    identity and lane on the ``thread:`` actor built from the same id. The
    rename is the join's own business, and doing it here is what keeps either
    plane from having to spell the other's names. A movement no one held names
    no actor in either vocabulary.
    """
    return thread_actor_id(actor) if actor else ""


def _intervals_by_agent(
    intervals: Iterable[MembershipInterval],
) -> dict[str, tuple[tuple[float, ...], tuple[MembershipInterval, ...]]]:
    """Index membership spans per actor for repeated point-in-time lookups."""
    by_agent: dict[str, list[MembershipInterval]] = {}
    for interval in intervals:
        by_agent.setdefault(interval.agent_id, []).append(interval)
    indexed: dict[str, tuple[tuple[float, ...], tuple[MembershipInterval, ...]]] = {}
    for agent_id, spans in by_agent.items():
        spans.sort(key=lambda span: span.start)
        indexed[agent_id] = (
            tuple(span.start for span in spans),
            tuple(spans),
        )
    return indexed


def _team_at(
    intervals: dict[str, tuple[tuple[float, ...], tuple[MembershipInterval, ...]]],
    agent_id: str,
    moment: float,
) -> str:
    """The team the actor belonged to at one instant, or the actor's own lane.

    Memberships for one actor never overlap — joining a team closes the span
    on the previous one — so the latest span that had started is the only
    candidate, and it credits the movement only if it had not yet closed.
    """
    indexed = intervals.get(agent_id)
    if indexed is None:
        return agent_id
    starts, spans = indexed
    position = bisect_right(starts, moment)
    if position:
        span = spans[position - 1]
        if moment <= span.end:
            return span.team_id
    return agent_id
