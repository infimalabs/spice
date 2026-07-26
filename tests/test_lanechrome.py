"""The server-side lane chrome projection."""

from __future__ import annotations

import ast
import importlib.util
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.serve.payload.chrome import (
    LaneChromeObservation,
    LaneChromeOrder,
    assemble_lane_chrome,
)
from spice.serve.payload.wire import LANE_CHROME_FACET_AUTHORITIES

TARGET = "target-a"
OTHER_TARGET = "target-b"

IDENTITY = {
    "displayName": "spice-h",
    "target": {
        "id": TARGET,
        "worktreeName": "spice-h",
        "repoRoot": "/repo/spice-h",
        "branch": "main-h",
    },
    "driver": {"desired": "claude", "actual": "claude", "transcriptOwner": "claude"},
    "thread": {"state": "bound", "threadId": "thread-1"},
    "launch": {"desired": {"model": "opus"}, "actual": {"model": "opus"}},
}
TEAM_CONFIG = {"teamIdentity": {"state": "member", "teamId": "team-1"}}
PENDING_INBOX = {"count": 2, "label": "2", "keys": ["first", "second"]}
TASK_BOARD = {
    "taskFilters": ["serve"],
    "taskFilterEntries": [{"project": "serve", "source": "manual"}],
    "effectiveTaskFilters": ["serve"],
    "taskFilterInventory": {"revision": "17"},
    "privateTaskCount": 0,
}
LIFECYCLE = {"processStatus": "running", "visualStatus": "working"}
RENEWAL = {"lifetime": "Drive", "renewalIntent": {"state": "pending", "revision": 3}}
ACTIVITY = {"lastAssistantAt": "2026-07-25T04:00:00Z"}

FACET_VALUES = {
    "identity": IDENTITY,
    "teamConfig": TEAM_CONFIG,
    "pendingInbox": PENDING_INBOX,
    "taskBoard": TASK_BOARD,
    "lifecycle": LIFECYCLE,
    "renewal": RENEWAL,
    "activity": ACTIVITY,
}


def every_facet_observed() -> list[LaneChromeObservation]:
    """One observation per facet, each ordered in its own authority's counter."""
    return [
        LaneChromeObservation(facet, LaneChromeOrder(revision=index + 1), value)
        for index, (facet, value) in enumerate(FACET_VALUES.items())
    ]


def test_every_facet_carries_its_own_authority_and_freshness_token() -> None:
    projection = assemble_lane_chrome(TARGET, every_facet_observed())

    authorities = {
        facet: projection.payload[facet]["authority"] for facet in FACET_VALUES
    }
    orders = [projection.payload[facet]["order"] for facet in FACET_VALUES]
    assert authorities == dict(LANE_CHROME_FACET_AUTHORITIES)
    assert orders == [
        {"epoch": "", "revision": index + 1} for index in range(len(FACET_VALUES))
    ]
    assert projection.changed == tuple(LANE_CHROME_FACET_AUTHORITIES)


def test_two_callers_observing_in_different_orders_assemble_one_payload() -> None:
    observed = every_facet_observed()

    forward = assemble_lane_chrome(TARGET, observed).payload
    backward = assemble_lane_chrome(TARGET, list(reversed(observed))).payload

    assert forward == backward
    assert list(forward) == list(backward)


def test_the_assembler_reaches_only_the_error_type_and_the_wire_contract() -> None:
    """No assembler path can ensure inbox work, launch, claim, or read history.

    Stated as the whole reachable ``spice`` surface rather than a list of
    forbidden modules, so a future import that opens one of those doors has to
    be argued for here instead of slipping past a denylist nobody updated.
    """
    closure = spice_import_closure("spice.serve.payload.chrome")

    assert closure == {"spice.errors", "spice.serve.payload.wire"}


def test_an_unchanged_facet_is_left_out_while_a_moved_one_is_sent() -> None:
    first = assemble_lane_chrome(
        TARGET,
        [
            LaneChromeObservation("taskBoard", LaneChromeOrder("rev-8"), TASK_BOARD),
            LaneChromeObservation("activity", LaneChromeOrder(revision=1), ACTIVITY),
        ],
    )
    moved = {"lastAssistantAt": "2026-07-25T05:00:00Z"}

    second = assemble_lane_chrome(
        TARGET,
        [
            LaneChromeObservation("taskBoard", LaneChromeOrder("rev-8"), TASK_BOARD),
            LaneChromeObservation("activity", LaneChromeOrder(revision=2), moved),
        ],
        published=first.orders,
    )

    assert second.changed == ("activity",)
    assert second.payload == {
        "targetId": TARGET,
        "activity": {
            "authority": LANE_CHROME_FACET_AUTHORITIES["activity"],
            "order": {"epoch": "", "revision": 2},
            "value": moved,
        },
    }
    assert second.orders["taskBoard"] == LaneChromeOrder("rev-8")


def test_one_task_board_reaches_every_lane_as_the_same_value() -> None:
    """The board is revision-owned by its authority, not rebuilt per lane."""
    observation = LaneChromeObservation(
        "taskBoard", LaneChromeOrder("rev-8"), TASK_BOARD
    )

    here = assemble_lane_chrome(TARGET, [observation])
    there = assemble_lane_chrome(OTHER_TARGET, [observation])

    assert here.payload["taskBoard"]["value"] is there.payload["taskBoard"]["value"]
    assert (here.target_id, there.target_id) == (TARGET, OTHER_TARGET)


