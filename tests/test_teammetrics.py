import itertools
import random

from spice.serve.teams import ServeTeamStore, TeamConfig

_directive_seq = itertools.count()


def _seed_lane_metrics(
    store,
    agent_id,
    *,
    acked=0,
    sends=0,
    tool_calls=0,
    team_id=None,
    message_timestamps=(),
):
    # sends/acked are recorded as directives (acked <= sends by construction);
    # tool_calls and message buckets stay on the per-agent activity counter.
    team = team_id or agent_id
    keys = []
    for _ in range(sends):
        key = f"dir-{next(_directive_seq)}"
        store.record_directive_sent(key, agent_id=agent_id, team_id=team)
        keys.append(key)
    for key in keys[:acked]:
        store.mark_directive_acked(key)
    if tool_calls or message_timestamps:
        store.record_agent_metric_delta(
            agent_id, tool_calls=tool_calls, message_timestamps=message_timestamps
        )


def _record_identity(
    store: ServeTeamStore, actor_id: str, *, thread_id: str = ""
) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id="wt-a",
        thread_id=thread_id or actor_id.removeprefix("thread:"),
        actual_driver="codex",
        actual_model="actual-model",
        actual_effort="low",
        actual_service_tier="default",
        desired_driver="codex",
        desired_model="desired-model",
        desired_effort="high",
        transcript_owner="codex",
    )


TEAM_MERGE_ACKED_TOTAL = 14
TEAM_MERGE_SEND_TOTAL = 25
TEAM_MERGE_TOOL_CALL_TOTAL = 36
AGENT_MOVE_LIFETIME_ACKED = 11
AGENT_MOVE_LIFETIME_SEND = 12
AGENT_MOVE_LIFETIME_TOOL_CALL = 13
COMPOSER_MOVE_DEST_ACKED = 14
COMPOSER_MOVE_DEST_SEND = 25
COMPOSER_MOVE_DEST_TOOL_CALL = 36
RESTORED_SUBGROUP_ACKED_TOTAL = 18
RESTORED_SUBGROUP_SEND_TOTAL = 30
RESTORED_SUBGROUP_TOOL_CALL_TOTAL = 42
LEGACY_TEAM_METRIC_TABLES = (
    "team_agent_metrics",
    "team_agent_metric_buckets",
    "team_agent_history",
)
METRIC_INVARIANT_AGENT_IDS = tuple(f"agent-{letter}" for letter in "abcdef")
METRIC_INVARIANT_LIFECYCLE_OPS = frozenset(
    {
        "assign",
        "close",
        "create",
        "merge",
        "move",
        "prune",
        "remove",
        "reorder",
        "split",
        "split_back",
    }
)


def _sum_metric_totals(
    metrics: dict[str, tuple[int, int, int]], agent_ids: tuple[str, ...]
) -> tuple[int, int, int]:
    acked = 0
    sends = 0
    tool_calls = 0
    for agent_id in agent_ids:
        agent_acked, agent_sends, agent_tool_calls = metrics.get(agent_id, (0, 0, 0))
        acked += agent_acked
        sends += agent_sends
        tool_calls += agent_tool_calls
    return acked, sends, tool_calls


def _agent_metric_totals(
    store: ServeTeamStore, agents: tuple[str, ...]
) -> dict[str, tuple[int, int, int]]:
    totals = {agent_id: (0, 0, 0) for agent_id in agents}
    with store.connect() as connection:
        tool_rows = connection.execute(
            "SELECT agent_id, COALESCE(SUM(tool_calls), 0) AS tool_calls "
            "FROM agent_metrics GROUP BY agent_id"
        ).fetchall()
        directive_rows = connection.execute(
            "SELECT agent_id, COALESCE(SUM(sends), 0) AS sends, "
            "COALESCE(SUM(acked), 0) AS acked "
            "FROM directive_totals GROUP BY agent_id"
        ).fetchall()
    tool_calls = {
        str(row["agent_id"]): int(row["tool_calls"] or 0) for row in tool_rows
    }
    directives = {
        str(row["agent_id"]): (int(row["acked"] or 0), int(row["sends"] or 0))
        for row in directive_rows
    }
    for agent_id in agents:
        acked, sends = directives.get(agent_id, (0, 0))
        totals[agent_id] = (acked, sends, tool_calls.get(agent_id, 0))
    return totals


