"""spice agent import: team-membership carry (import as a renewal)."""

from spice.agent import lifecycle
from spice.serve.team.store import ServeTeamStore


def test_import_carries_predecessor_team_slot_to_successor(tmp_path, monkeypatch):
    store_path = tmp_path / "teams.sqlite3"
    store = ServeTeamStore(path=store_path)
    team = store.create_team(members=["thread:pred"])
    monkeypatch.setattr(
        "spice.serve.team.store.ServeTeamStore",
        lambda: ServeTeamStore(path=store_path),
    )

    # A renewal: the imported thread inherits the predecessor's slot.
    lifecycle._carry_team_membership("thread:pred", "thread:succ")

    members = [m.agent_id for m in store.team_state(team.team_id).members]
    assert "thread:succ" in members
    assert "thread:pred" not in members  # slot moved, team did not grow


def test_import_carry_is_a_noop_without_a_team_or_predecessor(tmp_path, monkeypatch):
    store_path = tmp_path / "teams.sqlite3"
    store = ServeTeamStore(path=store_path)
    team = store.create_team(members=["thread:only"])
    monkeypatch.setattr(
        "spice.serve.team.store.ServeTeamStore",
        lambda: ServeTeamStore(path=store_path),
    )

    lifecycle._carry_team_membership("", "thread:succ")  # no predecessor
    lifecycle._carry_team_membership("thread:stranger", "thread:succ")  # not a member

    members = [m.agent_id for m in store.team_state(team.team_id).members]
    assert members == ["thread:only"]  # untouched, no growth
