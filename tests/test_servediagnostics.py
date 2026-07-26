import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.cli.parser import build_parser
from spice.paths import set_state_backend
from spice.serve.cli import run_serve_team_diagnostics
from spice.serve.diagnostics import render_team_diagnostics, team_diagnostics_payload
from spice.serve.team.projection import (
    AGENT_ACTIVITY,
    PROJECTION_SCHEMA_VERSION,
    PROJECTION_STATUS_INCOMPATIBLE,
    PROJECTION_STATUS_STALE,
    ServeProjectionStore,
    rebuild_projection_family,
)
from spice.serve.team.store import (
    TEAM_DATABASE_FILENAME,
    ServeTeamStore,
    TeamConfig,
)
from spice.tasks import config as task_config
from spice.tasks import lanes
from tests.test_teamstorehelpers import store_remove_agent

AGENT_A = "agent-a"
ANCESTOR_THREAD = "ancestor-thread-a"
EXIT_OK = 0
TASK_A = "task-a"
TASK_FILTERS = ("serve.ui", "task.review")
# The one instant the task plane records, stated as the epoch seconds every
# other canonical family in this table publishes.
TASK_OPERATION_EPOCH = 1785095505.29108
TEAM_ID = "team-main"


def test_operator_projection_docs_use_the_registered_recovery_command():
    readme = (Path(__file__).parents[1] / "README.md").read_text(encoding="utf-8")
    command = AGENT_ACTIVITY.recovery_action.rsplit(" ", 1)[0]

    assert f"Run `{command}`" in readme
    assert "`spiceprojections.sqlite3`" in readme
    assert "`spiceteams.sqlite3`" in readme


def _record_identity(store: ServeTeamStore) -> None:
    store.record_agent_identity(
        actor_id=AGENT_A,
        target_id="wt-a",
        thread_id=AGENT_A,
        actual_driver="codex",
        actual_model="actual-model",
        actual_effort="low",
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
    metric_families = {row["family"]: row for row in payload["metricFamilies"]}
    assert set(metric_families) == {
        "agentActivity",
        "directiveLifecycle",
        "taskLifecycle",
        "teamAttribution",
    }
    assert metric_families["agentActivity"]["projectionGeneration"] == 1
    assert metric_families["agentActivity"]["status"] == "ready"
    assert metric_families["directiveLifecycle"]["projectionGeneration"] is None
    assert metric_families["taskLifecycle"]["storageClass"] == "canonical authority"
    assert metric_families["teamAttribution"]["cursor"] == (
        f"global team revision {renewal.revision}"
    )
    assert "metric families:" in text
    assert "agentActivity owner=" in text
    assert "directiveLifecycle owner=" in text


def test_task_lifecycle_freshness_dates_the_recorded_operation(tmp_path, task_plane):
    """The instant the task plane recorded, carried through to both surfaces.

    Freshness here is the same kind of answer the neighbouring canonical
    families give -- epoch seconds naming when that authority last recorded
    something -- so a reader comparing the column across families is comparing
    one thing. The store's own file times cannot supply it: Taskwarrior
    rewrites the database on every read, and this value must move only when an
    operation lands.
    """
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    store.create_team(team_id=TEAM_ID, members=[AGENT_A])
    _record_identity(store)
    task_plane.record(
        "claim", task_id=TASK_A, agent_id=AGENT_A, ts=TASK_OPERATION_EPOCH
    )

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)
    families = {row["family"]: row for row in payload["metricFamilies"]}

    assert families["taskLifecycle"]["freshness"] == TASK_OPERATION_EPOCH
    assert "taskLifecycle owner=" in text
    assert f"freshness={TASK_OPERATION_EPOCH} " in text


def test_task_lifecycle_freshness_degrades_when_the_log_is_unreachable(
    tmp_path, monkeypatch
):
    """An absent authority costs the column, never the whole diagnostics read."""
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    store.create_team(team_id=TEAM_ID, members=[AGENT_A])
    _record_identity(store)
    monkeypatch.setattr(task_config, "data_dir", lambda: tmp_path / "absent")

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)
    families = {row["family"]: row for row in payload["metricFamilies"]}

    assert families["taskLifecycle"]["freshness"] is None
    assert families["taskLifecycle"]["status"] == "canonical"
    assert "taskLifecycle owner=" in text


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
    closed_revision = store_remove_agent(store, unused.team_id, AGENT_A)

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
    set_state_backend(str(tmp_path / "managed-state"))
    try:
        result = run_serve_team_diagnostics(args)
        data = json.loads(capsys.readouterr().out)
    finally:
        task_config.set_backend(None)
        set_state_backend(None)

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


