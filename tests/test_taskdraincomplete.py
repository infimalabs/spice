"""The readout a lane gets when the allocator has nothing left to give it.

A dry allocator proves that no allocator-selected row is available; it cannot
prove that the current operator request was captured or resolved. The readout
has to carry both that boundary and the parked-board picture, or an agent either
abandons an uncaptured request or starts hunting rows the allocator parked.
"""

from __future__ import annotations

import pytest

from spice.tasks import claimstate, render

NOW = "20260807T060000Z"
LAPSED = "20260807T050000Z"
FRESH = "20260807T070000Z"
PARKED_ROWS = {
    "active": [
        {"uuid": "held-a", "claim_by": "other-lane", "claim_until": FRESH},
        {"uuid": "lapsed-a", "claim_by": "gone-lane", "claim_until": LAPSED},
    ],
    "blocked": [{"uuid": "blocked-a"}, {"uuid": "blocked-b"}],
    "waiting": [{"uuid": "waiting-a"}],
    "oops": [{"uuid": "oops-a"}],
}
EMPTY_COUNTS = dict.fromkeys(
    [label for label, _reason in render.DRAIN_PARKED_REASONS], 0
)


@pytest.fixture
def parked_board(monkeypatch):
    """A board holding one row of every kind the allocator will not hand over."""

    def rows_in_scope(filters, _scope):
        if "+BLOCKED" in filters:
            return list(PARKED_ROWS["blocked"])
        return list(PARKED_ROWS["waiting"])

    monkeypatch.setattr(render.tw, "current_actor", lambda: "this-lane")
    monkeypatch.setattr(render.tw, "now_iso", lambda: NOW)
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
        "1 stale (lapsed claims whose takeover did not land this pass), "
        "2 blocked (waiting on their dependencies), "
        "1 waiting (deferred until their scheduled time), "
        "1 oops (parked on the deferred triage board)"
    ]


def test_drain_readout_counts_a_lapsed_claim_as_stale_not_as_held(parked_board):
    """A lapsed claim is not "someone else's" -- the allocator takes those over.

    `task status` splits active from stale, so folding a lapsed row into the
    held count would both mislabel why it is parked and disagree with the very
    readout an agent would check next.
    """
    counts = render._drain_parked_counts()

    assert (counts["active"], counts["stale"]) == (1, 1)


def test_drain_readout_calls_a_populated_board_the_expected_answer(parked_board):
    """The rows are there and allocation still came back dry: that is correct.

    Naming it as the expected answer is the whole point -- the agent is looking
    at entries it could go touch, and needs to read them as parked on purpose
    rather than as an allocator that overlooked them.
    """
    readout = " ".join(" ".join(render.drain_complete_lines()).split())

    assert "Parked rows are expected, not missed work" in readout
    assert "no allocator-selected task is available to this lane" in readout
    assert (
        "does not decide whether the current operator request or turn is complete"
        in readout
    )


def test_drain_readout_keeps_operator_work_independent_from_allocator_emptiness(
    parked_board,
):
    """Allocator emptiness cannot erase work that never reached the allocator.

    The screenshot regression is an acknowledged port request followed by a dry
    allocator and a final response. The readout must instead require the agent
    to persist that request and consult the allocator again.
    """
    readout = " ".join(" ".join(render.drain_complete_lines()).split())

    assert "operator continuity: continue the current prompt and steering" in readout
    assert "Perform immediate directions now" in readout
    assert "capture durable work as a task, then run spice task next again" in readout
    assert "turn boundary" not in readout
    assert "end this turn" not in readout


def test_drain_readout_tells_the_agent_to_stop_hunting(parked_board):
    """The capture check precedes the instruction not to hunt parked rows."""
    readout = " ".join(" ".join(render.drain_complete_lines()).split())

    capture = readout.index("capture durable work as a task")
    no_hunt = readout.index("do not hunt the parked board")

    assert capture < no_hunt
    assert (
        "A quiet board or low pending count is not evidence that operator work is "
        "complete" in readout
    )


def test_drain_readout_reports_a_board_holding_nothing_else(monkeypatch):
    """A genuinely empty board still gets a board line, so the shape is stable."""
    monkeypatch.setattr(render, "_drain_parked_counts", lambda: dict(EMPTY_COUNTS))

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
