from concurrent.futures import ThreadPoolExecutor

import pytest

from spice.errors import SpiceError
from spice.tasks import config as task_config
from spice.serve.team.store import (
    TASK_FILTER_SOURCE_AUTO_CLAIM,
    TASK_FILTER_SOURCE_AUTO_CREATE,
    TASK_FILTER_SOURCE_MANUAL,
    TEAM_SQLITE_BUSY_TIMEOUT_MS,
    ServeTeamStore,
    TeamCommandService,
    TeamConfig,
)

IDENTITY_RENEWAL_REVISION = 42


def _record_identity(
    store: ServeTeamStore,
    actor_id: str,
    *,
    target_id: str = "wt-a",
    thread_id: str = "",
    actual_model: str = "actual-model",
    actual_effort: str = "low",
    desired_model: str = "desired-model",
    desired_effort: str = "high",
) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id=target_id,
        thread_id=thread_id or actor_id.removeprefix("thread:"),
        actual_driver="codex",
        actual_model=actual_model,
        actual_effort=actual_effort,
        actual_service_tier="default",
        desired_driver="codex",
        desired_model=desired_model,
        desired_effort=desired_effort,
        transcript_owner="codex",
    )


def test_team_event_wakes_task_event_file_after_commit(tmp_path):
    task_config.set_backend(str(tmp_path / "task-backend"))
    try:
        event_path = task_config.ensure_task_event_file()
        before = event_path.read_text(encoding="utf-8")
        store = ServeTeamStore(path=tmp_path / "teams.sqlite3")

        with store.connect() as connection:
            store._create_team_locked(
                connection, "team-display-event", TeamConfig(), ["agent-a"]
            )
            assert event_path.read_text(encoding="utf-8") == before

        after = event_path.read_text(encoding="utf-8")
        assert after != before
        assert after.endswith(" team\n")
    finally:
        task_config.set_backend(None)


def test_team_metric_write_does_not_wake_task_event_file(tmp_path):
    task_config.set_backend(str(tmp_path / "task-backend"))
    try:
        event_path = task_config.ensure_task_event_file()
        store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
        store.create_team(team_id="team-metric", members=["agent-a"])
        after_create = event_path.read_text(encoding="utf-8")

        store.record_agent_metric_delta("agent-a", tool_calls=1)

        assert event_path.read_text(encoding="utf-8") == after_create
    finally:
        task_config.set_backend(None)


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
    store.create_team(members=["target:target-a"])
    destination = store.create_team(members=["thread:agent-b"])

    store.assign_agent(
        destination.team_id,
        "thread:thread-a",
        aliases=["target:target-a"],
    )

    open_members = {
        team.team_id: {member.agent_id for member in team.members}
        for team in store.team_snapshot().teams
    }
    with store.connect() as connection:
        team_rows = connection.execute(
            "SELECT team_id, status FROM teams ORDER BY created_at"
        ).fetchall()

    assert open_members == {destination.team_id: {"thread:thread-a", "thread:agent-b"}}
    assert store.current_team_for_agent("thread:thread-a") == destination.team_id
    assert [(row["team_id"], row["status"]) for row in team_rows] == [
        (destination.team_id, "open")
    ]


