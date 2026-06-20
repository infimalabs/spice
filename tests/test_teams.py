from concurrent.futures import ThreadPoolExecutor

import pytest

from spice.errors import SpiceError
from spice.serve.teams import (
    TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL,
    TEAM_SQLITE_BUSY_TIMEOUT_MS,
    ServeTeamStore,
    TeamCommandService,
    TeamConfig,
)

COMPOSER_MOVE_SOURCE_ACKED_TOTAL = 11
COMPOSER_MOVE_SOURCE_SEND_TOTAL = 22
COMPOSER_MOVE_SOURCE_TOOL_CALL_TOTAL = 33
TEAM_MERGE_ACKED_TOTAL = 14
TEAM_MERGE_SEND_TOTAL = 25
TEAM_MERGE_TOOL_CALL_TOTAL = 36
RESTORED_SUBGROUP_ACKED_TOTAL = 18
RESTORED_SUBGROUP_SEND_TOTAL = 30
RESTORED_SUBGROUP_TOOL_CALL_TOTAL = 42


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


def test_composer_move_leaves_source_and_destination_metrics_unchanged(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a", "agent-c"])
    destination = store.create_team(members=["agent-b"])
    store.record_agent_metric_delta("agent-a", acked=10, sends=20, tool_calls=30)
    store.record_agent_metric_delta("agent-c", acked=1, sends=2, tool_calls=3)
    store.record_agent_metric_delta("agent-b", acked=4, sends=5, tool_calls=6)

    source_before = store.lane_metric_summary("agent-c", bucket_count=12)
    destination_before = store.lane_metric_summary("agent-b", bucket_count=12)

    store.assign_agent(destination.team_id, "agent-a")

    source_after = store.lane_metric_summary("agent-c", bucket_count=12)
    destination_after = store.lane_metric_summary("agent-b", bucket_count=12)

    assert store.team_state(source.team_id).status == "open"
    assert source_after.agent_ids == ("agent-a", "agent-c")
    assert source_after.acked == source_before.acked == COMPOSER_MOVE_SOURCE_ACKED_TOTAL
    assert source_after.sends == source_before.sends == COMPOSER_MOVE_SOURCE_SEND_TOTAL
    assert (
        source_after.tool_calls
        == source_before.tool_calls
        == COMPOSER_MOVE_SOURCE_TOOL_CALL_TOTAL
    )
    assert destination_after.agent_ids == ("agent-a", "agent-b")
    assert destination_after.acked == destination_before.acked == 4
    assert destination_after.sends == destination_before.sends == 5
    assert destination_after.tool_calls == destination_before.tool_calls == 6


def test_lane_merge_moves_source_metrics_into_destination_once(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a"])
    destination = store.create_team(members=["agent-b"])
    store.record_agent_metric_delta(
        "agent-a",
        acked=10,
        sends=20,
        tool_calls=30,
        message_timestamps=[120, 180],
    )
    store.record_agent_metric_delta(
        "agent-b",
        acked=4,
        sends=5,
        tool_calls=6,
        message_timestamps=[180],
    )

    store.merge_teams(source.team_id, destination.team_id)
    destination_after = store.lane_metric_summary("agent-b", bucket_count=12, now=180)
    moved_after = store.lane_metric_summary("agent-a", bucket_count=12, now=180)
    store.merge_teams(source.team_id, destination.team_id)
    repeated_after = store.lane_metric_summary("agent-b", bucket_count=12, now=180)

    assert store.team_state(source.team_id).status == "closed"
    assert destination_after.agent_ids == ("agent-a", "agent-b")
    assert destination_after.acked == TEAM_MERGE_ACKED_TOTAL
    assert destination_after.sends == TEAM_MERGE_SEND_TOTAL
    assert destination_after.tool_calls == TEAM_MERGE_TOOL_CALL_TOTAL
    assert sum(destination_after.sparkline) == 3
    assert moved_after == destination_after
    assert repeated_after == destination_after
    with store.connect() as connection:
        source_metric_rows = connection.execute(
            "SELECT COUNT(*) FROM team_agent_metrics WHERE team_id = ?",
            (source.team_id,),
        ).fetchone()[0]
        source_bucket_rows = connection.execute(
            "SELECT COUNT(*) FROM team_agent_metric_buckets WHERE team_id = ?",
            (source.team_id,),
        ).fetchone()[0]
    assert source_metric_rows == 0
    assert source_bucket_rows == 0


def test_split_team_back_restores_latest_merged_source_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a", "agent-b", "agent-c", "agent-d"])
    destination = store.create_team(members=["agent-e"])

    store.merge_teams(source.team_id, destination.team_id)
    merged = store.team_state(destination.team_id)
    restored = store.split_team_back(destination.team_id)

    open_members = {
        team.team_id: [member.agent_id for member in team.members]
        for team in store.team_snapshot().teams
    }
    assert merged.split_back_available is True
    assert merged.split_back_member_count == 4
    assert restored.team_id == source.team_id
    assert open_members == {
        source.team_id: ["agent-a", "agent-b", "agent-c", "agent-d"],
        destination.team_id: ["agent-e"],
    }


def test_split_team_back_moves_subgroup_metrics_back_to_restored_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a", "agent-b"])
    destination = store.create_team(members=["agent-c"])
    store.record_agent_metric_delta("agent-a", acked=10, sends=20, tool_calls=30)
    store.record_agent_metric_delta("agent-b", acked=1, sends=2, tool_calls=3)
    store.record_agent_metric_delta("agent-c", acked=4, sends=5, tool_calls=6)

    store.merge_teams(source.team_id, destination.team_id)
    store.record_agent_metric_delta("agent-a", acked=7, sends=8, tool_calls=9)
    store.split_team_back(destination.team_id)
    restored_summary = store.lane_metric_summary("agent-a", bucket_count=12)
    destination_summary = store.lane_metric_summary("agent-c", bucket_count=12)

    assert restored_summary.agent_ids == ("agent-a", "agent-b")
    assert restored_summary.acked == RESTORED_SUBGROUP_ACKED_TOTAL
    assert restored_summary.sends == RESTORED_SUBGROUP_SEND_TOTAL
    assert restored_summary.tool_calls == RESTORED_SUBGROUP_TOOL_CALL_TOTAL
    assert destination_summary.agent_ids == ("agent-c",)
    assert destination_summary.acked == 4
    assert destination_summary.sends == 5
    assert destination_summary.tool_calls == 6


def test_split_team_back_unwinds_nested_team_merges_one_boundary_at_a_time(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    inner = store.create_team(members=["agent-a", "agent-b"])
    middle = store.create_team(members=["agent-c", "agent-d"])
    outer = store.create_team(members=["agent-e"])

    store.merge_teams(inner.team_id, middle.team_id)
    store.merge_teams(middle.team_id, outer.team_id)
    restored_middle = store.split_team_back(outer.team_id)
    restored_inner = store.split_team_back(middle.team_id)

    open_members = {
        team.team_id: [member.agent_id for member in team.members]
        for team in store.team_snapshot().teams
    }
    assert restored_middle.team_id == middle.team_id
    assert restored_inner.team_id == inner.team_id
    assert open_members == {
        inner.team_id: ["agent-a", "agent-b"],
        middle.team_id: ["agent-c", "agent-d"],
        outer.team_id: ["agent-e"],
    }


def test_assigning_agent_to_new_team_moves_single_open_membership(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["agent-a"])
    destination = store.create_team(members=["agent-b"])

    store.assign_agent(destination.team_id, "agent-a")

    open_members = {
        team.team_id: {member.agent_id for member in team.members}
        for team in store.team_snapshot().teams
    }
    with store.connect() as connection:
        team_rows = connection.execute(
            "SELECT team_id, status FROM teams ORDER BY created_at"
        ).fetchall()

    assert open_members == {destination.team_id: {"agent-a", "agent-b"}}
    assert store.current_team_for_agent("agent-a") == destination.team_id
    assert [(row["team_id"], row["status"]) for row in team_rows] == [
        (destination.team_id, "open")
    ]


def test_assigning_agent_with_target_alias_retires_stale_membership(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["target-a"])
    destination = store.create_team(members=["agent-b"])

    store.assign_agent(destination.team_id, "thread-a", aliases=["target-a"])

    open_members = {
        team.team_id: {member.agent_id for member in team.members}
        for team in store.team_snapshot().teams
    }
    with store.connect() as connection:
        team_rows = connection.execute(
            "SELECT team_id, status FROM teams ORDER BY created_at"
        ).fetchall()

    assert open_members == {destination.team_id: {"thread-a", "agent-b"}}
    assert store.current_team_for_agent("thread-a") == destination.team_id
    assert [(row["team_id"], row["status"]) for row in team_rows] == [
        (destination.team_id, "open")
    ]


def test_assigning_agent_with_same_team_alias_preserves_roster_slot(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a", "agent-b", "agent-c"])

    store.assign_agent(team.team_id, "agent-b-renewed", aliases=["agent-b"])

    state = store.team_state(team.team_id)
    assert [member.agent_id for member in state.members] == [
        "agent-a",
        "agent-b-renewed",
        "agent-c",
    ]
    assert store.current_team_for_agent("agent-b") is None
    assert store.current_team_for_agent("agent-b-renewed") == team.team_id


def test_team_command_service_imports_agent_into_empty_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    store.create_team(members=["target-a"])
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
    with store.connect() as connection:
        team_rows = connection.execute(
            "SELECT team_id, status FROM teams ORDER BY created_at"
        ).fetchall()

    assert open_members == {empty.team_id: {"thread-a"}}
    assert store.current_team_for_agent("thread-a") == empty.team_id
    assert [(row["team_id"], row["status"]) for row in team_rows] == [
        (empty.team_id, "open")
    ]


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
    assert store.agent_renewal_active("agent-a") is True
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
    assert store.agent_renewal_active("agent-a") is False
    assert disabled_member["renewalIntent"]["requested"] is False
    assert disabled_member["renewalIntent"]["state"] == ""


def test_pending_renewal_remains_active_until_successor_starts(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["agent-a"])

    store.record_pending_renewal(agent_id="agent-a", ancestor_thread_id="agent-a")

    assert store.agent_renewal_requested("agent-a") is False
    assert store.agent_renewal_active("agent-a") is True

    store.record_started_renewal(
        predecessor_agent_id="agent-a",
        successor_agent_id="agent-b",
        ancestor_thread_id="agent-a",
    )

    assert store.agent_renewal_active("agent-a") is False


def test_removing_final_agent_closes_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a"])

    revision = store.remove_agent(team.team_id, "agent-a")
    snapshot = store.team_snapshot()
    with store.connect() as connection:
        team_rows = connection.execute(
            "SELECT team_id, status FROM teams ORDER BY created_at"
        ).fetchall()
        event_rows = connection.execute(
            "SELECT kind FROM events ORDER BY revision"
        ).fetchall()

    assert snapshot.global_revision > revision
    assert len(snapshot.teams) == 1
    replacement = snapshot.teams[0]
    assert replacement.team_id != team.team_id
    assert replacement.status == "open"
    assert replacement.members == ()
    assert [(row["team_id"], row["status"]) for row in team_rows] == [
        (replacement.team_id, "open")
    ]
    assert [row["kind"] for row in event_rows] == [
        "createTeam",
        "pruneZeroActivityTeams",
    ]


def test_team_command_service_close_final_team_returns_replacement_empty_team(
    tmp_path,
):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply({"command": "createTeam", "members": ["agent-a"]})
    team = created.snapshot.teams[0]

    result = service.apply({"command": "closeTeam", "teamId": team.team_id})
    with store.connect() as connection:
        team_rows = connection.execute(
            "SELECT team_id, status FROM teams ORDER BY created_at"
        ).fetchall()
        event_rows = connection.execute(
            "SELECT kind FROM events ORDER BY revision"
        ).fetchall()

    assert result.revision == result.snapshot.global_revision
    assert result.revision > created.revision
    assert len(result.snapshot.teams) == 1
    replacement = result.snapshot.teams[0]
    assert replacement.team_id != team.team_id
    assert replacement.status == "open"
    assert replacement.members == ()
    assert [(row["team_id"], row["status"]) for row in team_rows] == [
        (replacement.team_id, "open")
    ]
    assert [row["kind"] for row in event_rows] == [
        "createTeam",
        "pruneZeroActivityTeams",
    ]


def test_zero_activity_prune_preserves_metric_and_config_teams(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    metric_team = store.create_team(team_id="team-metric", members=["agent-a"])
    store.record_agent_metric_delta("agent-a", sends=1)
    store.remove_agent(metric_team.team_id, "agent-a")
    config_team = store.create_team(
        team_id="team-config",
        members=["agent-b"],
        config=TeamConfig(task_filters=("serve.ui",)),
    )
    store.remove_agent(config_team.team_id, "agent-b")

    snapshot = store.team_snapshot()
    with store.connect() as connection:
        team_rows = connection.execute(
            "SELECT team_id, status FROM teams ORDER BY team_id"
        ).fetchall()

    assert {row["team_id"]: row["status"] for row in team_rows} == {
        "team-config": "closed",
        "team-metric": "closed",
        snapshot.teams[0].team_id: "open",
    }


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


def test_team_task_filter_api_tracks_sources_and_projection(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(
        members=["agent-a"], config=TeamConfig(task_filters=("serve.ui",))
    )

    initial = store.team_config(team.team_id)
    assert initial.task_filters == ("serve.ui",)
    assert [entry.to_payload() for entry in initial.task_filter_entries] == [
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_MANUAL}
    ]

    initial_revision = store.global_revision()
    duplicate = store.add_task_filter(
        team.team_id, "serve.ui", source=TASK_FILTER_SOURCE_MANUAL
    )

    assert duplicate == initial_revision
    assert store.global_revision() == initial_revision

    added_auto = store.add_task_filter(
        team.team_id, "serve.ui", source=TASK_FILTER_SOURCE_AUTO_CREATE
    )
    with_auto = store.team_config(team.team_id)

    assert added_auto > initial_revision
    assert with_auto.task_filters == ("serve.ui",)
    assert [entry.to_payload() for entry in with_auto.task_filter_entries] == [
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_MANUAL},
    ]

    removed_auto = store.remove_task_filter(
        team.team_id, "serve.ui", source=TASK_FILTER_SOURCE_AUTO_CREATE
    )
    manual_only = store.team_config(team.team_id)
    duplicate_remove = store.remove_task_filter(
        team.team_id, "serve.ui", source=TASK_FILTER_SOURCE_AUTO_CREATE
    )

    assert removed_auto > added_auto
    assert duplicate_remove == removed_auto
    assert manual_only.task_filters == ("serve.ui",)
    assert [entry.to_payload() for entry in manual_only.task_filter_entries] == [
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_MANUAL}
    ]

    store.remove_task_filter(team.team_id, "serve.ui")
    empty = store.team_config(team.team_id)

    assert empty.task_filters == ()
    assert empty.task_filter_entries == ()


def test_team_task_filter_api_preserves_concurrent_distinct_adds(tmp_path):
    path = tmp_path / "teams.sqlite3"
    store = ServeTeamStore(path=path)
    team = store.create_team(members=["agent-a"])

    def add(project: str) -> int:
        return ServeTeamStore(path=path).add_task_filter(
            team.team_id, project, source=TASK_FILTER_SOURCE_AUTO_CREATE
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        revisions = list(executor.map(add, ("serve.ui", "task.review")))

    config = store.team_config(team.team_id)

    assert len(set(revisions)) == 2
    assert config.task_filters == ("serve.ui", "task.review")
    assert [entry.to_payload() for entry in config.task_filter_entries] == [
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
        {"project": "task.review", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
    ]


def test_team_task_filter_api_validates_project_and_source(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a"])

    with pytest.raises(SpiceError, match="internal"):
        store.add_task_filter(team.team_id, "agent.private")
    with pytest.raises(SpiceError, match="task filter source"):
        store.add_task_filter(team.team_id, "serve.ui", source="automatic")


def test_team_store_connect_enables_wal_and_busy_timeout(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")

    with store.connect() as connection:
        journal_mode = connection.execute("PRAGMA journal_mode").fetchone()[0]
        busy_timeout = connection.execute("PRAGMA busy_timeout").fetchone()[0]

    assert str(journal_mode).lower() == "wal"
    assert int(busy_timeout) == TEAM_SQLITE_BUSY_TIMEOUT_MS


def test_team_command_service_replaces_membership_without_rewriting_sources(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply(
        {
            "command": "createTeam",
            "members": ["agent-a"],
            "config": {"taskFilters": ["serve.ui"]},
        }
    )
    team = created.snapshot.teams[0]
    store.add_task_filter(
        team.team_id, "task.review", source=TASK_FILTER_SOURCE_AUTO_CLAIM
    )

    service.apply(
        {
            "command": "updateTeamConfig",
            "teamId": team.team_id,
            "configPatch": {"lifetime": "Steer"},
        }
    )
    lifetime_only = store.team_config(team.team_id)

    assert lifetime_only.lifetime == "Steer"
    assert lifetime_only.task_filters == ("serve.ui", "task.review")

    service.apply(
        {
            "command": "updateTeamConfig",
            "teamId": team.team_id,
            "configPatch": {"taskFilters": ["task.review", "task.extra"]},
        }
    )
    replaced = store.team_config(team.team_id)

    assert replaced.task_filters == ("task.extra", "task.review")
    assert [entry.to_payload() for entry in replaced.task_filter_entries] == [
        {"project": "task.extra", "source": TASK_FILTER_SOURCE_MANUAL},
        {"project": "task.review", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
    ]


def test_team_config_replace_preserves_existing_filter_sources(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a"])
    store.add_task_filter(
        team.team_id, "serve.ui", source=TASK_FILTER_SOURCE_AUTO_CREATE
    )
    store.add_task_filter(
        team.team_id, "task.review", source=TASK_FILTER_SOURCE_AUTO_CLAIM
    )
    current = store.team_config(team.team_id)

    store.update_team_config(
        team.team_id,
        TeamConfig(
            lifetime=current.lifetime,
            speech_mode=current.speech_mode,
            task_filters=("serve.ui", "task.extra"),
            selected_view=current.selected_view,
            shell_settings=current.shell_settings,
        ),
        replace_task_filters=True,
    )
    replaced = store.team_config(team.team_id)

    assert replaced.task_filters == ("serve.ui", "task.extra")
    assert [entry.to_payload() for entry in replaced.task_filter_entries] == [
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
        {"project": "task.extra", "source": TASK_FILTER_SOURCE_MANUAL},
    ]
