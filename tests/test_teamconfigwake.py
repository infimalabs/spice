"""updateTeamConfig must not wake the lane watchers (cross-team reflow fix)."""

from spice.tasks import config as task_config
from spice.serve.team.store import ServeTeamStore, TeamConfig
from tests.test_teamstorehelpers import store_remove_agent


def test_update_team_config_does_not_wake_the_lane_watchers(tmp_path):
    # A lifetime/filter change alters only THIS team's effective task view, but
    # waking bumps the shared task event file that sits in every lane's
    # signature across every team -- so it reflowed unrelated teams' boards. It
    # must NOT wake: the config revision rides the team channel and each client
    # resubscribes only the lanes whose config revision actually advanced.
    task_config.set_backend(str(tmp_path / "task-backend"))
    try:
        event_path = task_config.ensure_task_event_file()
        store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
        team = store.create_team(members=["thread:a"])

        after_create = event_path.read_text(encoding="utf-8")
        store.update_team_config(team.team_id, TeamConfig(lifetime="Steer"))
        assert event_path.read_text(encoding="utf-8") == after_create  # no wake

        # A membership change still wakes -- its lane stream genuinely changes.
        store_remove_agent(store, team.team_id, "thread:a")
        assert event_path.read_text(encoding="utf-8") != after_create

        assert store.team_config(team.team_id).lifetime == "Steer"
    finally:
        task_config.set_backend(None)
