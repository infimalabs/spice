"""Every server chrome producer answers in the one facet contract.

The assembler is proven in ``test_lanechrome``; what is proven here is that the
producers hand it what they actually observed. A pass that read the inbox and
nothing else names one facet, a pass that settled a team names the team facets,
and the facts that were never chrome -- discovery errors, lifecycle outcomes,
submission results -- stay in the envelopes that own them.
"""

from __future__ import annotations

import hashlib
from http import HTTPStatus
import shutil
from types import SimpleNamespace

import pytest

from spice.agent.driver import CODEX_DRIVER
from spice.errors import SpiceError
from spice.serve import agentapi, lifecycle as serve_lifecycle, observer, taskboard
from spice.serve.lifecycle import LifecycleDecision
from spice.serve.payload import lane, message
from spice.serve.payload.chrome import (
    LaneChromeObservation,
    LaneChromeOrder,
    assemble_lane_chrome,
)
from spice.serve.payload.identity import team_facts_for_actor, team_identity_payload
from spice.serve.payload.wire import LANE_CHROME_FACET_AUTHORITIES
from spice.serve.team.store import ServeTeamStore
from spice.serve.team.schema import DEFAULT_LIFETIME
from spice.serve.worktree import inventory
from spice.serve.workroutes import (
    work_tree_send_accepted_response_payload,
    work_tree_send_response_payload,
    work_tree_task_drain_response_payload,
)
from spice.tasks import config as task_config
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
    AFTER_GENERATION,
    BEFORE_GENERATION,
    STABLE_GENERATION,
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
TEAM_REVISION_BEFORE_CONFIG = 11
TEAM_REVISION_AFTER_CONFIG = 12


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
        _task_facet_board(BEFORE_GENERATION, "one"),
        _task_facet_board(AFTER_GENERATION, "two"),
    ]
    monkeypatch.setattr(inventory, "open_task_board_projection", boards.pop)

    second = inventory.work_trees_payload(_InventoryState(target))
    first = inventory.work_trees_payload(_InventoryState(target))

    before = first["workTrees"][0]["chrome"]["taskBoard"]
    after = second["workTrees"][0]["chrome"]["taskBoard"]
    # The board's own revision is what orders this facet, so a filter change is
    # a new epoch rather than a lane-wide counter someone had to advance.
    assert before["order"]["epoch"] == BEFORE_GENERATION
    assert after["order"]["epoch"] == AFTER_GENERATION
    assert [
        item["name"] for item in after["value"]["taskFilterInventory"]["filters"]
    ] == ["serve.two"]


def test_a_team_config_change_advances_joined_board_and_renewal_facets() -> None:
    inventory_payload = _task_facet_board(
        STABLE_GENERATION,
        "same",
    ).task_filter_inventory
    before = lane.lane_chrome_payload(
        target_id="wt",
        team_identity={
            "state": "member",
            "teamId": "team-a",
            "teamRevision": TEAM_REVISION_BEFORE_CONFIG,
            "configRevision": 3,
        },
        team_facts={
            "taskFilters": ["serve.old"],
            "taskFilterEntries": [],
            "effectiveTaskFilters": ["serve.old"],
            "lifetime": "Drive",
        },
        renewal_intent={"revision": 0, "requested": False},
        task_filter_inventory=inventory_payload,
    )
    after = lane.lane_chrome_payload(
        target_id="wt",
        team_identity={
            "state": "member",
            "teamId": "team-a",
            "teamRevision": TEAM_REVISION_AFTER_CONFIG,
            "configRevision": 4,
        },
        team_facts={
            "taskFilters": ["serve.new"],
            "taskFilterEntries": [],
            "effectiveTaskFilters": ["serve.new"],
            "lifetime": "Drain",
        },
        renewal_intent={"revision": 0, "requested": False},
        task_filter_inventory=inventory_payload,
    )

    assert before["taskBoard"]["order"] == {
        "epoch": STABLE_GENERATION,
        "revision": TEAM_REVISION_BEFORE_CONFIG,
    }
    assert after["taskBoard"]["order"] == {
        "epoch": STABLE_GENERATION,
        "revision": TEAM_REVISION_AFTER_CONFIG,
    }
    assert before["renewal"]["order"]["revision"] == TEAM_REVISION_BEFORE_CONFIG
    assert after["renewal"]["order"]["revision"] == TEAM_REVISION_AFTER_CONFIG
    assert after["renewal"]["value"]["lifetime"] == "Drain"


