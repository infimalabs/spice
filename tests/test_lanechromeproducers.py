"""Every server chrome producer answers in the one facet contract.

The assembler is proven in ``test_lanechrome``; what is proven here is that the
producers hand it what they actually observed. A pass that read the inbox and
nothing else names one facet, a pass that settled a team names the team facets,
and the facts that were never chrome -- discovery errors, lifecycle outcomes,
submission results -- stay in the envelopes that own them.
"""

from __future__ import annotations

from http import HTTPStatus
from types import SimpleNamespace

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.serve import agentapi, lifecycle as serve_lifecycle, observer
from spice.serve.payload.wire import LANE_CHROME_FACET_AUTHORITIES
from spice.serve.team.schema import DEFAULT_LIFETIME
from spice.serve.worktree import inventory
from spice.serve.workroutes import (
    work_tree_send_accepted_response_payload,
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from tests.test_servehelpers import (
    ACTOR_A,
    THREAD_A,
    _patch_agent_status,
    _record_identity,
    _repo,
    _serve_state,
    _target,
)
from tests.test_worktreepayload import (
    _EmptyOpenTaskBoard,
    _InventoryState,
    _State,
    _Target,
    _stub_running_inventory_dependencies,
    _task_facet_board,
)

# A lane whose producer saw the whole world publishes exactly these. Identity and
# lifecycle are absent by contract: their authorities keep no counter yet, and a
# producer that cannot order two observations must publish neither.
WHOLE_LANE_FACETS = {
    "targetId",
    "teamConfig",
    "pendingInbox",
    "taskBoard",
    "renewal",
    "activity",
}


class _FailedDiscoveryState(_State):
    """One surviving target beside a discovery failure that must reach the UI."""

    def __init__(self, target: _Target, errors: list[str]) -> None:
        super().__init__()
        self._target = target
        self._errors = errors

    def worktree_targets(self) -> list[_Target]:
        return [self._target]

    def targets_discovery_errors(self) -> list[str]:
        return list(self._errors)


@pytest.fixture
def empty_task_board(monkeypatch):
    """Answer every inventory pass from one empty board, as the UI's own does."""
    monkeypatch.setattr(
        inventory, "open_task_board_projection", lambda: _EmptyOpenTaskBoard()
    )


def _observer_session(root, target_id: str = "observed") -> SimpleNamespace:
    return SimpleNamespace(
        target=SimpleNamespace(
            id=target_id,
            repo_root=root,
            name="observed-repo",
            branch="main",
            display_name="observed-repo",
        ),
        thread_id="observed-thread",
        transcript=SimpleNamespace(owner_driver=CODEX_DRIVER),
    )


def _facet_authorities(chrome: dict) -> dict[str, str]:
    return {
        facet: value["authority"]
        for facet, value in chrome.items()
        if facet != "targetId"
    }


def _contract_authorities(chrome: dict) -> dict[str, str]:
    return {
        facet: authority
        for facet, authority in LANE_CHROME_FACET_AUTHORITIES.items()
        if facet in chrome
    }


def test_a_fresh_lane_publishes_every_facet_its_producer_observed(
    tmp_path, monkeypatch, empty_task_board
):
    target = _Target(id="wt", repo_root=tmp_path)
    _stub_running_inventory_dependencies(monkeypatch, target=target)

    payload = inventory.work_trees_payload(_InventoryState(target))

    work_tree = payload["workTrees"][0]
    chrome = work_tree["chrome"]
    assert chrome["targetId"] == "wt"
    assert set(chrome) == WHOLE_LANE_FACETS
    assert _facet_authorities(chrome) == _contract_authorities(chrome)
    # The lifecycle facts this pass did observe still ride their own fields, so
    # withholding the facet costs the client nothing it already had.
    assert work_tree["agentProcessStatus"] == "running"
    assert work_tree["statusLine"]["agentVisualStatus"] == "running"


def test_a_discovery_failure_keeps_its_envelope_and_every_surviving_facet(
    tmp_path, monkeypatch, empty_task_board
):
    target = _Target(id="wt", repo_root=tmp_path)
    _stub_running_inventory_dependencies(monkeypatch, target=target)
    state = _FailedDiscoveryState(target, ["could not scan /gone"])

    payload = inventory.work_trees_payload(state)

    # The failure is its own field precisely because it is not chrome: a client
    # keys lane closure off it, and the lanes that did resolve still answer with
    # the facets their own authorities observed.
    assert payload["targetsDiscoveryErrors"] == ["could not scan /gone"]
    assert set(payload["workTrees"][0]["chrome"]) == WHOLE_LANE_FACETS


def test_a_task_filter_change_advances_the_board_facet_epoch(tmp_path, monkeypatch):
    target = _Target(id="wt", repo_root=tmp_path)
    _stub_running_inventory_dependencies(monkeypatch, target=target)
    boards = [
        _task_facet_board("revision-1", "one"),
        _task_facet_board("revision-2", "two"),
    ]
    monkeypatch.setattr(inventory, "open_task_board_projection", boards.pop)

    second = inventory.work_trees_payload(_InventoryState(target))
    first = inventory.work_trees_payload(_InventoryState(target))

    before = first["workTrees"][0]["chrome"]["taskBoard"]
    after = second["workTrees"][0]["chrome"]["taskBoard"]
    # The board's own revision is what orders this facet, so a filter change is
    # a new epoch rather than a lane-wide counter someone had to advance.
    assert before["order"]["epoch"] == "revision-1"
    assert after["order"]["epoch"] == "revision-2"
    assert [
        item["name"] for item in after["value"]["taskFilterInventory"]["filters"]
    ] == ["serve.two"]


def test_the_observer_lane_answers_in_the_producer_contract(tmp_path):
    session = _observer_session(tmp_path)

    target_payload = observer.observer_target_payload(session)

    chrome = target_payload["chrome"]
    assert chrome["targetId"] == "observed"
    # A read-only lane has no activity of its own to report -- it never binds an
    # agent -- so it names the four facts it does stand for and nothing else.
    assert set(chrome) == WHOLE_LANE_FACETS - {"activity"}
    assert _facet_authorities(chrome) == _contract_authorities(chrome)
    assert chrome["teamConfig"]["value"]["teamIdentity"]["teamId"] == (
        "observer-observed"
    )
    assert chrome["pendingInbox"]["value"] == {"count": 0, "label": "0", "keys": []}
    assert chrome["renewal"]["value"]["lifetime"] == "Steer"
    # A read-only lane observes each fact once and never again, so its facets
    # stand at the one order they were minted with.
    assert chrome["pendingInbox"]["order"] == {"epoch": "", "revision": 1}


def test_an_accepted_send_reports_only_the_inbox_it_read(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=False)
    # The accepted route owes this send a lane start it does not wait for.
    # Holding that start still lets the reply report the inbox it published to.
    monkeypatch.setattr(
        serve_lifecycle, "ensure_agent_for_pending_inbox", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_a, **_k: ({"ok": True, "threadId": THREAD_A}, HTTPStatus.OK),
    )

    payload, status = work_tree_send_accepted_response_payload(
        state, target, {"text": "wake this lane"}
    )

    chrome = payload["chrome"]
    assert status == HTTPStatus.OK
    # The accepted route replies before the lane starts: it read the inbox it
    # just published into and settled no team, so the client keeps whatever it
    # already holds for every other facet.
    assert set(chrome) == {"targetId", "pendingInbox"}
    assert chrome["pendingInbox"]["value"]["count"] == 1
    assert (
        chrome["pendingInbox"]["order"]["revision"] == (payload["pendingInboxVersion"])
    )
    assert payload["pendingInboxKeys"] == chrome["pendingInbox"]["value"]["keys"]


def test_a_renewal_send_reports_the_lifetime_with_its_intent(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    state.team_store.create_team(members=[ACTOR_A])
    _record_identity(state, target, ACTOR_A, THREAD_A)
    state.team_store.set_agent_renewal_request(ACTOR_A, requested=True)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    monkeypatch.setattr(
        agentapi,
        "agent_ensure_response_payload",
        lambda *_a, **_k: ({"ok": True, "threadId": THREAD_A}, HTTPStatus.OK),
    )

    payload, status = work_tree_send_response_payload(
        state, target, {"text": "hand this lane over"}
    )

    chrome = payload["chrome"]
    assert status == HTTPStatus.OK
    # The lifetime and the request are one team-store observation, so the facet
    # carries both at the revision that authority counted them at.
    assert chrome["renewal"]["value"] == {
        "lifetime": DEFAULT_LIFETIME,
        "renewalIntent": payload["renewalIntent"],
    }
    assert (
        chrome["renewal"]["order"]["revision"] == (payload["renewalIntent"]["revision"])
    )
    assert chrome["renewal"]["value"]["renewalIntent"]["agentId"] == ACTOR_A
    assert chrome["renewal"]["order"]["revision"] > 0


def test_a_direct_route_reports_only_the_team_it_settled(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)

    payload, status = work_tree_task_drain_response_payload(
        state,
        target,
        {
            "replaceTaskFilters": True,
            "taskFilters": ["serve", "task.review"],
            "lifetime": "Drive",
        },
    )

    chrome = payload["route"]["chrome"]
    assert status == HTTPStatus.OK
    assert payload["route"]["actor"] == ACTOR_A
    # A drain settles the team's filters and reads no inbox and no transcript,
    # so it names the two facets that moved and stays silent on the rest.
    assert set(chrome) == {"targetId", "teamConfig", "taskBoard"}
    assert (
        chrome["taskBoard"]["value"]["effectiveTaskFilters"]
        == (payload["route"]["effectiveTaskFilters"])
    )
    assert (
        chrome["teamConfig"]["value"]["teamIdentity"]
        == (payload["route"]["teamIdentity"])
    )
