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
from tests.test_teamstorehelpers import (
    store_global_revision,
    store_merge_teams,
    store_remove_agent,
    store_reorder_team_agents,
    store_split_team_back,
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
        before_task_revision = task_config.task_event_revision()
        store = ServeTeamStore(path=tmp_path / "teams.sqlite3")

        with store.connect() as connection:
            store._create_team_locked(
                connection, "team-display-event", TeamConfig(), ["agent-a"]
            )
            assert event_path.read_text(encoding="utf-8") == before

        after = event_path.read_text(encoding="utf-8")
        assert after != before
        assert after.endswith(" team\n")
        assert task_config.task_event_revision() == before_task_revision
    finally:
        task_config.set_backend(None)


def test_reorder_does_not_wake_the_lane_watchers(tmp_path):
    # A composer reorder permutes member order only -- no lane's content
    # changes -- so it must NOT bump the watched task event file. Waking made
    # every swap re-push all members' messages and re-render the whole board.
    task_config.set_backend(str(tmp_path / "task-backend"))
    try:
        event_path = task_config.ensure_task_event_file()
        store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
        team = store.create_team(members=["thread:a", "thread:b", "thread:c"])

        after_create = event_path.read_text(encoding="utf-8")
        store_reorder_team_agents(
            store, team.team_id, ["thread:b", "thread:a", "thread:c"]
        )
        after_reorder = event_path.read_text(encoding="utf-8")
        assert after_reorder == after_create  # reorder did NOT wake the watchers

        # A membership change still wakes them (its stream genuinely changes).
        store_remove_agent(store, team.team_id, "thread:c")
        assert event_path.read_text(encoding="utf-8") != after_reorder

        state = store.team_state(team.team_id)
        assert [member.agent_id for member in state.members] == ["thread:b", "thread:a"]
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


def test_read_path_prune_does_not_wake_task_event_file(tmp_path):
    # team_snapshot() backs every teams.refresh poll and garbage-collects closed
    # zero-activity teams. A pruned team is already closed -- absent from the
    # open-team topology every client renders -- so the prune must NOT bump the
    # watched task event file: a plain read waking every visible lane into a
    # full payload re-push (transcript re-read + history) on invisible GC is an
    # unrelated-work stall on the topology path.
    task_config.set_backend(str(tmp_path / "task-backend"))
    try:
        event_path = task_config.ensure_task_event_file()
        store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
        keep = store.create_team(members=["agent-keep"])
        retire = store.create_team(members=["agent-retire"])

        # Close the retiring team with a raw locked close rather than a command
        # (whose response snapshot would prune in the same transaction), leaving
        # a closed zero-activity team for the next read to collect. `keep` stays
        # open so the snapshot's ensure-open path never fires and wakes.
        with store.connect() as connection:
            store._close_team_locked(connection, retire.team_id)
        before = event_path.read_text(encoding="utf-8")

        snapshot = store.team_snapshot()  # the pruning read

        assert event_path.read_text(encoding="utf-8") == before
        snapshot_team_ids = [team.team_id for team in snapshot.teams]
        with store.connect() as connection:
            persisted_team_ids = [
                str(row["team_id"])
                for row in connection.execute(
                    "SELECT team_id FROM teams ORDER BY created_at"
                ).fetchall()
            ]
        assert snapshot_team_ids == [keep.team_id]
        assert persisted_team_ids == [keep.team_id]
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

    store_merge_teams(store, source.team_id, destination.team_id)
    merged = store.team_state(destination.team_id)
    restored = store_split_team_back(store, destination.team_id)

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

    store_merge_teams(store, inner.team_id, middle.team_id)
    store_merge_teams(store, middle.team_id, outer.team_id)
    restored_middle = store_split_team_back(store, outer.team_id)
    restored_inner = store_split_team_back(store, middle.team_id)

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


def test_moving_an_agent_leaves_a_bystander_teams_roster_intact(tmp_path):
    # The client sends every id form its lane answers to, and one of those
    # forms can still hold a slot in a team this move is neither leaving nor
    # joining. That team is a bystander: it keeps every member it had, at the
    # positions it had them, and stays open.
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    bystander = store.create_team(members=["target:target-a", "thread:agent-keep"])
    destination = store.create_team(members=["thread:agent-c"])

    store.assign_agent(
        destination.team_id, "thread:agent-a", aliases=["target:target-a"]
    )

    open_members = {
        team.team_id: [member.agent_id for member in team.members]
        for team in store.team_snapshot().teams
    }
    assert open_members == {
        bystander.team_id: ["target:target-a", "thread:agent-keep"],
        destination.team_id: ["thread:agent-c", "thread:agent-a"],
    }
    assert store.current_team_for_agent("target:target-a") == bystander.team_id
    assert store.current_team_for_agent("thread:agent-a") == destination.team_id
    with store.connect() as connection:
        statuses = dict(
            connection.execute("SELECT team_id, status FROM teams").fetchall()
        )
    # The team the move emptied is the only one retired -- closed, then pruned
    # by the snapshot above for having carried no activity.
    assert statuses == {bystander.team_id: "open", destination.team_id: "open"}


def test_ui_move_between_multi_member_teams_preserves_server_topology(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    source = store.create_team(
        team_id="team-source",
        members=["target:moved", "thread:source-a", "thread:source-b"],
    )
    destination = store.create_team(
        team_id="team-destination",
        members=["thread:destination-a", "thread:destination-b"],
    )

    moved = service.apply(
        {
            "command": "moveAgentToTeam",
            "expectedRevision": destination.revision,
            "teamId": destination.team_id,
            "agentId": "thread:moved",
            "agentAliases": ["target:moved"],
        }
    )

    open_members = {
        team.team_id: [member.agent_id for member in team.members]
        for team in moved.snapshot.teams
    }
    assert moved.revision == 3
    assert open_members == {
        source.team_id: ["thread:source-a", "thread:source-b"],
        destination.team_id: [
            "thread:destination-a",
            "thread:destination-b",
            "thread:moved",
        ],
    }
    with store.connect() as connection:
        events = [
            (row["revision"], row["kind"], row["team_id"])
            for row in connection.execute(
                "SELECT revision, kind, team_id FROM events ORDER BY revision"
            )
        ]
    assert events == [
        (1, "createTeam", source.team_id),
        (2, "createTeam", destination.team_id),
        (3, "assignAgent", destination.team_id),
    ]


def test_ui_create_team_from_member_preserves_other_server_groups(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    source = store.create_team(
        team_id="team-source",
        members=["target:moved", "thread:source-a", "thread:source-b"],
    )
    bystander = store.create_team(
        team_id="team-bystander",
        members=["thread:bystander-a", "thread:bystander-b"],
    )

    created = service.apply(
        {
            "command": "createTeam",
            "expectedRevision": bystander.revision,
            "members": ["thread:moved"],
            "agentAliases": ["target:moved"],
            "config": {},
        }
    )

    open_members = {
        team.team_id: [member.agent_id for member in team.members]
        for team in created.snapshot.teams
    }
    existing_team_ids = {source.team_id, bystander.team_id}
    new_team = next(
        team for team in created.snapshot.teams if team.team_id not in existing_team_ids
    )
    assert created.revision == 3
    assert open_members == {
        source.team_id: ["thread:source-a", "thread:source-b"],
        bystander.team_id: ["thread:bystander-a", "thread:bystander-b"],
        new_team.team_id: ["thread:moved"],
    }
    with store.connect() as connection:
        events = [
            (row["revision"], row["kind"], row["team_id"])
            for row in connection.execute(
                "SELECT revision, kind, team_id FROM events ORDER BY revision"
            )
        ]
    assert events == [
        (1, "createTeam", source.team_id),
        (2, "createTeam", bystander.team_id),
        (3, "createTeam", new_team.team_id),
    ]


def test_removing_an_agent_leaves_a_bystander_teams_roster_intact(tmp_path):
    # Same bystander rule on the removal path: the aliases resolve which slot
    # in the named team the client meant, and reach no further than that team.
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["thread:agent-a", "thread:agent-b"])
    bystander = store.create_team(members=["target:target-a", "thread:agent-keep"])

    store_remove_agent(
        store, team.team_id, "thread:agent-a", aliases=["target:target-a"]
    )

    open_members = {
        state.team_id: [member.agent_id for member in state.members]
        for state in store.team_snapshot().teams
    }
    assert open_members == {
        team.team_id: ["thread:agent-b"],
        bystander.team_id: ["target:target-a", "thread:agent-keep"],
    }
    assert store.current_team_for_agent("target:target-a") == bystander.team_id


def test_team_membership_is_capped_at_six_across_growth_paths(tmp_path):
    # A team renders six accent slots; every growth path must refuse a
    # seventh so a merge or a driver-switch append can never exceed what the
    # board can color. Verified for create, single assign/move, and merge --
    # with merge rejected up front so neither team is left half-changed.
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    with pytest.raises(SpiceError, match="limited to 6 agents"):
        store.create_team(members=[f"thread:{index:032x}" for index in range(7)])

    full = store.create_team(members=[f"thread:{index:032x}" for index in range(6)])
    store.create_team(members=["thread:newcomer"])
    with pytest.raises(SpiceError, match="limited to 6 agents"):
        store.assign_agent(full.team_id, "thread:newcomer")

    source = store.create_team(members=["thread:s1", "thread:s2"])
    with pytest.raises(SpiceError, match="limited to 6 agents"):
        store_merge_teams(store, source.team_id, full.team_id)
    # Both teams untouched by the rejected merge.
    assert len(store.team_state(full.team_id).members) == 6
    assert len(store.team_state(source.team_id).members) == 2


def test_renewal_successor_replaces_slot_on_a_full_team(tmp_path):
    # The exemption that keeps the cap safe: a successor carrying its
    # predecessor's id as an alias inherits that slot, so a driver switch on a
    # full team replaces rather than being blocked by the ceiling.
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    members = [f"thread:{index:032x}" for index in range(6)]
    full = store.create_team(members=members)

    store.assign_agent(full.team_id, "thread:successor", aliases=[members[0]])

    after = [member.agent_id for member in store.team_state(full.team_id).members]
    assert "thread:successor" in after
    assert members[0] not in after
    assert len(after) == 6


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


def test_create_team_reuses_open_shell_instead_of_minting_sibling(tmp_path):
    """The ensure-open-team shell is recycled by the next createTeam so the
    operator never has to close a leftover empty team by hand."""
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    # Snapshot with no teams mints the click-to-import shell.
    shell = store.team_snapshot().teams[0]
    assert shell.members == ()

    created = service.apply(
        {
            "command": "createTeam",
            "members": ["agent-a"],
            "config": {"lifetime": "Steer", "taskFilters": ["serve.ui"]},
        }
    )
    open_teams = created.snapshot.teams

    assert [team.team_id for team in open_teams] == [shell.team_id]
    assert [member.agent_id for member in open_teams[0].members] == ["agent-a"]
    assert open_teams[0].config.lifetime == "Steer"
    assert open_teams[0].config.task_filters == ("serve.ui",)
    with store.connect() as connection:
        event_rows = connection.execute(
            "SELECT kind, payload FROM events ORDER BY revision"
        ).fetchall()
    reuse_events = [
        row for row in event_rows if "reusedOpenShell" in str(row["payload"])
    ]
    assert len(reuse_events) == 1
    assert str(reuse_events[0]["kind"]) == "createTeam"


def test_create_team_with_explicit_id_never_reuses_shell(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    shell = store.team_snapshot().teams[0]

    explicit = store.create_team(team_id="team-explicit", members=["agent-a"])

    assert explicit.team_id == "team-explicit"
    open_ids = {team.team_id for team in store.team_snapshot().teams}
    assert open_ids == {shell.team_id, "team-explicit"}


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
            "agents": [
                {"agentId": "agent-c"},
                {"agentId": "agent-a"},
                {"agentId": "agent-b"},
            ],
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
    identity = store.agent_identity_for_actor("thread:agent-a")
    assert identity is not None
    assert (
        identity.renewal_state,
        identity.renewal_ancestor_thread_id,
        identity.renewal_successor_thread_id,
        identity.renewal_revision,
    ) == ("", "", "", 0)


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


def _completed_handoff_with_successor_assign(store: ServeTeamStore):
    # The live sequence that deadlocked renewal on 2026-07-17: a completed
    # handoff followed by the successor's own startup assign carrying the
    # predecessor id as an alias.
    team = store.create_team(members=["thread:agent-a"])
    _record_identity(store, "thread:agent-a", thread_id="agent-a")
    store.record_pending_renewal(
        agent_id="thread:agent-a", ancestor_thread_id="agent-a"
    )
    store.record_started_renewal(
        predecessor_agent_id="thread:agent-a",
        successor_agent_id="thread:agent-a-renewed",
        ancestor_thread_id="agent-a",
    )
    _record_identity(store, "thread:agent-a-renewed", thread_id="agent-a-renewed")
    store.assign_agent(
        team.team_id, "thread:agent-a-renewed", aliases=["thread:agent-a"]
    )
    return team


def test_renewal_request_reopens_after_successor_assign_with_alias(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    _completed_handoff_with_successor_assign(store)

    requested = store.set_agent_renewal_request(
        "thread:agent-a-renewed", requested=True
    )

    assert requested is not None
    assert requested.state == "requested"
    stored = store.renewal_state_for_agent("thread:agent-a-renewed")
    assert stored is not None
    assert stored.agent_id == "thread:agent-a-renewed"
    assert stored.state == "requested"


def test_renewal_intent_payload_actionable_after_successor_assign(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = _completed_handoff_with_successor_assign(store)

    members = store.team_state(team.team_id).to_payload()["members"]
    fresh = {m["agentId"]: m["renewalIntent"] for m in members}[
        "thread:agent-a-renewed"
    ]
    assert fresh["requested"] is False
    assert fresh["state"] == ""

    store.set_agent_renewal_request("thread:agent-a-renewed", requested=True)
    members = store.team_state(team.team_id).to_payload()["members"]
    toggled = {m["agentId"]: m["renewalIntent"] for m in members}[
        "thread:agent-a-renewed"
    ]
    assert toggled["requested"] is True
    assert toggled["state"] == "requested"


def test_stale_self_keyed_started_row_self_heals_on_next_request(tmp_path):
    # Shape observed in live stores written before the assign-rewrite guard:
    # a started row re-keyed onto the live agent, listing itself as successor.
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["thread:agent-a"])
    _record_identity(store, "thread:agent-a", thread_id="agent-a")
    with store.connect() as connection:
        connection.execute(
            "INSERT INTO renewals (agent_id, team_id, state, "
            "ancestor_thread_id, successor_agent_id, successor_thread_id, "
            "team_slot, predecessor_identity, successor_identity, revision) "
            "VALUES (?, ?, 'started', 'agent-zero', ?, 'agent-a', 0, "
            "'{}', '{}', 167)",
            ("thread:agent-a", team.team_id, "thread:agent-a"),
        )

    assert store.renewal_state_for_agent("thread:agent-a") is None
    assert store.agent_renewal_active("thread:agent-a") is False

    fresh = store.set_agent_renewal_request("thread:agent-a", requested=True)

    assert fresh is not None
    assert fresh.state == "requested"
    reread = store.renewal_state_for_agent("thread:agent-a")
    assert reread is not None
    assert reread.state == "requested"
    assert reread.successor_agent_id == ""


def test_started_transition_facts_survive_successor_assign(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    _completed_handoff_with_successor_assign(store)

    with store.connect() as connection:
        kinds = [
            str(row["kind"])
            for row in connection.execute(
                "SELECT kind FROM events ORDER BY revision"
            ).fetchall()
        ]
    assert "renewalPending" in kinds
    assert "renewalStarted" in kinds


def test_driver_switch_successor_replaces_prior_thread_membership(tmp_path):
    # The bug that produced a seventh member: a team's membership sits under an
    # earlier THREAD of a target (the placeholder was rewritten to a thread on
    # first bind), then a driver switch mints a new thread for the same target.
    # The successor's only shared identity with the roster is the target, so if
    # the promotion offers only the target actor it appends a duplicate. It must
    # also offer the target's prior threads as aliases and inherit the slot.
    from types import SimpleNamespace

    from spice.serve.payload import identity as identity_payload

    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    _record_identity(store, "thread:old", target_id="wt-a", thread_id="old")
    team = store.create_team(members=["thread:old", "thread:b", "thread:c"])

    _record_identity(store, "thread:new", target_id="wt-a", thread_id="new")
    target = SimpleNamespace(id="wt-a")
    names = identity_payload._target_actor_previous_names(store, target, "thread:new")
    assert "thread:old" in names  # the current membership offered as an alias
    identity_payload._promote_team_actor(store, "thread:new", names)

    after = [member.agent_id for member in store.team_state(team.team_id).members]
    assert after == ["thread:new", "thread:b", "thread:c"]  # replaced, not appended
    assert store.current_team_for_agent("thread:old") is None

    # A second switch must stay bounded: it offers the CURRENT membership
    # (thread:new) but never the grandparent (thread:old), so the alias set a
    # successor carries does not grow with the target's thread history.
    _record_identity(store, "thread:newer", target_id="wt-a", thread_id="newer")
    names2 = identity_payload._target_actor_previous_names(
        store, target, "thread:newer"
    )
    assert "thread:new" in names2
    assert "thread:old" not in names2
    identity_payload._promote_team_actor(store, "thread:newer", names2)
    final = [member.agent_id for member in store.team_state(team.team_id).members]
    assert final == ["thread:newer", "thread:b", "thread:c"]


def test_reorder_resolves_client_ids_through_aliases(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply(
        {
            "command": "createTeam",
            "members": ["target:wt-a", "target:wt-b", "target:wt-c"],
        }
    )
    team = created.snapshot.teams[0]

    service.apply(
        {
            "command": "reorderTeamAgents",
            "teamId": team.team_id,
            "agents": [
                {"agentId": "thread:tb", "agentAliases": ["target:wt-b"]},
                {"agentId": "thread:ta", "agentAliases": ["target:wt-a"]},
                {"agentId": "thread:tc", "agentAliases": ["target:wt-c"]},
            ],
        }
    )

    state = store.team_state(team.team_id)
    assert [member.agent_id for member in state.members] == [
        "target:wt-b",
        "target:wt-a",
        "target:wt-c",
    ]


def test_reorder_of_open_subset_holds_hidden_members(tmp_path):
    # The live failure: the client only knows the members it has open as
    # composers, a subset of the team, so it sends fewer entries than there
    # are memberships. Reorder must permute the mentioned members among their
    # own slots and leave every unmentioned member exactly where it was --
    # never reject the whole drag for not naming the full set. Membership is
    # thread-actor form here, matching the real serve database.
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    members = [f"thread:{index:032x}" for index in range(6)]
    created = service.apply({"command": "createTeam", "members": members})
    team = created.snapshot.teams[0]
    assert [member.agent_id for member in team.members] == members

    # Member at index 3 is "closed" on the client; the others are visible and
    # dragged so the first two swap.
    visible = [member for index, member in enumerate(members) if index != 3]
    reordered = [visible[1], visible[0], *visible[2:]]
    result = service.apply(
        {
            "command": "reorderTeamAgents",
            "teamId": team.team_id,
            "agents": [
                {
                    "agentId": agent,
                    "agentAliases": [agent.replace("thread:", "target:")],
                }
                for agent in reordered
            ],
        }
    )

    after = [member.agent_id for member in result.snapshot.teams[0].members]
    assert after[3] == members[3]  # hidden member unmoved
    assert after[0] == members[1] and after[1] == members[0]  # visible swap
    assert set(after) == set(members)  # no member lost or duplicated


def test_reorder_rejects_id_no_alias_resolves(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    service = TeamCommandService(store)
    created = service.apply(
        {"command": "createTeam", "members": ["target:wt-a", "target:wt-b"]}
    )
    team = created.snapshot.teams[0]

    with pytest.raises(SpiceError, match="not assigned"):
        service.apply(
            {
                "command": "reorderTeamAgents",
                "teamId": team.team_id,
                "agents": [
                    {"agentId": "thread:stranger"},
                    {"agentId": "target:wt-b"},
                ],
            }
        )


def test_reorder_then_renew_preserves_successor_visible_slot(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(
        members=["thread:agent-a", "thread:agent-b", "thread:agent-c"]
    )
    store_reorder_team_agents(
        store,
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

    store_reorder_team_agents(
        store,
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

    revision = store_remove_agent(store, team.team_id, "agent-a")
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
            "config": {"lifetime": "Steer"},
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
                "configPatch": {"lifetime": "Steer"},
                "expectedRevision": created.revision,
            }
        )
    state = store.team_state(team.team_id)

    assert first_update.revision > created.revision
    assert state.config_revision == 1
    assert state.config.lifetime == "Drive"


def test_team_config_payload_carries_only_team_scoped_fields():
    # Interface preferences (narration, selected view) are browser-local lane
    # hints; the shared config payload enumerates exactly the team facts.
    payload = TeamConfig().to_payload(7)

    assert sorted(payload) == [
        "lifetime",
        "revision",
        "shellSettings",
        "taskFilterEntries",
        "taskFilters",
    ]
    assert payload["revision"] == 7


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

    initial_revision = store_global_revision(store)
    duplicate = store.add_task_filter(
        team.team_id, "serve.ui", source=TASK_FILTER_SOURCE_MANUAL
    )

    assert duplicate == initial_revision
    assert store_global_revision(store) == initial_revision

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
        {"project": "task.review", "source": TASK_FILTER_SOURCE_MANUAL},
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
            task_filters=("serve.ui", "task.extra"),
            shell_settings=current.shell_settings,
        ),
        replace_task_filters=True,
    )
    replaced = store.team_config(team.team_id)

    assert replaced.task_filters == ("serve.ui", "task.extra", "task.review")
    assert [entry.to_payload() for entry in replaced.task_filter_entries] == [
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_AUTO_CREATE},
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_MANUAL},
        {"project": "task.extra", "source": TASK_FILTER_SOURCE_MANUAL},
        {"project": "task.review", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
    ]


def test_team_config_replace_never_deletes_auto_subscriptions(tmp_path):
    """A stale client-side list must not clobber server-managed auto:* rows."""
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(
        members=["agent-a"], config=TeamConfig(task_filters=("serve.ui",))
    )
    store.add_task_filter(
        team.team_id, "task.review", source=TASK_FILTER_SOURCE_AUTO_CLAIM
    )

    # Client snapshot predates the auto:claim row; its replace list omits it.
    store.update_team_config(
        team.team_id,
        TeamConfig(task_filters=("serve.ui",)),
        replace_task_filters=True,
    )
    after_stale = store.team_config(team.team_id)

    assert after_stale.task_filters == ("serve.ui", "task.review")
    assert [entry.to_payload() for entry in after_stale.task_filter_entries] == [
        {"project": "serve.ui", "source": TASK_FILTER_SOURCE_MANUAL},
        {"project": "task.review", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
    ]

    # An empty pin list clears manual pins only.
    store.update_team_config(
        team.team_id,
        TeamConfig(task_filters=()),
        replace_task_filters=True,
    )
    after_clear = store.team_config(team.team_id)

    assert after_clear.task_filters == ("task.review",)
    assert [entry.to_payload() for entry in after_clear.task_filter_entries] == [
        {"project": "task.review", "source": TASK_FILTER_SOURCE_AUTO_CLAIM},
    ]