def _real_board_chrome(target_id: str) -> dict:
    """Build board chrome the way a lane pass does, off the live projection."""
    return lane.lane_chrome_payload(
        target_id=target_id,
        team_facts={},
        task_filter_inventory=(
            taskboard.open_task_board_projection().task_filter_inventory
        ),
    )


def test_a_remade_task_store_supersedes_the_generation_it_replaced(
    tmp_path, monkeypatch
):
    backend = tmp_path / "task-backend"
    monkeypatch.setenv(task_config.TASK_BACKEND_ENV, str(backend))
    monkeypatch.setattr(taskboard.tw, "export", lambda *_args, **_kwargs: [])
    task_config.mark_task_backend_changed("task")

    before = _real_board_chrome("wt")["taskBoard"]
    shutil.rmtree(backend)
    after = _real_board_chrome("wt")["taskBoard"]

    remade = LaneChromeObservation(
        "taskBoard", LaneChromeOrder(epoch=after["order"]["epoch"]), after["value"]
    )
    replaced = {"taskBoard": LaneChromeOrder(epoch=before["order"]["epoch"])}

    # A store deleted and remade counts its revisions from the start again, so
    # the generation is the only thing that can carry the lane across it. Both
    # epochs come from the authority itself; nothing here writes one, which is
    # what makes the comparison worth anything. Re-observing the same
    # generation publishes nothing, which is what says the first result is a
    # supersession rather than an assembler that republishes whatever it holds.
    assert before["order"]["epoch"].isdigit()
    assert after["order"]["epoch"].isdigit()
    assert assemble_lane_chrome("wt", [remade], published=replaced).changed == (
        "taskBoard",
    )
    assert (
        assemble_lane_chrome(
            "wt", [remade], published={"taskBoard": remade.order}
        ).changed
        == ()
    )


def test_a_hash_identity_never_reaches_the_board_facet_as_an_epoch():
    # The inbox keeps a digest beside its version in one payload, so a digest is
    # exactly what a future producer reaches for by mistake. It orders at
    # random, and the browser would apply it as a well-formed order and then
    # refuse every later generation that happened to hash lower.
    digest = hashlib.blake2s(b"lane-chrome", digest_size=16).hexdigest()

    with pytest.raises(SpiceError, match="must be a decimal count"):
        lane.lane_chrome_payload(
            target_id="wt",
            team_facts={},
            task_filter_inventory={"revision": digest},
        )


def _real_activity_chrome(last_assistant_at: str) -> dict:
    """Build activity chrome the way a lane pass does, off a transcript stamp."""
    return lane.lane_chrome_payload(
        target_id="wt", last_assistant_at=last_assistant_at
    )["activity"]


def test_a_hash_identity_never_reaches_the_activity_facet_as_an_epoch():
    # The other epoch-carrying facet reaches for a transcript instant, so the
    # same digest wired here is the same mistake. A transcript's malformed line
    # must not end the pass, so this one is refused into no generation at all
    # rather than raised, and the browser is never handed an order that moves at
    # random. The instant beside it is still reported: what the facet describes
    # is unchanged, only the authority's claim to have dated it is withheld.
    digest = hashlib.blake2s(b"lane-chrome", digest_size=16).hexdigest()

    refused = _real_activity_chrome(digest)
    dated = _real_activity_chrome("2026-07-26T05:49:43.256080Z")

    assert refused["order"]["epoch"] == ""
    assert refused["value"]["lastAssistantAt"] == digest
    assert dated["order"]["epoch"].isdigit()


def test_the_activity_generation_orders_stamps_across_a_written_offset():
    # 07:49:43+02:00 is 05:49:43Z, a second before the stamp below it, yet it
    # sorts after as text -- so an authority whose driver writes a local offset
    # would pin the facet and refuse everything that followed. Counting the
    # instant is what makes the later one land.
    earlier = _real_activity_chrome("2026-07-26T07:49:43+02:00")
    later = _real_activity_chrome("2026-07-26T05:49:44Z")

    observed = LaneChromeObservation(
        "activity", LaneChromeOrder(epoch=later["order"]["epoch"]), later["value"]
    )
    landed = assemble_lane_chrome(
        "wt",
        [observed],
        published={"activity": LaneChromeOrder(epoch=earlier["order"]["epoch"])},
    )

    assert earlier["value"]["lastAssistantAt"] > later["value"]["lastAssistantAt"]
    assert int(later["order"]["epoch"]) > int(earlier["order"]["epoch"])
    assert landed.changed == ("activity",)


