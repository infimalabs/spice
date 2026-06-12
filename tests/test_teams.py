from spice.serve.teams import ServeTeamStore, TeamCommandService


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


def test_removing_final_agent_closes_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a"])

    store.remove_agent(team.team_id, "agent-a")

    assert store.team_state(team.team_id).status == "closed"


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