def _membership_by_team(store: ServeTeamStore) -> dict[str, tuple[str, ...]]:
    with store.connect() as connection:
        rows = connection.execute(
            "SELECT m.team_id, m.agent_id "
            "FROM memberships AS m "
            "JOIN teams AS t ON t.team_id = m.team_id "
            "ORDER BY t.created_at, m.position, m.agent_id"
        ).fetchall()
    members: dict[str, list[str]] = {}
    for row in rows:
        members.setdefault(str(row["team_id"]), []).append(str(row["agent_id"]))
    return {team_id: tuple(agent_ids) for team_id, agent_ids in members.items()}


def _agent_team(members_by_team: dict[str, tuple[str, ...]]) -> dict[str, str]:
    teams: dict[str, str] = {}
    for team_id, agent_ids in members_by_team.items():
        for agent_id in agent_ids:
            teams[agent_id] = team_id
    return teams


def _open_team_ids(store: ServeTeamStore) -> tuple[str, ...]:
    with store.connect() as connection:
        rows = connection.execute(
            "SELECT team_id FROM teams WHERE status = 'open' "
            "ORDER BY created_at, team_id"
        ).fetchall()
    return tuple(str(row["team_id"]) for row in rows)


def _split_back_team_ids(store: ServeTeamStore) -> tuple[str, ...]:
    return tuple(
        team_id
        for team_id in _open_team_ids(store)
        if store.team_state(team_id).split_back_available
    )


def _assert_legacy_team_metric_tables_absent(store: ServeTeamStore) -> None:
    with store.connect() as connection:
        rows = connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (?, ?, ?)",
            LEGACY_TEAM_METRIC_TABLES,
        ).fetchall()
    assert {str(row["name"]) for row in rows} == set()


def _assert_team_metric_single_basis_invariant(
    store: ServeTeamStore, expected_metrics: dict[str, tuple[int, int, int]]
) -> None:
    agents = tuple(expected_metrics)
    actual_metrics = _agent_metric_totals(store, agents)
    members_by_team = _membership_by_team(store)
    agent_team = _agent_team(members_by_team)
    total_memberships = sum(len(agent_ids) for agent_ids in members_by_team.values())

    assert actual_metrics == expected_metrics
    assert len(agent_team) == total_memberships
    for agent_id in agents:
        team_id = agent_team.get(agent_id)
        lane_agent_ids = members_by_team[team_id] if team_id else (agent_id,)
        summary = store.lane_metric_summary(agent_id, bucket_count=12, now=0)
        expected_total = _sum_metric_totals(expected_metrics, lane_agent_ids)
        actual_total = _sum_metric_totals(actual_metrics, lane_agent_ids)

        assert summary.agent_ids == lane_agent_ids
        assert (summary.acked, summary.sends, summary.tool_calls) == actual_total
        assert actual_total == expected_total
    _assert_legacy_team_metric_tables_absent(store)


def _record_random_metric_delta(
    store: ServeTeamStore,
    rng: random.Random,
    expected_metrics: dict[str, tuple[int, int, int]],
) -> None:
    agents = tuple(expected_metrics)
    before = _agent_metric_totals(store, agents)
    agent_id = rng.choice(agents)
    sends = rng.randrange(0, 4)
    acked = rng.randrange(0, sends + 1)
    delta = (acked, sends, rng.randrange(0, 3))
    if delta == (0, 0, 0):
        delta = (0, 0, 1)
    _seed_lane_metrics(
        store,
        agent_id,
        acked=delta[0],
        sends=delta[1],
        tool_calls=delta[2],
    )
    current = expected_metrics[agent_id]
    expected_metrics[agent_id] = (
        current[0] + delta[0],
        current[1] + delta[1],
        current[2] + delta[2],
    )
    after = _agent_metric_totals(store, agents)

    for monotonic_agent_id in agents:
        before_totals = before[monotonic_agent_id]
        after_totals = after[monotonic_agent_id]
        assert after_totals[0] >= before_totals[0]
        assert after_totals[1] >= before_totals[1]
        assert after_totals[2] >= before_totals[2]
    assert after == expected_metrics


