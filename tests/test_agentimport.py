"""spice agent import: team-membership carry (import as a renewal)."""

import subprocess
from pathlib import Path

from spice.agent import lifecycle
from spice.agent.identity import canonical_thread_id
from spice.serve.team.store import ServeTeamStore

PREDECESSOR_UUID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
SUCCESSOR_UUID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"


def _init_git_repo(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    subprocess.run(
        ["git", "config", "user.email", "spice@example.test"], cwd=path, check=True
    )
    subprocess.run(["git", "config", "user.name", "Spice Tests"], cwd=path, check=True)


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


def test_import_from_conveys_lineage_into_a_fresh_worktree(tmp_path, monkeypatch):
    # The fork case: a fresh worktree has no locally-bound predecessor, so
    # `--from` must supply the lineage `import_agent` would otherwise resolve
    # from `agent_status(repo_root)`.
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    store_path = tmp_path / "teams.sqlite3"
    predecessor = canonical_thread_id(PREDECESSOR_UUID)
    successor = canonical_thread_id(SUCCESSOR_UUID)
    store = ServeTeamStore(path=store_path)
    team = store.create_team(members=[predecessor])
    monkeypatch.setattr(
        "spice.serve.team.store.ServeTeamStore",
        lambda: ServeTeamStore(path=store_path),
    )

    lifecycle.import_agent(repo, SUCCESSOR_UUID, predecessor_thread=PREDECESSOR_UUID)

    members = [m.agent_id for m in store.team_state(team.team_id).members]
    assert successor in members
    assert predecessor not in members  # slot moved, team did not grow


def test_import_without_from_is_unchanged_manual_renewal(tmp_path, monkeypatch):
    # No --from: behaves exactly like the plain renewal case (resolve the
    # locally-bound predecessor via agent_status), unaffected by the new
    # parameter existing.
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    store_path = tmp_path / "teams.sqlite3"
    predecessor = canonical_thread_id(PREDECESSOR_UUID)
    successor = canonical_thread_id(SUCCESSOR_UUID)
    monkeypatch.setattr(
        "spice.serve.team.store.ServeTeamStore",
        lambda: ServeTeamStore(path=store_path),
    )
    # Bind the worktree to the predecessor through the real import path first
    # (no team exists yet, so this call itself carries nothing).
    lifecycle.import_agent(repo, PREDECESSOR_UUID)
    store = ServeTeamStore(path=store_path)
    team = store.create_team(members=[predecessor])

    lifecycle.import_agent(repo, SUCCESSOR_UUID)

    members = [m.agent_id for m in store.team_state(team.team_id).members]
    assert successor in members
    assert predecessor not in members  # slot moved, team did not grow


def test_import_carry_seats_the_imported_driver_on_the_member(tmp_path, monkeypatch):
    store_path = tmp_path / "teams.sqlite3"
    store = ServeTeamStore(path=store_path)
    store.create_team(members=["thread:pred"])
    monkeypatch.setattr(
        "spice.serve.team.store.ServeTeamStore",
        lambda: ServeTeamStore(path=store_path),
    )

    lifecycle._carry_team_membership("thread:pred", "thread:succ", "codex")

    identity = store.agent_identity_for_actor("thread:succ")
    assert identity is not None
    assert identity.actual_driver == "codex"
    assert identity.desired_driver == "codex"