def test_serve_rebuild_projections_rebuilds_and_reports_the_new_build(tmp_path, capsys):
    backend = tmp_path / "task-backend"
    args = build_parser().parse_args(
        ["serve", "--task-backend", str(backend), "rebuild-projections"]
    )
    set_state_backend(str(tmp_path / "managed-state"))
    task_config.set_backend(str(backend))
    try:
        ServeTeamStore().record_agent_metric_delta(
            AGENT_A, tool_calls=1, message_timestamps=[1000.0]
        )
        before = _projection_row_counts()
    finally:
        task_config.set_backend(None)
        set_state_backend(None)

    set_state_backend(str(tmp_path / "managed-state"))
    try:
        result = args.func(args)
        text = capsys.readouterr().out
        after = _projection_row_counts()
    finally:
        task_config.set_backend(None)
        set_state_backend(None)

    assert result == EXIT_OK
    assert before["agent_metrics"] == 1
    assert after == dict.fromkeys(before, 0)
    assert "serve projections rebuilt agentActivity generation=2" in text
    assert "status=ready" in text
    assert AGENT_ACTIVITY.rebuild in text


def test_projection_failure_diagnostics_keep_the_stale_generation_and_recovery(
    tmp_path,
):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    store.record_agent_metric_delta(AGENT_A, tool_calls=1, message_timestamps=[1000.0])

    def fail(_stage):
        raise RuntimeError("rebuild fixture stopped")

    with pytest.raises(RuntimeError, match="rebuild fixture stopped"):
        rebuild_projection_family(store.projections, AGENT_ACTIVITY.name, fail)

    payload = team_diagnostics_payload(store=store)
    text = render_team_diagnostics(payload)
    projection = payload["projections"][0]
    activity = payload["metricFamilies"][0]

    assert projection["status"] == activity["status"] == PROJECTION_STATUS_STALE
    assert projection["servable"] is True
    assert "rebuild fixture stopped" in projection["detail"]
    assert projection["recoveryAction"] == AGENT_ACTIVITY.recovery_action
    # A bad rebuild is meant to be diagnosable without the design record, so
    # every registered answer travels beside the failure detail. Wiring is what
    # this proves: a dropped key or a crossed field, not the prose itself.
    assert projection["source"] == AGENT_ACTIVITY.source
    assert projection["cursor"] == AGENT_ACTIVITY.cursor
    assert projection["horizon"] == AGENT_ACTIVITY.horizon
    assert projection["rebuild"] == AGENT_ACTIVITY.rebuild
    assert projection["beyondHorizon"] == AGENT_ACTIVITY.beyond_horizon
    assert f"status={PROJECTION_STATUS_STALE}" in text
    assert f"recovery={AGENT_ACTIVITY.recovery_action}" in text


def test_incompatible_projection_diagnostics_report_explicit_unavailability(
    tmp_path,
):
    store = ServeTeamStore(path=tmp_path / TEAM_DATABASE_FILENAME)
    store.record_agent_metric_delta(AGENT_A, tool_calls=1, message_timestamps=[1000.0])
    with store.projections.connect() as projection:
        projection.execute(f"PRAGMA user_version = {PROJECTION_SCHEMA_VERSION + 1}")
    ServeProjectionStore._initialized_files.pop(store.projections.path, None)

    payload = team_diagnostics_payload(store=store)
    projection = payload["projections"][0]

    assert projection["status"] == PROJECTION_STATUS_INCOMPATIBLE
    assert projection["servable"] is False
    assert "schema version" in projection["detail"]
    assert projection["recoveryAction"] == AGENT_ACTIVITY.recovery_action


def _projection_row_counts() -> dict[str, int]:
    states = ServeTeamStore().projections.family_states()
    return {
        table: count for state in states for table, count in state.row_counts.items()
    }
