"""Task-add batch parser seam."""

from __future__ import annotations

import shutil

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.serve.team.ids import thread_actor_id
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CREATE,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import claimstate, config, create, identity, tw
from tests.test_reposcaffolding import init_committed_repo as _init_repo

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ACTOR_MEMBER = thread_actor_id(ACTOR)


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-taskbatch")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_parse_add_batch_returns_typed_requests_without_creating_tasks(task_repo):
    requests = create.parse_add_batch(
        [
            "title=Typed batch | project=task.unit | description=Parser seam | "
            "priority=high | flow=todo,review | tags=parser,inline | "
            "acceptance=Parsed without creation | due=2026-06-30"
        ]
    )

    assert requests == [
        create.TaskAddBatchRequest(
            title="Typed batch",
            description="Parser seam",
            project="task.unit",
            priority="high",
            flow=("todo", "review"),
            tags=("parser", "inline"),
            acceptance=("Parsed without creation",),
            due="2026-06-30",
        )
    ]
    assert tw.export(["status:pending"]) == []


def test_parse_add_batch_accrues_every_collection_field_in_input_order(task_repo):
    first_dep = create.add(
        "First batch dependency",
        project="task.unit",
        acceptance=["first dependency exists"],
        origin="ack:1jN54zJJ",
    )
    second_dep = create.add(
        "Second batch dependency",
        project="task.unit",
        acceptance=["second dependency exists"],
        origin="ack:1jN54zJJ",
    )

    requests = create.parse_add_batch(
        [
            "title=Multi-accept batch | project=task.unit | "
            "flow=todo | flow=review | tags=first,second | tags=third | "
            f"after={first_dep} | after={second_dep} | "
            "acceptance=First criterion | acceptance=Second criterion"
        ]
    )

    assert requests == [
        create.TaskAddBatchRequest(
            title="Multi-accept batch",
            project="task.unit",
            flow=("todo", "review"),
            tags=("first", "second", "third"),
            after=(first_dep, second_dep),
            acceptance=("First criterion", "Second criterion"),
        )
    ]
    assert create.REPEATABLE_BATCH_FIELDS == frozenset(
        {"acceptance", "after", "flow", "tags"}
    )
    assert len(tw.export(["status:pending"])) == 2


@pytest.mark.parametrize(
    ("field", "first", "second"),
    [
        ("deferred", "true", "false"),
        ("description", "First paragraph", "Second paragraph"),
        ("due", "2026-07-01", "2026-07-02"),
        (
            "origin",
            "ack:1jN54zJJ",
            "ack:1jN54zJK",
        ),
        ("priority", "high", "low"),
        ("project", "task.unit", "task.cli"),
        ("title", "First", "Second"),
    ],
)
def test_parse_add_batch_rejects_every_duplicate_scalar_field(
    task_repo, field, first, second
):
    segments = [
        "title=Scalar batch",
        "project=task.unit",
        "acceptance=Scalar fields stay singular",
    ]
    if field == "title":
        segments.pop(0)
    if field == "project":
        segments.pop(1)
    segments.extend((f"{field}={first}", f"{field}={second}"))

    with pytest.raises(SpiceError, match=f"duplicate field '{field}'"):
        create.parse_add_batch([" | ".join(segments)])

    assert create.SCALAR_BATCH_FIELDS == frozenset(
        {
            "deferred",
            "description",
            "due",
            "origin",
            "priority",
            "project",
            "title",
        }
    )
    assert tw.export(["status:pending"]) == []


def test_parse_add_batch_reports_actionable_bare_segment_error(task_repo):
    with pytest.raises(SpiceError) as exc_info:
        create.parse_add_batch(
            ["title=Bare segment | project=task.unit | acceptance=ok | missing equals"]
        )

    message = str(exc_info.value)
    assert "field without '='" in message
    assert "use key=value segments" in message
    assert "acceptance, after, flow, and tags may be repeated" in message
    assert tw.export(["status:pending"]) == []


def test_parse_add_batch_accepts_task_directive_prefix(task_repo):
    requests = create.parse_add_batch(
        [
            "TASK: title=Prefixed batch | project=task.unit | "
            "acceptance=Same batch parser"
        ]
    )

    assert requests == [
        create.TaskAddBatchRequest(
            title="Prefixed batch",
            description=None,
            project="task.unit",
            priority=config.DEFAULT_PRIORITY,
            flow=(),
            tags=(),
            acceptance=("Same batch parser",),
            due=None,
        )
    ]
    assert tw.export(["status:pending"]) == []


def test_parse_add_batch_accepts_missing_acceptance(task_repo):
    requests = create.parse_add_batch(
        ["TASK title=Plan batch | project=task.unit | origin=ack:1jN54zJJ"]
    )

    assert requests == [
        create.TaskAddBatchRequest(
            title="Plan batch",
            project="task.unit",
            acceptance=(),
            origin="ack:1jN54zJJ",
        )
    ]
    assert tw.export(["status:pending"]) == []


def test_parse_add_batch_accepts_deferred_field(task_repo):
    requests = create.parse_add_batch(
        [
            "TASK title=Deferred batch | project=task.unit | "
            "acceptance=Deferred until explicit wake | deferred=true"
        ]
    )

    assert requests == [
        create.TaskAddBatchRequest(
            title="Deferred batch",
            project="task.unit",
            acceptance=("Deferred until explicit wake",),
            deferred=True,
        )
    ]
    assert tw.export(["status:pending"]) == []