def _available_lifecycle_ops(
    store: ServeTeamStore, agents: tuple[str, ...]
) -> set[str]:
    members_by_team = _membership_by_team(store)
    agent_team = _agent_team(members_by_team)
    open_team_ids = _open_team_ids(store)
    unassigned_agents = [agent_id for agent_id in agents if agent_id not in agent_team]
    ops = {"create", "prune"}

    if open_team_ids:
        ops.add("close")
    if unassigned_agents and open_team_ids:
        ops.add("assign")
    if _move_candidates(open_team_ids, agent_team):
        ops.add("move")
    if len(open_team_ids) >= 2:
        ops.add("merge")
    if _team_ids_with_member_count(open_team_ids, members_by_team, minimum=1):
        ops.add("remove")
    if _team_ids_with_member_count(open_team_ids, members_by_team, minimum=2):
        ops.add("reorder")
        ops.add("split")
    if _split_back_team_ids(store):
        ops.add("split_back")
    return ops


def _team_ids_with_member_count(
    team_ids: tuple[str, ...],
    members_by_team: dict[str, tuple[str, ...]],
    *,
    minimum: int,
) -> tuple[str, ...]:
    return tuple(
        team_id
        for team_id in team_ids
        if len(members_by_team.get(team_id, ())) >= minimum
    )


def _move_candidates(
    open_team_ids: tuple[str, ...], agent_team: dict[str, str]
) -> tuple[tuple[str, tuple[str, ...]], ...]:
    candidates = []
    for agent_id, current_team_id in sorted(agent_team.items()):
        destination_ids = tuple(
            team_id for team_id in open_team_ids if team_id != current_team_id
        )
        if destination_ids:
            candidates.append((agent_id, destination_ids))
    return tuple(candidates)


def _apply_random_create(
    store: ServeTeamStore, rng: random.Random, agents: tuple[str, ...]
) -> None:
    assigned_agents = set(_agent_team(_membership_by_team(store)))
    candidates = [agent_id for agent_id in agents if agent_id not in assigned_agents]
    if not candidates:
        store.create_team(members=())
        return
    member_count = rng.randint(1, min(2, len(candidates)))
    store.create_team(members=rng.sample(candidates, member_count))


def _apply_random_assign(
    store: ServeTeamStore, rng: random.Random, agents: tuple[str, ...]
) -> None:
    assigned_agents = set(_agent_team(_membership_by_team(store)))
    unassigned_agents = [
        agent_id for agent_id in agents if agent_id not in assigned_agents
    ]
    store.assign_agent(rng.choice(_open_team_ids(store)), rng.choice(unassigned_agents))


def _apply_random_move(
    store: ServeTeamStore, rng: random.Random, _agents: tuple[str, ...]
) -> None:
    candidates = _move_candidates(
        _open_team_ids(store), _agent_team(_membership_by_team(store))
    )
    agent_id, destination_ids = rng.choice(candidates)
    store.assign_agent(rng.choice(destination_ids), agent_id)


def _apply_random_merge(
    store: ServeTeamStore, rng: random.Random, _agents: tuple[str, ...]
) -> None:
    open_team_ids = _open_team_ids(store)
    members_by_team = _membership_by_team(store)
    source_ids = (
        _team_ids_with_member_count(open_team_ids, members_by_team, minimum=1)
        or open_team_ids
    )
    source_team_id = rng.choice(source_ids)
    destination_ids = tuple(
        team_id for team_id in open_team_ids if team_id != source_team_id
    )
    store.merge_teams(source_team_id, rng.choice(destination_ids))