def test_assigning_agent_with_same_team_alias_preserves_roster_slot(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(
        members=["thread:agent-a", "thread:agent-b", "thread:agent-c"]
    )

    store.assign_agent(
        team.team_id, "thread:agent-b-renewed", aliases=["thread:agent-b"]
    )

    state = store.team_state(team.team_id)
    assert [member.agent_id for member in state.members] == [
        "thread:agent-a",
        "thread:agent-b-renewed",
        "thread:agent-c",
    ]
    assert store.current_team_for_agent("thread:agent-b") is None
    assert store.current_team_for_agent("thread:agent-b-renewed") == team.team_id


def test_team_command_service_imports_agent_into_empty_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    store.create_team(members=["target:target-a"])
    empty = store.create_team()

    result = service.apply(
        {
            "command": "moveAgentToTeam",
            "teamId": empty.team_id,
            "agentId": "thread:thread-a",
            "agentAliases": ["target:target-a"],
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

    assert open_members == {empty.team_id: {"thread:thread-a"}}
    assert store.current_team_for_agent("thread:thread-a") == empty.team_id
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
    with store.connect() as connection:
        joined_before = {
            row["agent_id"]: row["joined_at"]
            for row in connection.execute(
                "SELECT agent_id, joined_at FROM memberships WHERE team_id = ?",
                (team.team_id,),
            )
        }

    result = service.apply(
        {
            "command": "reorderTeamAgents",
            "teamId": team.team_id,
            "agentIds": ["agent-c", "agent-a", "agent-b"],
            "expectedRevision": created.revision,
        }
    )

    state = store.team_state(team.team_id)
    with store.connect() as connection:
        membership_rows = connection.execute(
            "SELECT agent_id, joined_at, position FROM memberships "
            "WHERE team_id = ? ORDER BY position",
            (team.team_id,),
        ).fetchall()

    assert result.revision > created.revision
    assert [member.agent_id for member in state.members] == [
        "agent-c",
        "agent-a",
        "agent-b",
    ]
    assert {row["agent_id"]: row["joined_at"] for row in membership_rows} == (
        joined_before
    )
    assert [(row["agent_id"], row["position"]) for row in membership_rows] == [
        ("agent-c", 0),
        ("agent-a", 1),
        ("agent-b", 2),
    ]


def test_team_command_service_toggles_agent_renewal_intent(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply({"command": "createTeam", "members": ["thread:agent-a"]})
    _record_identity(store, "thread:agent-a", thread_id="agent-a")

    enabled = service.apply(
        {
            "command": "setAgentRenewalIntent",
            "agentId": "thread:agent-a",
            "requested": True,
            "expectedRevision": created.revision,
        }
    )
    enabled_member = enabled.snapshot.teams[0].to_payload()["members"][0]

    assert store.agent_renewal_requested("thread:agent-a") is True
    assert store.agent_renewal_active("thread:agent-a") is True
    assert enabled_member["renewalIntent"]["agentId"] == "thread:agent-a"
    assert enabled_member["renewalIntent"]["requested"] is True
    assert enabled_member["renewalIntent"]["state"] == "requested"
    assert enabled_member["renewalIntent"]["teamSlot"] == 0
    assert enabled_member["renewalIntent"]["predecessorIdentity"]["threadId"] == (
        "agent-a"
    )
    assert enabled_member["renewalIntent"]["successorIdentity"]["desiredModel"] == (
        "desired-model"
    )

    disabled = service.apply(
        {
            "command": "setAgentRenewalIntent",
            "agentId": "thread:agent-a",
            "requested": False,
            "expectedRevision": enabled.revision,
        }
    )
    disabled_member = disabled.snapshot.teams[0].to_payload()["members"][0]

    assert store.renewal_state_for_agent("thread:agent-a") is None
    assert store.agent_renewal_active("thread:agent-a") is False
    assert disabled_member["renewalIntent"]["requested"] is False
    assert disabled_member["renewalIntent"]["state"] == ""


def test_pending_renewal_remains_active_until_successor_starts(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    _record_identity(store, "thread:agent-a", thread_id="agent-a")

    store.record_pending_renewal(
        agent_id="thread:agent-a", ancestor_thread_id="agent-a"
    )

    assert store.agent_renewal_requested("thread:agent-a") is False
    assert store.agent_renewal_active("thread:agent-a") is True

    store.record_started_renewal(
        predecessor_agent_id="thread:agent-a",
        successor_agent_id="thread:agent-b",
        ancestor_thread_id="agent-a",
    )

    assert store.agent_renewal_active("thread:agent-a") is False


def test_started_renewal_preserves_predecessor_roster_slot(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(
        members=["thread:agent-a", "thread:agent-b", "thread:agent-c"]
    )
    _record_identity(store, "thread:agent-b", target_id="wt-b", thread_id="agent-b")
    store.record_pending_renewal(
        agent_id="thread:agent-b", ancestor_thread_id="agent-b"
    )

    store.record_started_renewal(
        predecessor_agent_id="thread:agent-b",
        successor_agent_id="thread:agent-b-renewed",
        ancestor_thread_id="agent-b",
    )

    state = store.team_state(team.team_id)
    assert [member.agent_id for member in state.members] == [
        "thread:agent-a",
        "thread:agent-b-renewed",
        "thread:agent-c",
    ]
    assert store.current_team_for_agent("thread:agent-b") is None
    assert store.current_team_for_agent("thread:agent-b-renewed") == team.team_id
    renewal = store.renewal_state_for_agent("thread:agent-b")
    assert renewal is not None
    assert renewal.successor_agent_id == "thread:agent-b-renewed"
    assert renewal.successor_thread_id == "agent-b-renewed"
    assert renewal.team_slot == 1
    assert renewal.predecessor_identity["actorId"] == "thread:agent-b"
    assert renewal.predecessor_identity["actualModel"] == "actual-model"
    assert renewal.successor_identity["actorId"] == "thread:agent-b-renewed"
    assert renewal.successor_identity["targetId"] == "wt-b"
    assert renewal.successor_identity["threadId"] == "agent-b-renewed"


def test_reorder_then_renew_preserves_successor_visible_slot(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(
        members=["thread:agent-a", "thread:agent-b", "thread:agent-c"]
    )
    store.reorder_team_agents(
        team.team_id,
        ["thread:agent-c", "thread:agent-a", "thread:agent-b"],
    )
    _record_identity(store, "thread:agent-b", target_id="wt-b", thread_id="agent-b")
    store.record_pending_renewal(
        agent_id="thread:agent-b", ancestor_thread_id="agent-b"
    )

    store.record_started_renewal(
        predecessor_agent_id="thread:agent-b",
        successor_agent_id="thread:agent-b-renewed",
        ancestor_thread_id="agent-b",
    )

    state = store.team_state(team.team_id)
    assert [member.agent_id for member in state.members] == [
        "thread:agent-c",
        "thread:agent-a",
        "thread:agent-b-renewed",
    ]


def test_renew_then_reorder_moves_successor_by_position(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(
        members=["thread:agent-a", "thread:agent-b", "thread:agent-c"]
    )
    _record_identity(store, "thread:agent-b", target_id="wt-b", thread_id="agent-b")
    store.record_pending_renewal(
        agent_id="thread:agent-b", ancestor_thread_id="agent-b"
    )
    store.record_started_renewal(
        predecessor_agent_id="thread:agent-b",
        successor_agent_id="thread:agent-b-renewed",
        ancestor_thread_id="agent-b",
    )

    store.reorder_team_agents(
        team.team_id,
        ["thread:agent-c", "thread:agent-b-renewed", "thread:agent-a"],
    )

    state = store.team_state(team.team_id)
    assert [member.agent_id for member in state.members] == [
        "thread:agent-c",
        "thread:agent-b-renewed",
        "thread:agent-a",
    ]


def test_renewal_records_model_effort_change_for_successor_identity(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    _record_identity(
        store,
        "thread:agent-a",
        thread_id="agent-a",
        actual_model="old-model",
        actual_effort="low",
        desired_model="new-model",
        desired_effort="xhigh",
    )

    pending = store.record_pending_renewal(
        agent_id="thread:agent-a", ancestor_thread_id="agent-a"
    )
    started = store.record_started_renewal(
        predecessor_agent_id="thread:agent-a",
        successor_agent_id="thread:agent-b",
        ancestor_thread_id="agent-a",
    )

    assert pending.predecessor_identity["actualModel"] == "old-model"
    assert pending.successor_identity["desiredModel"] == "new-model"
    assert pending.successor_identity["desiredEffort"] == "xhigh"
    assert started.successor_thread_id == "agent-b"
    assert started.successor_identity["actorId"] == "thread:agent-b"
    assert started.successor_identity["threadId"] == "agent-b"
    assert started.successor_identity["desiredModel"] == "new-model"
    assert started.successor_identity["desiredEffort"] == "xhigh"


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
    with pytest.raises(SpiceError, match="stale team command"):
        service.apply(
            {
                "command": "updateTeamConfig",
                "teamId": team.team_id,
                "configPatch": {"selectedView": "metrics"},
                "expectedRevision": created.revision,
            }
        )
    state = store.team_state(team.team_id)

    assert first_update.revision > created.revision
    assert state.config_revision == 1
    assert state.config.lifetime == "Drive"
    assert state.config.selected_view == "compose"


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


def test_team_state_reads_explicit_identity_for_member(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["thread:agent-a"])

    store.record_agent_identity(
        actor_id="thread:agent-a",
        target_id="wt-a",
        thread_id="agent-a",
        actual_driver="codex",
        actual_model="gpt-5",
        actual_effort="high",
        desired_driver="codex",
        desired_model="gpt-5",
        desired_effort="high",
    )
    member = store.team_state(team.team_id).members[0]

    assert member.agent_id == "thread:agent-a"
    assert member.agent_facts["actorId"] == "thread:agent-a"
    assert member.agent_facts["targetId"] == "wt-a"
    assert member.agent_facts["threadId"] == "agent-a"


def test_team_store_records_repeated_agent_identity_updates(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["thread:agent-a"])

    first = store.record_agent_identity(
        actor_id="thread:agent-a",
        target_id="wt-a",
        thread_id="agent-a",
        actual_driver="codex",
        actual_model="gpt-5",
        actual_effort="high",
        actual_service_tier="default",
        desired_driver="codex",
        desired_model="gpt-5",
        desired_effort="high",
        transcript_owner="codex",
    )
    updated = store.record_agent_identity(
        actor_id="thread:agent-a",
        target_id="wt-a",
        thread_id="agent-a",
        actual_driver="claude",
        actual_model="claude-sonnet-4-6",
        actual_effort="medium",
        actual_service_tier="fast",
        desired_driver="codex",
        desired_model="gpt-5.5",
        desired_effort="xhigh",
        transcript_owner="claude",
        renewal_state="pending",
        renewal_ancestor_thread_id="agent-a",
        renewal_successor_thread_id="",
        renewal_revision=IDENTITY_RENEWAL_REVISION,
    )
    stored = store.agent_identity_for_actor("thread:agent-a")
    member = store.team_state(team.team_id).members[0]

    assert stored is not None
    assert stored == updated
    assert updated.updated_at >= first.updated_at
    assert stored.actual_driver == "claude"
    assert stored.actual_model == "claude-sonnet-4-6"
    assert stored.actual_service_tier == "fast"
    assert stored.desired_model == "gpt-5.5"
    assert stored.renewal_revision == IDENTITY_RENEWAL_REVISION
    assert stored.updated_at == updated.updated_at
    assert member.agent_facts["actorId"] == "thread:agent-a"
    assert member.agent_facts["actualDriver"] == "claude"
    assert member.agent_facts["desiredEffort"] == "xhigh"


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
