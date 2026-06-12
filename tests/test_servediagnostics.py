import json
from types import SimpleNamespace

from spice.cli.parser import build_parser
from spice.serve.cli import run_serve_team_diagnostics
from spice.serve.diagnostics import render_team_diagnostics, team_diagnostics_payload
from spice.serve.teams import (
    TEAM_DATABASE_FILENAME,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import lanes, ops
from spice.tasks import config as task_config

AGENT_A = "agent-a"
ANCESTOR_THREAD = "ancestor-thread-a"
EXIT_OK = 0
EMPTY_REVISION = 0
TASK_FILTERS = ("serve.ui", "task.review")
TEAM_ID = "team-main"


def test_team_diagnostics_include_events_routes_and_taskdrain_filters(tmp_path):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    created = store.create_team(
        team_id=TEAM_ID,
        members=[AGENT_A],
        config=TeamConfig(
            lifetime="Drive",
            task_filters=TASK_FILTERS,
            selected_view="queue",
        ),
    )
    renewal = store.record_pending_renewal(
        agent_id=AGENT_A,
        ancestor_thread_id=ANCESTOR_THREAD,
    )
    expected_filter_terms = sorted(f"project:{item}" for item in TASK_FILTERS)
    expected_filter_args = lanes.filter_terms_args(expected_filter_terms)

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)
    route = payload["effectiveRoutes"][0]
    taskdrain = payload["taskDrainFilters"][0]

    assert payload["globalRevision"] == renewal.revision
    assert payload["events"][0]["kind"] == "createTeam"
    assert payload["events"][-1]["kind"] == "renewalPending"
    assert payload["teams"][0]["teamId"] == created.team_id
    assert payload["members"][0]["agentId"] == AGENT_A
    assert route["routeFilters"] == [
        ops.default_project(AGENT_A),
        *expected_filter_terms,
    ]
    assert route["filterArgs"] == expected_filter_args
    assert taskdrain["filterArgs"] == expected_filter_args
    assert f"serve teams store={store.path} globalRevision={renewal.revision}" in text
    assert f"revision={created.revision} kind=createTeam team={TEAM_ID}" in text
    assert f"route {AGENT_A} team={TEAM_ID} lifetime=Drive" in text
    assert "routeFilters=agent.agenta.task,project:serve.ui,project:task.review" in text
    assert "taskdrain team=team-main lifetime=Drive applies=yes" in text
    assert f"renewal {AGENT_A} state=pending team={TEAM_ID}" in text


def test_empty_team_diagnostics_have_stable_sections(tmp_path):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)

    assert payload["globalRevision"] == EMPTY_REVISION
    assert payload["events"] == []
    assert payload["effectiveRoutes"] == []
    assert "events:\n  (none)" in text
    assert "teams:\n  (none)" in text
    assert "effective routes:\n  (none)" in text
    assert "taskdrain filters:\n  (none)" in text
    assert "renewals:\n  (none)" in text


def test_serve_teams_cli_json_uses_task_backend(tmp_path, capsys):
    backend = tmp_path / "task-backend"
    args = SimpleNamespace(task_backend=str(backend), json_output=True)
    try:
        result = run_serve_team_diagnostics(args)
        data = json.loads(capsys.readouterr().out)
    finally:
        task_config.set_backend(None)

    assert result == EXIT_OK
    assert data["storePath"] == str(backend / TEAM_DATABASE_FILENAME)
    assert data["globalRevision"] == EMPTY_REVISION


def test_serve_teams_parser_dispatches_json_subcommand(tmp_path):
    backend = tmp_path / "task-backend"

    args = build_parser().parse_args(
        ["serve", "--task-backend", str(backend), "teams", "--json"]
    )

    assert args.func is run_serve_team_diagnostics
    assert args.task_backend == str(backend)
    assert args.json_output is True