TEAM_ACTOR = "agent-a"
TEAM_STORE_FACETS = ("teamConfig", "renewal")


def _team_store(path) -> ServeTeamStore:
    """Open a store the way a restarted serve does, and seat one team in it.

    The schema memo is process-wide, so discarding this path is what stands in
    for the restart that actually separates a replaced store from the one that
    remade it.
    """
    ServeTeamStore._initialized_paths.discard(path)
    store = ServeTeamStore(path=path)
    store.create_team(members=[TEAM_ACTOR])
    return store


def _real_team_chrome(store: ServeTeamStore) -> dict:
    """Build team chrome the way a lane pass does, off the live team store."""
    facts = team_facts_for_actor(store, TEAM_ACTOR)
    return lane.lane_chrome_payload(
        target_id="wt",
        team_identity=team_identity_payload(facts),
        team_facts=facts,
        renewal_intent=facts["renewalIntent"],
    )


def _orders(chrome: dict) -> dict:
    return {
        facet: LaneChromeOrder(
            epoch=chrome[facet]["order"]["epoch"],
            revision=chrome[facet]["order"]["revision"],
        )
        for facet in TEAM_STORE_FACETS
    }


def test_a_remade_team_store_supersedes_the_generation_it_replaced(tmp_path):
    path = tmp_path / "teams.sqlite3"

    replaced = _real_team_chrome(_team_store(path))
    path.unlink()
    remade = _real_team_chrome(_team_store(path))

    remade_orders = _orders(remade)
    observed = [
        LaneChromeObservation(facet, remade_orders[facet], remade[facet]["value"])
        for facet in TEAM_STORE_FACETS
    ]

    # Both stores were seated the same way, so their revisions match exactly and
    # the generation is the only thing left that can say which store spoke last.
    # Both come from the authority; nothing here writes one. Re-observing the
    # same generation publishes nothing, which is what says the first result is
    # a supersession rather than an assembler republishing what it holds.
    assert _orders(replaced) == {
        facet: LaneChromeOrder(
            epoch=replaced[facet]["order"]["epoch"],
            revision=remade[facet]["order"]["revision"],
        )
        for facet in TEAM_STORE_FACETS
    }
    assert (
        assemble_lane_chrome("wt", observed, published=_orders(replaced)).changed
        == TEAM_STORE_FACETS
    )
    assert assemble_lane_chrome("wt", observed, published=remade_orders).changed == ()
    assert int(remade["teamConfig"]["order"]["epoch"]) > int(
        replaced["teamConfig"]["order"]["epoch"]
    )


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


def test_a_send_followup_carries_its_lifecycle_outcome_beside_the_facets(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    target = _target(repo)
    state = _serve_state(tmp_path, target)
    _record_identity(state, target, ACTOR_A, THREAD_A)
    _patch_agent_status(monkeypatch, thread_id=THREAD_A, running=True)
    ensure = {"ok": True, "threadId": THREAD_A, "action": "start"}

    payload = message.messages_payload_for_worktree(
        state,
        target,
        limit=5,
        decision=LifecycleDecision(
            thread_id=THREAD_A,
            predecessor_actor=ACTOR_A,
            renewal_intent=False,
            agent_ensure=ensure,
        ),
    )

    chrome = payload["chrome"]
    # The follow-up is the render that reports the start its send queued. That
    # outcome belongs to the reconciler, whose facet no producer may mint yet,
    # so it stays the flat field a client already keys its lane state off while
    # the facts that do have counting authorities ride the facets beside it.
    assert payload["agentEnsure"] == ensure
    assert payload["agentProcessStatus"] == "running"
    assert set(chrome) == WHOLE_LANE_FACETS
    assert _facet_authorities(chrome) == _contract_authorities(chrome)


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
    assert (
        chrome["teamConfig"]["order"]["revision"]
        == payload["route"]["teamIdentity"]["teamRevision"]
    )
    assert (
        chrome["taskBoard"]["order"]["revision"]
        == payload["route"]["teamIdentity"]["teamRevision"]
    )