def _apply_random_split(
    store: ServeTeamStore, rng: random.Random, _agents: tuple[str, ...]
) -> None:
    members_by_team = _membership_by_team(store)
    source_ids = _team_ids_with_member_count(
        _open_team_ids(store), members_by_team, minimum=2
    )
    source_team_id = rng.choice(source_ids)
    source_members = members_by_team[source_team_id]
    split_count = rng.randint(1, len(source_members) - 1)
    selected_agents = set(rng.sample(source_members, split_count))
    store.split_team(
        source_team_id,
        agent_ids=[
            agent_id for agent_id in source_members if agent_id in selected_agents
        ],
    )


def _apply_random_split_back(
    store: ServeTeamStore, rng: random.Random, _agents: tuple[str, ...]
) -> None:
    store.split_team_back(rng.choice(_split_back_team_ids(store)))


def _apply_random_remove(
    store: ServeTeamStore, rng: random.Random, _agents: tuple[str, ...]
) -> None:
    members_by_team = _membership_by_team(store)
    team_ids = _team_ids_with_member_count(
        _open_team_ids(store), members_by_team, minimum=1
    )
    team_id = rng.choice(team_ids)
    store.remove_agent(team_id, rng.choice(members_by_team[team_id]))


def _apply_random_close(
    store: ServeTeamStore, rng: random.Random, _agents: tuple[str, ...]
) -> None:
    store.close_team(rng.choice(_open_team_ids(store)))


def _apply_random_prune(
    store: ServeTeamStore, _rng: random.Random, _agents: tuple[str, ...]
) -> None:
    store.prune_zero_activity_closed_teams()


def _apply_random_reorder(
    store: ServeTeamStore, rng: random.Random, _agents: tuple[str, ...]
) -> None:
    members_by_team = _membership_by_team(store)
    team_ids = _team_ids_with_member_count(
        _open_team_ids(store), members_by_team, minimum=2
    )
    team_id = rng.choice(team_ids)
    ordered_agent_ids = list(members_by_team[team_id])
    rng.shuffle(ordered_agent_ids)
    if ordered_agent_ids == list(members_by_team[team_id]):
        ordered_agent_ids.reverse()
    store.reorder_team_agents(team_id, ordered_agent_ids)


METRIC_INVARIANT_OP_HANDLERS = {
    "assign": _apply_random_assign,
    "close": _apply_random_close,
    "create": _apply_random_create,
    "merge": _apply_random_merge,
    "move": _apply_random_move,
    "prune": _apply_random_prune,
    "remove": _apply_random_remove,
    "reorder": _apply_random_reorder,
    "split": _apply_random_split,
    "split_back": _apply_random_split_back,
}


def _apply_lifecycle_op(
    store: ServeTeamStore,
    rng: random.Random,
    op: str,
    agents: tuple[str, ...],
    expected_metrics: dict[str, tuple[int, int, int]],
) -> None:
    before = _agent_metric_totals(store, agents)
    METRIC_INVARIANT_OP_HANDLERS[op](store, rng, agents)
    after = _agent_metric_totals(store, agents)

    assert after == before
    assert after == expected_metrics
    _assert_team_metric_single_basis_invariant(store, expected_metrics)


