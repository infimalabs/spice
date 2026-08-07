"""The readout a lane gets when the allocator has nothing left to give it.

A dry allocator right after `spice task next` is the one condition that ever
releases an agent, and it arrives while the board still visibly holds rows. The
readout has to carry that whole picture -- what is parked, why none of it is
allocatable, and that stopping is the correct move -- or an agent reads a
non-empty board as evidence the allocator missed something and starts hunting.
"""

from __future__ import annotations

import pytest

from spice.tasks import claimstate, render

PARKED_ROWS = {
    "active": [{"uuid": "held-a", "claim_by": "other-lane"}],
    "blocked": [{"uuid": "blocked-a"}, {"uuid": "blocked-b"}],
    "waiting": [{"uuid": "waiting-a"}],
    "oops": [{"uuid": "oops-a"}],
}


@pytest.fixture
def parked_board(monkeypatch):
    """A board holding one row of every kind the allocator will not hand over."""

    def rows_in_scope(filters, _scope):
        if "+BLOCKED" in filters:
            return list(PARKED_ROWS["blocked"])
        return list(PARKED_ROWS["waiting"])

    monkeypatch.setattr(render.tw, "current_actor", lambda: "this-lane")
    monkeypatch.setattr(render.lanes, "team_route_for_actor", lambda _actor: None)
    monkeypatch.setattr(
        render.alloc, "effective_route_filter_args", lambda _actor, _route: []
    )
    monkeypatch.setattr(
        render.alloc,
        "visible_active_rows",
        lambda _actor, scope=None: list(PARKED_ROWS["active"]),
    )
    monkeypatch.setattr(render.alloc, "visible_rows_in_scope", rows_in_scope)
    monkeypatch.setattr(render.alloc, "is_hidden", lambda _row: False)
    monkeypatch.setattr(render.alloc, "oops_rows", lambda: list(PARKED_ROWS["oops"]))


def test_drain_readout_names_every_parked_row_and_why_it_is_parked(parked_board):
    """The counts land next to their reasons, so the board explains itself."""
    board = [line for line in render.drain_complete_lines() if line.startswith("board")]

    assert board == [
        "board: 1 active (held by other lanes), "
        "2 blocked (waiting on their dependencies), "
        "1 waiting (deferred until their scheduled time), "
        "1 oops (parked on the deferred triage board)"
    ]


def test_drain_readout_calls_a_populated_board_the_expected_answer(parked_board):
    """The rows are there and allocation still came back dry: that is correct.

    Naming it as the expected answer is the whole point -- the agent is looking
    at entries it could go touch, and needs to read them as parked on purpose
    rather than as an allocator that overlooked them.
    """
    readout = " ".join(" ".join(render.drain_complete_lines()).split())

    assert "parked on purpose" in readout
    assert "is the expected answer here, not an error" in readout
    assert "There is nothing for you to do." in readout


def test_drain_readout_ties_release_to_the_dry_allocator_alone(parked_board):
    """The license to stop must not generalize past this one condition.

    An agent that learns "a quiet board means done" spins itself down early, so
    the readout names the dry allocator as the only thing that releases it and
    calls out the two lookalikes it must keep working through.
    """
    readout = " ".join(" ".join(render.drain_complete_lines()).split())

    assert "A dry allocator immediately after spice task next is the only thing" in (
        readout
    )
    assert "a board that merely looks quiet, or a low pending count, never does" in (
        readout
    )


def test_drain_readout_tells_the_agent_to_stop_hunting(parked_board):
    """The concrete behavior being replaced, named as the thing to skip."""
    readout = " ".join(" ".join(render.drain_complete_lines()).split())

    assert "stop here rather than hunting the board for something to unstick" in readout


def test_drain_readout_reports_a_board_holding_nothing_else(monkeypatch):
    """A genuinely empty board still gets a board line, so the shape is stable."""
    monkeypatch.setattr(
        render, "_drain_parked_counts", lambda: dict.fromkeys(PARKED_ROWS, 0)
    )

    lines = render.drain_complete_lines()

    assert lines[1] == "board: nothing else visible to this lane"


def test_task_next_ends_a_dry_allocation_with_the_drain_readout(monkeypatch):
    """The readout reaches the command an agent actually runs."""
    monkeypatch.setattr(
        render.claimstate,
        "renew_claim",
        lambda: claimstate.ClaimRenewalResult(True, "renewed"),
    )
    monkeypatch.setattr(render.alloc, "next_task", lambda: None)
    monkeypatch.setattr(
        render, "drain_complete_lines", lambda: ["no available tasks", "board: quiet"]
    )

    output = render.render_next()

    assert output.splitlines()[1:] == ["no available tasks", "board: quiet"]
