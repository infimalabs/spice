"""Backend cost of the serve topology-change round trip (LATENCY-1kGFQGQN).

Guards the measured premise behind the topology-latency diagnosis
(docs/design/experimental/serve-topology-latency-diagnosis.md): the backend half
of a topology change is cheap, and a per-team differential is strictly smaller
than the full-topology snapshot the store emits today. Byte sizes are
deterministic so the assertions stay green in CI; the wall-clock stages are only
printed (run ``spice dev pytest tests/serveperf/test_topologycost.py -s``) so the
instrument can refresh the diagnosis numbers without asserting on timing, which
is not deterministic under load.
"""

from __future__ import annotations

import json
import statistics
import time

from spice.serve.team.store import ServeTeamStore, TeamCommandService

TEAMS = 3
MEMBERS_PER_TEAM = 3
TIMING_ROUNDS = 50
MS_PER_SECOND = 1000.0
KIBIBYTE = 1024
SNAPSHOT_CEILING_KIB = 16
# The measured full snapshot at this topology is ~3 KB (see the diagnosis doc);
# a blowup past this generous ceiling (e.g. embedding transcripts in team facts)
# would regress topology responsiveness and should fail here.
MAX_FULL_SNAPSHOT_BYTES = SNAPSHOT_CEILING_KIB * KIBIBYTE


def _build_topology(store: ServeTeamStore, teams: int, members_per_team: int) -> None:
    """Create ``teams`` open teams each holding ``members_per_team`` agents."""
    service = TeamCommandService(store=store)
    for team_index in range(teams):
        snapshot = store.team_snapshot()
        # createTeam relocates member[0]; give it a fresh agent id.
        service.apply(
            {
                "command": "createTeam",
                "expectedRevision": snapshot.global_revision,
                "members": [f"agent-{team_index}-0"],
            }
        )
        team_id = store.team_snapshot().teams[-1].team_id  # newest open team
        for member_index in range(1, members_per_team):
            snapshot = store.team_snapshot()
            service.apply(
                {
                    "command": "moveAgentToTeam",
                    "expectedRevision": snapshot.global_revision,
                    "teamId": team_id,
                    "agentId": f"agent-{team_index}-{member_index}",
                }
            )


def _print_stage(label: str, fn, rounds: int) -> None:
    samples_ms: list[float] = []
    for _ in range(rounds):
        start = time.perf_counter()
        fn()
        samples_ms.append((time.perf_counter() - start) * MS_PER_SECOND)
    ordered = sorted(samples_ms)
    p50 = ordered[len(ordered) // 2]
    print(f"  {label:<24} p50={p50:.3f} mean={statistics.fmean(ordered):.3f} (ms)")


def test_topology_snapshot_is_cheap_and_delta_is_smaller(tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    _build_topology(store, TEAMS, MEMBERS_PER_TEAM)
    snapshot = store.team_snapshot()

    assert len(snapshot.teams) == TEAMS
    assert sum(len(team.members) for team in snapshot.teams) == TEAMS * MEMBERS_PER_TEAM

    full = json.dumps(snapshot.to_payload(), separators=(",", ":"))
    delta = json.dumps(snapshot.teams[-1].to_payload(), separators=(",", ":"))

    # A single-team differential is a strict subset of the full snapshot, so it
    # must be materially smaller -- the measured opportunity boarded as the
    # per-team differential follow-up (LATENCY-1kGFrWKF).
    assert len(delta) < len(full)
    # Backend payload stays small at a realistic operator topology.
    assert len(full) < MAX_FULL_SNAPSHOT_BYTES

    print(f"\n=== {TEAMS} teams, {TEAMS * MEMBERS_PER_TEAM} members ===")
    print(f"  full snapshot bytes      = {len(full)}")
    print(
        f"  single-team delta bytes  = {len(delta)}  (ratio {len(full) / len(delta):.2f}x)"
    )
    _print_stage("team_snapshot build", store.team_snapshot, TIMING_ROUNDS)
    _print_stage(
        "to_payload serialize",
        lambda: store.team_snapshot().to_payload(),
        TIMING_ROUNDS,
    )