def test_a_lane_whose_discovery_failed_publishes_no_facet() -> None:
    """A caller that observed nothing says nothing, so the browser keeps chrome.

    Publishing cleared facets here is how a transient discovery failure empties
    a lane that is still perfectly alive.
    """
    projection = assemble_lane_chrome(TARGET, [])

    assert projection.payload == {"targetId": TARGET}
    assert projection.changed == ()


def test_an_authority_reporting_nothing_left_clears_the_facet() -> None:
    projection = assemble_lane_chrome(
        TARGET, [LaneChromeObservation("renewal", LaneChromeOrder(revision=9))]
    )

    assert projection.payload["renewal"]["value"] is None
    assert projection.changed == ("renewal",)


def test_an_observer_reading_the_same_facts_assembles_the_owner_payload() -> None:
    """Observer mode is a caller with no high-water marks, not a second shape."""
    observed = every_facet_observed()
    owner = assemble_lane_chrome(TARGET, observed)

    observer = assemble_lane_chrome(TARGET, observed, published={})

    assert observer.payload == owner.payload
    assert dict(observer.orders) == dict(owner.orders)


def test_a_renewal_transition_replaces_the_facet_whole() -> None:
    """Successor identity lands intact rather than blended with predecessor."""
    predecessor = {
        "lifetime": "Drive",
        "renewalIntent": {"state": "pending", "ancestorThreadId": "thread-1"},
    }
    successor = {
        "lifetime": "Drive",
        "renewalIntent": {"state": "started", "successorThreadId": "thread-2"},
    }

    projection = assemble_lane_chrome(
        TARGET,
        [
            LaneChromeObservation("renewal", LaneChromeOrder(revision=5), successor),
            LaneChromeObservation("renewal", LaneChromeOrder(revision=4), predecessor),
        ],
    )

    assert projection.payload["renewal"]["value"] == successor
    assert projection.payload["renewal"]["order"] == {"epoch": "", "revision": 5}


def test_two_source_versions_claiming_one_order_are_refused() -> None:
    """Keeping either would make the payload depend on arrival order."""
    other_board = {**TASK_BOARD, "privateTaskCount": 4}

    with pytest.raises(SpiceError, match="conflicting values"):
        assemble_lane_chrome(
            TARGET,
            [
                LaneChromeObservation(
                    "taskBoard", LaneChromeOrder("rev-8"), TASK_BOARD
                ),
                LaneChromeObservation(
                    "taskBoard", LaneChromeOrder("rev-8"), other_board
                ),
            ],
        )


def test_a_later_epoch_supersedes_under_natural_order() -> None:
    """Generation 10 follows generation 9 even resuming from a lower revision.

    The authority restarted its counter, so revision 1 of the tenth generation
    is newer than revision 200 of the ninth. Plain collation reads "gen-10"
    below "gen-9" and would strand the lane on the older observation forever.
    """
    resumed = {"lastAssistantAt": "2026-07-25T05:00:00Z"}
    ninth = LaneChromeObservation("activity", LaneChromeOrder("gen-9", 200), ACTIVITY)
    tenth = LaneChromeObservation("activity", LaneChromeOrder("gen-10", 1), resumed)

    forward = assemble_lane_chrome(TARGET, [ninth, tenth]).payload
    backward = assemble_lane_chrome(TARGET, [tenth, ninth]).payload

    assert forward == backward
    assert forward["activity"] == {
        "authority": LANE_CHROME_FACET_AUTHORITIES["activity"],
        "order": {"epoch": "gen-10", "revision": 1},
        "value": resumed,
    }


def test_a_target_id_is_required() -> None:
    with pytest.raises(SpiceError, match="requires a target id"):
        assemble_lane_chrome("   ", every_facet_observed())


def test_a_facet_outside_the_contract_is_refused() -> None:
    with pytest.raises(SpiceError, match="unknown lane chrome facet: laneInfo"):
        assemble_lane_chrome(
            TARGET, [LaneChromeObservation("laneInfo", LaneChromeOrder(), {})]
        )


def test_a_revision_cannot_count_backwards() -> None:
    with pytest.raises(SpiceError, match="cannot count backwards"):
        LaneChromeOrder(revision=-1)


def spice_import_closure(root: str) -> set[str]:
    """Every ``spice`` module reachable from ``root`` by import, minus itself."""
    seen: set[str] = set()
    pending = [root]
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        pending.extend(module_imports(name))
    return seen - {root}


def module_imports(name: str) -> set[str]:
    source = module_source(name)
    if not source:
        return set()
    imported: set[str] = set()
    for node in ast.walk(ast.parse(source.read_text(encoding="utf-8"))):
        if isinstance(node, ast.Import):
            imported.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
            imported.add(node.module)
            imported.update(f"{node.module}.{alias.name}" for alias in node.names)
    return {
        candidate
        for candidate in imported
        if candidate.startswith("spice.") and module_source(candidate)
    }


def module_source(name: str) -> Path | None:
    try:
        spec = importlib.util.find_spec(name)
    except (ImportError, AttributeError, ValueError):
        return None
    origin = getattr(spec, "origin", "") or ""
    return Path(origin) if origin.endswith(".py") else None