def test_add_batch_validates_all_lines_before_creating_tasks(task_repo):
    with pytest.raises(SpiceError, match="batch add rejected"):
        create.add_batch(
            [
                "title=Would otherwise create | project=task.unit | acceptance=ok",
                "title=Invalid project depth | project=task | acceptance=bad",
            ]
        )

    assert not any(
        row.get("description") == "Would otherwise create"
        for row in tw.export(["status:pending"])
    )


def test_add_batch_creates_from_parsed_requests(task_repo):
    handles = create.add_batch(
        [
            "title=Created batch | project=task.unit | description=Batch body | "
            "priority=low | acceptance=Batch creation still works | "
            "origin=ack:1jN54zJJ"
        ]
    )
    row = identity.resolve(handles[0])

    assert row["description"] == "Created batch"
    assert row["task_description"] == "Batch body"
    assert row["project"] == "task.unit"
    assert row["priority"] == "L"
    assert row["acceptance"] == "Batch creation still works"


def test_add_batch_creates_multiple_acceptance_criteria(task_repo):
    handles = create.add_batch(
        [
            "title=Created multi-accept batch | project=task.unit | "
            "acceptance=First criterion | acceptance=Second criterion | "
            "origin=ack:1jN54zJJ"
        ]
    )
    row = identity.resolve(handles[0])

    assert row["description"] == "Created multi-accept batch"
    assert row["acceptance"] == "First criterion | Second criterion"


def test_cli_surface_batch_missing_acceptance_routes_to_plan(task_repo):
    handles = create.add_batch(
        [
            "title=Plan routed batch | project=task.unit | due=2026-08-01 | "
            "origin=ack:1jN54zJJ"
        ],
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    )
    row = identity.resolve(handles[0])

    assert row["description"] == "Plan routed batch"
    assert row["project"] == "task.unit"
    assert row["phase"] == "plan"
    assert claimstate.phases_of(row) == ["plan", "todo", "review"]
    assert not str(row.get("acceptance") or "")
    assert row["origin"] == "ack:1jN54zJJ"
    assert str(row.get("due") or "").startswith("20260801")


def test_cli_surface_batch_missing_acceptance_honors_explicit_flow(task_repo):
    handles = create.add_batch(
        [
            "title=Explicit flow batch | project=task.unit | flow=todo,review | "
            "origin=ack:1jN54zJJ"
        ],
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    )
    row = identity.resolve(handles[0])

    assert row["description"] == "Explicit flow batch"
    assert row["phase"] == "todo"
    assert claimstate.phases_of(row) == ["todo", "review"]
    assert not str(row.get("acceptance") or "")


def test_cli_surface_batch_suspect_wording_preserves_existing_plan_flow(task_repo):
    handles = create.add_batch(
        [
            "title=Orphaning explicit plan batch | project=task.unit | "
            "flow=todo,plan,review | acceptance=Explicit flow is intentional | "
            "origin=ack:1jN54zJJ"
        ],
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    )
    row = identity.resolve(handles[0])
    annotations = [ann.get("description", "") for ann in row.get("annotations") or []]

    assert row["description"] == "Orphaning explicit plan batch"
    assert row["phase"] == "todo"
    assert claimstate.phases_of(row) == ["todo", "plan", "review"]
    assert row[config.TASK_WORDING_REVIEW_UDA] == "required"
    assert row["origin"] == "ack:1jN54zJJ"
    assert any(
        "orphaning" in ann and "self-correction required" in ann for ann in annotations
    )


def test_add_batch_deferred_field_creates_waiting_task(task_repo):
    handles = create.add_batch(
        [
            "TASK title=Waiting batch | project=task.unit | "
            "acceptance=Batch deferred until wake | deferred=true | "
            "origin=ack:1jN54zJJ"
        ]
    )
    row = identity.resolve(handles[0])

    assert row["description"] == "Waiting batch"
    assert row["project"] == "task.unit"
    assert str(row.get("wait") or "").startswith("2099")


def test_add_batch_can_mark_cli_creation_surface(task_repo):
    handles = create.add_batch(
        [
            "title=CLI marked batch | project=task.unit | "
            "acceptance=Batch task card source is durable | "
            "origin=ack:1jN54zJJ"
        ],
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    )
    row = identity.resolve(handles[0])

    assert row[config.TASK_CREATION_SURFACE_UDA] == config.TASK_CREATION_SURFACE_CLI


def test_add_batch_results_update_drive_task_filter_with_visible_route(task_repo):
    store = ServeTeamStore()
    team = store.create_team(
        members=[ACTOR_MEMBER],
        config=TeamConfig(lifetime="Drive"),
    )

    results = create.add_batch_results(
        [
            "TASK title=Visible batch | project=task.batch | "
            "acceptance=Batch creation updates routing | "
            "origin=ack:1jN54zJJ"
        ]
    )
    row = identity.resolve(results[0].handle)
    team_config = store.team_config(team.team_id)

    assert row["description"] == "Visible batch"
    assert row["project"] == "task.batch"
    assert results[0].project == "task.batch"
    assert results[0].route_feedback == "route_filter=added:task.batch:auto:create"
    assert team_config.task_filters == ("task.batch",)
    assert [entry.to_payload() for entry in team_config.task_filter_entries] == [
        {"project": "task.batch", "source": TASK_FILTER_SOURCE_AUTO_CREATE}
    ]


def test_add_batch_results_carry_suspect_wording_matches(task_repo):
    results = create.add_batch_results(
        ["TASK title=Orphaning batch | project=task.unit | origin=ack:1jN54zJJ"]
    )

    assert results[0].wording_matches == (
        create.TaskWordingMatch(
            source="title",
            matched="orphaning",
            trigger_family="taste",
            reason="consider 'loose'",
        ),
    )