def test_lane_metrics_drop_removed_member_counts(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    team = store.create_team(members=["agent-a"])
    _seed_lane_metrics(store, "agent-a", acked=1, sends=2, tool_calls=3)
    store.assign_agent(team.team_id, "agent-b")
    _seed_lane_metrics(store, "agent-b", acked=4, sends=5, tool_calls=6)
    store.remove_agent(team.team_id, "agent-a")

    summary = store.lane_metric_summary("agent-b", bucket_count=12)

    # Work follows the agent: agent-a left, so its counters leave the lane (they
    # live on the agent and resurface wherever it lands next).
    assert summary.agent_ids == ("agent-b",)
    assert summary.acked == 4
    assert summary.sends == 5
    assert summary.tool_calls == 6


def test_lane_metrics_follow_agent_across_move(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a"])
    destination = store.create_team(members=["agent-b"])
    _seed_lane_metrics(store, "agent-a", acked=10, sends=10, tool_calls=10)

    store.assign_agent(destination.team_id, "agent-a")
    _seed_lane_metrics(store, "agent-a", acked=1, sends=2, tool_calls=3)

    destination_summary = store.lane_metric_summary("agent-b", bucket_count=12)
    moved_summary = store.lane_metric_summary("agent-a", bucket_count=12)

    assert store.team_state(source.team_id).status == "closed"
    # agent-a carries its full lifetime counters (10+1 / 10+2 / 10+3) into the
    # destination lane; agent-b contributes nothing.
    assert destination_summary.acked == AGENT_MOVE_LIFETIME_ACKED
    assert destination_summary.sends == AGENT_MOVE_LIFETIME_SEND
    assert destination_summary.tool_calls == AGENT_MOVE_LIFETIME_TOOL_CALL
    assert moved_summary == destination_summary
    assert moved_summary.acked == AGENT_MOVE_LIFETIME_ACKED
    assert moved_summary.sends == AGENT_MOVE_LIFETIME_SEND
    assert moved_summary.tool_calls == AGENT_MOVE_LIFETIME_TOOL_CALL


def test_composer_move_carries_agent_metrics_to_destination(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a", "agent-c"])
    destination = store.create_team(members=["agent-b"])
    _seed_lane_metrics(store, "agent-a", acked=10, sends=20, tool_calls=30)
    _seed_lane_metrics(store, "agent-c", acked=1, sends=2, tool_calls=3)
    _seed_lane_metrics(store, "agent-b", acked=4, sends=5, tool_calls=6)

    store.assign_agent(destination.team_id, "agent-a")

    source_after = store.lane_metric_summary("agent-c", bucket_count=12)
    destination_after = store.lane_metric_summary("agent-b", bucket_count=12)

    assert store.team_state(source.team_id).status == "open"
    # agent-a moved out: the source lane shows only agent-c; the destination lane
    # gains agent-a's counters on top of agent-b's. The metric assertion is about
    # membership, so it does not depend on the exact visible roster order.
    assert source_after.agent_ids == ("agent-c",)
    assert source_after.acked == 1
    assert source_after.sends == 2
    assert source_after.tool_calls == 3
    assert set(destination_after.agent_ids) == {"agent-a", "agent-b"}
    assert destination_after.acked == COMPOSER_MOVE_DEST_ACKED
    assert destination_after.sends == COMPOSER_MOVE_DEST_SEND
    assert destination_after.tool_calls == COMPOSER_MOVE_DEST_TOOL_CALL


def test_lane_merge_moves_source_metrics_into_destination_once(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a"])
    destination = store.create_team(members=["agent-b"])
    _seed_lane_metrics(
        store,
        "agent-a",
        acked=10,
        sends=20,
        tool_calls=30,
        message_timestamps=[120, 180],
    )
    _seed_lane_metrics(
        store,
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
    assert set(destination_after.agent_ids) == {"agent-a", "agent-b"}
    assert destination_after.acked == TEAM_MERGE_ACKED_TOTAL
    assert destination_after.sends == TEAM_MERGE_SEND_TOTAL
    assert destination_after.tool_calls == TEAM_MERGE_TOOL_CALL_TOTAL
    assert sum(destination_after.sparkline) == 3
    assert moved_after == destination_after
    assert repeated_after == destination_after


def test_lane_metrics_can_scope_to_latest_renewal_session(tmp_path, monkeypatch):
    clock = {"now": 0.0}
    monkeypatch.setattr("spice.serve.teams.time.time", lambda: clock["now"])
    monkeypatch.setattr("spice.serve.teammetrics.time.time", lambda: clock["now"])
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    predecessor = "thread:predecessor"
    successor = "thread:successor"

    team = store.create_team(members=[predecessor])
    _record_identity(store, predecessor, thread_id="predecessor")
    store.record_directive_sent(
        "dir-pre",
        agent_id=predecessor,
        team_id=team.team_id,
        sent_at=60,
    )
    store.mark_directive_acked("dir-pre", acked_at=70)
    store.record_agent_metric_delta(
        predecessor,
        tool_calls=3,
        tool_call_timestamps=[60, 61, 62],
        message_timestamps=[60, 60],
    )

    clock["now"] = 120
    store.record_started_renewal(
        predecessor_agent_id=predecessor,
        successor_agent_id=successor,
        ancestor_thread_id="predecessor",
    )
    store.record_directive_sent(
        "dir-post",
        agent_id=successor,
        team_id=team.team_id,
        sent_at=180,
    )
    store.mark_directive_acked("dir-post", acked_at=190)
    store.record_agent_metric_delta(
        successor,
        tool_calls=5,
        tool_call_timestamps=[180, 181, 182, 183, 184],
        message_timestamps=[180, 240],
    )

    lineage = store.lane_metric_summary(successor, bucket_count=5, now=240)
    session = store.lane_metric_summary(
        successor,
        bucket_count=5,
        now=240,
        since_latest_renewal=True,
    )

    assert lineage.agent_ids == (successor,)
    assert (lineage.acked, lineage.sends, lineage.tool_calls) == (2, 2, 8)
    assert sum(lineage.sparkline) == 4
    assert session.agent_ids == (successor,)
    assert (session.acked, session.sends, session.tool_calls) == (1, 1, 5)
    assert sum(session.sparkline) == 2
    with store.connect() as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) AS count FROM agent_metrics WHERE agent_id = ?",
                (predecessor,),
            ).fetchone()["count"]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) AS count FROM directives WHERE agent_id = ?",
                (predecessor,),
            ).fetchone()["count"]
            == 0
        )


