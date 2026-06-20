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
from spice.tasks import config as task_config
from spice.tasks import lanes

AGENT_A = "agent-a"
ANCESTOR_THREAD = "ancestor-thread-a"
EXIT_OK = 0
TASK_FILTERS = ("serve.ui", "task.review")
TEAM_ID = "team-main"


def _record_identity(store: ServeTeamStore) -> None:
    store.record_agent_identity(
        actor_id=AGENT_A,
        target_id="wt-a",
        thread_id=AGENT_A,
        actual_driver="codex",
        actual_model="actual-model",
        actual_effort="low",
        actual_service_tier="fast",
        desired_driver="codex",
        desired_model="desired-model",
        desired_effort="high",
        transcript_owner="codex",
    )


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
    _record_identity(store)
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
        task_config.private_project(AGENT_A),
        *expected_filter_terms,
    ]
    assert route["filterArgs"] == expected_filter_args
    assert route["scope"] == "stored"
    assert route["configuredFilterTerms"] == expected_filter_terms
    assert taskdrain["filterArgs"] == expected_filter_args
    assert taskdrain["effectiveTerms"] == expected_filter_terms
    assert taskdrain["scope"] == "stored"
    assert f"serve teams store={store.path} globalRevision={renewal.revision}" in text
    assert f"revision={created.revision} kind=createTeam team={TEAM_ID}" in text
    assert f"route {AGENT_A} team={TEAM_ID} lifetime=Drive" in text
    assert "scope=stored" in text
    assert "routeFilters=agent.agenta.task,project:serve.ui,project:task.review" in text
    assert "taskdrain team=team-main lifetime=Drive applies=yes" in text
    assert f"renewal {AGENT_A} state=pending team={TEAM_ID}" in text
    assert "successor_thread=- slot=0" in text


def test_team_diagnostics_include_requested_renewal_intent(tmp_path):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    store.create_team(team_id=TEAM_ID, members=[AGENT_A])
    _record_identity(store)

    renewal = store.set_agent_renewal_request(AGENT_A, requested=True)
    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)

    assert renewal is not None
    assert payload["renewals"][0]["state"] == "requested"
    assert payload["renewals"][0]["revision"] == renewal.revision
    assert f"renewal {AGENT_A} state=requested team={TEAM_ID}" in text


def test_team_diagnostics_prunes_zero_activity_closed_teams(tmp_path):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    unused = store.create_team(team_id="team-unused", members=[AGENT_A])
    closed_revision = store.remove_agent(unused.team_id, AGENT_A)

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)
    open_team = payload["teams"][0]

    assert payload["globalRevision"] > closed_revision
    assert payload["closedTeams"] == []
    assert [team["teamId"] for team in payload["teamRecords"]] == [open_team["teamId"]]
    assert [event["kind"] for event in payload["events"]] == [
        "createTeam",
        "pruneZeroActivityTeams",
    ]
    assert payload["events"][-1]["payload"] == {
        "count": 1,
        "teams": ["team-unused"],
    }
    assert "revision=" in text
    assert "kind=pruneZeroActivityTeams" in text


def test_empty_team_diagnostics_have_stable_sections(tmp_path):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)
    team = payload["teams"][0]

    assert payload["globalRevision"] == 1
    assert len(payload["events"]) == 1
    assert payload["events"][0]["kind"] == "createTeam"
    assert payload["events"][0]["payload"] == {"members": []}
    assert len(payload["teams"]) == 1
    assert team["status"] == "open"
    assert team["members"] == []
    assert payload["members"] == []
    assert payload["effectiveRoutes"] == []
    assert payload["taskDrainFilters"] == [
        {
            "teamId": team["teamId"],
            "lifetime": "Drive",
            "taskFilters": [],
            "filterTerms": [],
            "effectiveTerms": [],
            "filterArgs": [],
            "applies": True,
            "scope": "stored",
        }
    ]
    assert payload["renewals"] == []
    assert "events:\n  revision=1 kind=createTeam team=" in text
    assert 'payload={"members": []}' in text
    assert "teams:\n  team " in text
    assert " status=open " in text
    assert "members:\n  (none)" in text
    assert "effective routes:\n  (none)" in text
    assert "taskdrain team=" in text
    assert " lifetime=Drive applies=yes " in text
    assert "renewals:\n  (none)" in text


def test_team_diagnostics_reports_drain_as_computed_effective_scope(tmp_path):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    store.create_team(
        team_id=TEAM_ID,
        members=[AGENT_A],
        config=TeamConfig(lifetime="Drain", task_filters=("serve.ui",)),
    )
    expected_effective_terms = lanes.effective_filter_terms(
        {"filter": [], "lifetime": "Drain"}
    )
    expected_filter_args = lanes.filter_terms_args(expected_effective_terms)

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)
    route = payload["effectiveRoutes"][0]
    taskdrain = payload["taskDrainFilters"][0]

    assert route["scope"] == "all-assignable"
    assert route["configuredFilterTerms"] == ["project:serve.ui"]
    assert route["filterTerms"] == expected_effective_terms
    assert route["filterArgs"] == expected_filter_args
    assert route["routeFilters"] == [
        task_config.private_project(AGENT_A),
        *expected_effective_terms,
    ]
    assert taskdrain["scope"] == "all-assignable"
    assert taskdrain["filterTerms"] == ["project:serve.ui"]
    assert taskdrain["effectiveTerms"] == expected_effective_terms
    assert taskdrain["filterArgs"] == expected_filter_args
    assert "route agent-a team=team-main lifetime=Drain scope=all-assignable" in text
    assert (
        "taskdrain team=team-main lifetime=Drain applies=yes scope=all-assignable"
        in text
    )
    assert f"effectiveTerms={','.join(expected_effective_terms)}" in text


def test_serve_teams_cli_json_uses_task_backend(tmp_path, capsys):
    backend = tmp_path / "task-backend"
    args = SimpleNamespace(task_backend=str(backend), json_output=True)
    try:
        result = run_serve_team_diagnostics(args)
        data = json.loads(capsys.readouterr().out)
    finally:
        task_config.set_backend(None)

    assert result == EXIT_OK
    assert data["storePath"] == str(backend / "data" / TEAM_DATABASE_FILENAME)
    assert data["globalRevision"] == 1
    assert len(data["teams"]) == 1
    assert data["teams"][0]["members"] == []


def test_serve_teams_parser_dispatches_json_subcommand(tmp_path):
    backend = tmp_path / "task-backend"

    args = build_parser().parse_args(
        ["serve", "--task-backend", str(backend), "teams", "--json"]
    )

    assert args.func is run_serve_team_diagnostics
    assert args.task_backend == str(backend)
    assert args.json_output is True
