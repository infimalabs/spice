from spice.serve.teams import ServeTeamStore, TeamCommandService


def test_empty_team_snapshot_creates_initial_empty_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")

    snapshot = store.team_snapshot()
    followup = store.team_snapshot()

    assert snapshot.global_revision == 1
    assert len(snapshot.teams) == 1
    team = snapshot.teams[0]
    assert team.status == "open"
    assert team.members == ()
    assert team.revision == snapshot.global_revision
    assert followup.global_revision == snapshot.global_revision
    assert [followup_team.team_id for followup_team in followup.teams] == [team.team_id]


def test_lane_metrics_aggregate_removed_lifetime_team_members(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a"])
    store.record_agent_metric_delta("agent-a", acked=1, sends=2, tool_calls=3)
    store.assign_agent(team.team_id, "agent-b")
    store.record_agent_metric_delta("agent-b", acked=4, sends=5, tool_calls=6)
    store.remove_agent(team.team_id, "agent-a")

    summary = store.lane_metric_summary("agent-b", bucket_count=12)

    assert summary.agent_ids == ("agent-a", "agent-b")
    assert summary.acked == 5
    assert summary.sends == 7
    assert summary.tool_calls == 9


def test_lane_metrics_do_not_pull_prior_team_counts_after_agent_moves(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a"])
    destination = store.create_team(members=["agent-b"])
    store.record_agent_metric_delta("agent-a", acked=10, sends=10, tool_calls=10)

    store.assign_agent(destination.team_id, "agent-a")
    store.record_agent_metric_delta("agent-a", acked=1, sends=2, tool_calls=3)

    destination_summary = store.lane_metric_summary("agent-b", bucket_count=12)
    moved_summary = store.lane_metric_summary("agent-a", bucket_count=12)

    assert store.team_state(source.team_id).status == "closed"
    assert destination_summary.acked == 1
    assert moved_summary.acked == 1
    assert moved_summary.sends == 2
    assert moved_summary.tool_calls == 3


def test_assigning_agent_to_new_team_moves_single_open_membership(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a"])
    destination = store.create_team(members=["agent-b"])

    store.assign_agent(destination.team_id, "agent-a")

    open_members = {
        team.team_id: {member.agent_id for member in team.members}
        for team in store.team_snapshot().teams
    }
    assert open_members == {destination.team_id: {"agent-a", "agent-b"}}
    assert store.current_team_for_agent("agent-a") == destination.team_id
    assert store.team_state(source.team_id).status == "closed"


def test_assigning_agent_with_target_alias_retires_stale_membership(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["target-a"])
    destination = store.create_team(members=["agent-b"])

    store.assign_agent(destination.team_id, "thread-a", aliases=["target-a"])

    open_members = {
        team.team_id: {member.agent_id for member in team.members}
        for team in store.team_snapshot().teams
    }
    assert open_members == {destination.team_id: {"thread-a", "agent-b"}}
    assert store.current_team_for_agent("thread-a") == destination.team_id
    assert store.team_state(source.team_id).status == "closed"


def test_team_command_service_imports_agent_into_empty_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    source = store.create_team(members=["target-a"])
    empty = store.create_team()

    result = service.apply(
        {
            "command": "moveAgentToTeam",
            "teamId": empty.team_id,
            "agentId": "thread-a",
            "agentAliases": ["target-a"],
        }
    )

    open_members = {
        team.team_id: {member.agent_id for member in team.members}
        for team in result.snapshot.teams
    }
    assert open_members == {empty.team_id: {"thread-a"}}
    assert store.current_team_for_agent("thread-a") == empty.team_id
    assert store.team_state(source.team_id).status == "closed"


def test_team_command_service_reorders_team_agents(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply(
        {
            "command": "createTeam",
            "members": ["agent-a", "agent-b", "agent-c"],
        }
    )
    team = created.snapshot.teams[0]

    result = service.apply(
        {
            "command": "reorderTeamAgents",
            "teamId": team.team_id,
            "agentIds": ["agent-c", "agent-a", "agent-b"],
            "expectedRevision": created.revision,
        }
    )

    state = store.team_state(team.team_id)
    assert result.revision > created.revision
    assert [member.agent_id for member in state.members] == [
        "agent-c",
        "agent-a",
        "agent-b",
    ]


def test_team_command_service_toggles_agent_renewal_intent(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply({"command": "createTeam", "members": ["agent-a"]})

    enabled = service.apply(
        {
            "command": "setAgentRenewalIntent",
            "agentId": "agent-a",
            "requested": True,
            "expectedRevision": created.revision,
        }
    )
    enabled_member = enabled.snapshot.teams[0].to_payload()["members"][0]

    assert store.agent_renewal_requested("agent-a") is True
    assert enabled_member["renewalIntent"]["agentId"] == "agent-a"
    assert enabled_member["renewalIntent"]["requested"] is True
    assert enabled_member["renewalIntent"]["state"] == "requested"

    disabled = service.apply(
        {
            "command": "setAgentRenewalIntent",
            "agentId": "agent-a",
            "requested": False,
            "expectedRevision": enabled.revision,
        }
    )
    disabled_member = disabled.snapshot.teams[0].to_payload()["members"][0]

    assert store.renewal_state_for_agent("agent-a") is None
    assert disabled_member["renewalIntent"]["requested"] is False
    assert disabled_member["renewalIntent"]["state"] == ""


def test_removing_final_agent_closes_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a"])

    revision = store.remove_agent(team.team_id, "agent-a")
    snapshot = store.team_snapshot()

    assert store.team_state(team.team_id).status == "closed"
    assert snapshot.global_revision == revision
    assert len(snapshot.teams) == 1
    replacement = snapshot.teams[0]
    assert replacement.team_id != team.team_id
    assert replacement.status == "open"
    assert replacement.members == ()


def test_team_command_service_close_final_team_returns_replacement_empty_team(
    tmp_path,
):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply({"command": "createTeam", "members": ["agent-a"]})
    team = created.snapshot.teams[0]

    result = service.apply({"command": "closeTeam", "teamId": team.team_id})

    assert store.team_state(team.team_id).status == "closed"
    assert result.revision == result.snapshot.global_revision
    assert result.revision > created.revision
    assert len(result.snapshot.teams) == 1
    replacement = result.snapshot.teams[0]
    assert replacement.team_id != team.team_id
    assert replacement.status == "open"
    assert replacement.members == ()


def test_team_command_service_keeps_revisioned_config_history(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply(
        {
            "command": "createTeam",
            "members": ["agent-a"],
            "config": {"lifetime": "Steer", "selectedView": "compose"},
        }
    )
    team = created.snapshot.teams[0]

    first_update = service.apply(
        {
            "command": "updateTeamConfig",
            "teamId": team.team_id,
            "configPatch": {"lifetime": "Drive"},
            "expectedRevision": created.revision,
        }
    )
    stale_but_valid = service.apply(
        {
            "command": "updateTeamConfig",
            "teamId": team.team_id,
            "configPatch": {"selectedView": "metrics"},
            "expectedRevision": created.revision,
        }
    )
    state = store.team_state(team.team_id)

    assert first_update.revision > created.revision
    assert stale_but_valid.revision > first_update.revision
    assert state.config_revision == 2
    assert state.config.lifetime == "Drive"
    assert state.config.selected_view == "metrics"