def test_team_historical_metric_summary_projects_membership_intervals(
    tmp_path, monkeypatch
):
    clock = {"now": 0.0}
    monkeypatch.setattr("spice.serve.teams.time.time", lambda: clock["now"])
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")

    def set_time(timestamp: float) -> None:
        clock["now"] = timestamp

    set_time(0)
    team = store.create_team(members=["agent-a", "agent-b"])
    _seed_lane_metrics(store, "agent-a", message_timestamps=[60])
    _seed_lane_metrics(store, "agent-b", message_timestamps=[60])

    set_time(120)
    split = store.split_team(
        team.team_id, agent_ids=["agent-a"], new_team_id="team-split"
    )
    _seed_lane_metrics(store, "agent-a", message_timestamps=[180])
    _seed_lane_metrics(store, "agent-b", message_timestamps=[180])

    set_time(240)
    store.merge_teams(split.team_id, team.team_id)
    _seed_lane_metrics(store, "agent-a", message_timestamps=[300])
    _seed_lane_metrics(store, "agent-b", message_timestamps=[300])

    set_time(360)
    store.split_team_back(team.team_id)
    _seed_lane_metrics(store, "agent-a", message_timestamps=[420])
    _seed_lane_metrics(store, "agent-b", message_timestamps=[420])

    set_time(480)
    store.remove_agent(team.team_id, "agent-b")
    _seed_lane_metrics(store, "agent-b", message_timestamps=[540])

    set_time(600)
    store.close_team(split.team_id)
    _seed_lane_metrics(store, "agent-a", message_timestamps=[660])

    team_history = store.team_historical_metric_summary(
        team.team_id, bucket_count=16, now=720
    )
    split_history = store.team_historical_metric_summary(
        split.team_id, bucket_count=16, now=720
    )

    assert team_history.agent_ids == ("agent-a", "agent-b")
    assert team_history.messages == 6
    assert sum(team_history.sparkline) == 6
    assert split_history.agent_ids == ("agent-a",)
    assert split_history.messages == 2
    assert sum(split_history.sparkline) == 2


