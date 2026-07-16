"""Membership-interval reconstruction from team event streams."""

from __future__ import annotations

import json

import pytest

from spice.errors import SpiceError
from spice.serve.team.membership import (
    MembershipInterval,
    membership_intervals_from_events,
)

CREATE_TS = 100.0
ASSIGN_TS = 200.0
REMOVE_TS = 300.0
CLOSE_TS = 400.0
END_TIME = 500.0
AFTER_END_TS = 600.0


def _row(ts: float, kind: str, team_id: str, **payload: object) -> dict[str, object]:
    return {
        "ts": ts,
        "kind": kind,
        "team_id": team_id,
        "payload": json.dumps(payload),
    }


def test_create_team_opens_intervals_closed_at_end_time():
    intervals = membership_intervals_from_events(
        [_row(CREATE_TS, "createTeam", "team-a", members=["agent-1", "agent-2"])],
        end_time=END_TIME,
    )
    assert intervals == [
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=CREATE_TS, end=END_TIME
        ),
        MembershipInterval(
            team_id="team-a", agent_id="agent-2", start=CREATE_TS, end=END_TIME
        ),
    ]


def test_assign_agent_moves_membership_between_teams():
    intervals = membership_intervals_from_events(
        [
            _row(CREATE_TS, "createTeam", "team-a", members=["agent-1"]),
            _row(ASSIGN_TS, "assignAgent", "team-b", agentId="agent-1"),
        ],
        end_time=END_TIME,
    )
    assert intervals == [
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=CREATE_TS, end=ASSIGN_TS
        ),
        MembershipInterval(
            team_id="team-b", agent_id="agent-1", start=ASSIGN_TS, end=END_TIME
        ),
    ]


def test_remove_agent_closes_interval_at_removal_time():
    intervals = membership_intervals_from_events(
        [
            _row(CREATE_TS, "createTeam", "team-a", members=["agent-1"]),
            _row(REMOVE_TS, "removeAgent", "team-a", agentId="agent-1"),
        ],
        end_time=END_TIME,
    )
    assert intervals == [
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=CREATE_TS, end=REMOVE_TS
        ),
    ]


def test_close_team_closes_only_that_teams_memberships():
    intervals = membership_intervals_from_events(
        [
            _row(CREATE_TS, "createTeam", "team-a", members=["agent-1"]),
            _row(CREATE_TS, "createTeam", "team-b", members=["agent-2"]),
            _row(CLOSE_TS, "closeTeam", "team-a"),
        ],
        end_time=END_TIME,
    )
    assert intervals == [
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=CREATE_TS, end=CLOSE_TS
        ),
        MembershipInterval(
            team_id="team-b", agent_id="agent-2", start=CREATE_TS, end=END_TIME
        ),
    ]


def test_merge_teams_moves_agents_from_source_team():
    intervals = membership_intervals_from_events(
        [
            _row(CREATE_TS, "createTeam", "team-a", members=["agent-1"]),
            _row(
                ASSIGN_TS,
                "mergeTeams",
                "team-b",
                sourceTeamId="team-a",
                agents=["agent-1"],
            ),
        ],
        end_time=END_TIME,
    )
    assert intervals == [
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=CREATE_TS, end=ASSIGN_TS
        ),
        MembershipInterval(
            team_id="team-b", agent_id="agent-1", start=ASSIGN_TS, end=END_TIME
        ),
    ]


def test_split_team_and_split_back_round_trip():
    intervals = membership_intervals_from_events(
        [
            _row(CREATE_TS, "createTeam", "team-a", members=["agent-1"]),
            _row(
                ASSIGN_TS,
                "splitTeam",
                "team-a",
                newTeamId="team-a-split",
                agents=["agent-1"],
            ),
            _row(
                REMOVE_TS,
                "splitTeamBack",
                "team-a-split",
                restoredTeamId="team-a",
                agents=["agent-1"],
            ),
        ],
        end_time=END_TIME,
    )
    assert intervals == [
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=CREATE_TS, end=ASSIGN_TS
        ),
        MembershipInterval(
            team_id="team-a-split", agent_id="agent-1", start=ASSIGN_TS, end=REMOVE_TS
        ),
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=REMOVE_TS, end=END_TIME
        ),
    ]


def test_events_after_end_time_stay_outside_the_horizon():
    intervals = membership_intervals_from_events(
        [
            _row(CREATE_TS, "createTeam", "team-a", members=["agent-1"]),
            _row(AFTER_END_TS, "removeAgent", "team-a", agentId="agent-1"),
        ],
        end_time=END_TIME,
    )
    assert intervals == [
        MembershipInterval(
            team_id="team-a", agent_id="agent-1", start=CREATE_TS, end=END_TIME
        ),
    ]


def test_close_mismatch_raises_reconstruction_error():
    with pytest.raises(SpiceError, match="cannot reconstruct"):
        membership_intervals_from_events(
            [_row(REMOVE_TS, "removeAgent", "team-a", agentId="agent-1")],
            end_time=END_TIME,
        )