def test_split_team_back_moves_subgroup_metrics_back_to_restored_team(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    source = store.create_team(members=["agent-a", "agent-b"])
    destination = store.create_team(members=["agent-c"])
    _seed_lane_metrics(store, "agent-a", acked=10, sends=20, tool_calls=30)
    _seed_lane_metrics(store, "agent-b", acked=1, sends=2, tool_calls=3)
    _seed_lane_metrics(store, "agent-c", acked=4, sends=5, tool_calls=6)

    store.merge_teams(source.team_id, destination.team_id)
    _seed_lane_metrics(store, "agent-a", acked=7, sends=8, tool_calls=9)
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


def test_zero_activity_prune_reaps_metric_only_but_keeps_config_teams(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    # Metrics are per-agent now (no team-scoped counter), so a closed team whose
    # only history was metric activity carries no durable team state and is
    # correctly pruned; agent-a's counters live on the agent regardless.
    metric_team = store.create_team(team_id="team-metric", members=["agent-a"])
    _seed_lane_metrics(store, "agent-a", sends=1)
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

    # team-metric is gone (pruned); team-config survives on its task filter.
    assert {row["team_id"]: row["status"] for row in team_rows} == {
        "team-config": "closed",
        snapshot.teams[0].team_id: "open",
    }
    # agent-a's send is preserved on the agent even though its team was reaped.
    assert store.lane_metric_summary("agent-a", bucket_count=12).sends == 1


def test_fresh_team_store_has_no_legacy_team_metric_tables(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")

    _assert_legacy_team_metric_tables_absent(store)


def test_team_metric_single_basis_invariant_survives_random_lifecycle_sequence(
    tmp_path,
):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    rng = random.Random(20260621)
    agents = METRIC_INVARIANT_AGENT_IDS
    expected_metrics = {agent_id: (0, 0, 0) for agent_id in agents}
    seen_ops: set[str] = set()
    shuffled_agents = list(agents)
    rng.shuffle(shuffled_agents)

    for members in (shuffled_agents[:2], shuffled_agents[2:4]):
        before = _agent_metric_totals(store, agents)
        store.create_team(members=members)
        seen_ops.add("create")
        assert _agent_metric_totals(store, agents) == before
        _assert_team_metric_single_basis_invariant(store, expected_metrics)

    for _ in range(80):
        _record_random_metric_delta(store, rng, expected_metrics)
        _assert_team_metric_single_basis_invariant(store, expected_metrics)

        remaining_ops = METRIC_INVARIANT_LIFECYCLE_OPS - seen_ops
        available_ops = _available_lifecycle_ops(store, agents)
        if "split_back" in remaining_ops and "split_back" not in available_ops:
            op = (
                "merge"
                if "merge" in available_ops
                else rng.choice(sorted(available_ops))
            )
        else:
            required_available = sorted(remaining_ops & available_ops)
            op = rng.choice(required_available or sorted(available_ops))
        _apply_lifecycle_op(store, rng, op, agents, expected_metrics)
        seen_ops.add(op)

    assert METRIC_INVARIANT_LIFECYCLE_OPS <= seen_ops


def test_activity_metrics_are_tagged_with_team_at_capture(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    # Solo agent (no team): tagged with its own id (private team).
    store.record_agent_metric_delta(
        "agent-solo", tool_calls=1, message_timestamps=[1000]
    )
    # Assigned agent: tagged with the real team it is on at capture time.
    team = store.create_team(members=["agent-team"])
    store.record_agent_metric_delta(
        "agent-team", tool_calls=2, message_timestamps=[1000]
    )

    with store.connect() as connection:
        metric_team = {
            str(row["agent_id"]): str(row["team_id"])
            for row in connection.execute("SELECT agent_id, team_id FROM agent_metrics")
        }
        bucket_team = {
            str(row["agent_id"]): str(row["team_id"])
            for row in connection.execute(
                "SELECT agent_id, team_id FROM agent_metric_buckets"
            )
        }

    assert metric_team["agent-solo"] == "agent-solo"
    assert metric_team["agent-team"] == team.team_id
    assert bucket_team["agent-solo"] == "agent-solo"
    assert bucket_team["agent-team"] == team.team_id
    # The lane read still sums per-agent across teams (membership-derived).
    assert store.lane_metric_summary("agent-team", bucket_count=12).tool_calls == 2
