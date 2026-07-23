"""Serve keeps a reused thread bound to exactly one worktree lane."""

from __future__ import annotations

import subprocess
from pathlib import Path
from threading import Event, Thread

from spice.agent.lifecycle import agent_status, write_agent_state
from spice.agent.paths import (
    agent_thread_state_dir,
    read_agent_thread_pointer,
)
from spice.serve.app import ServeState
from spice.serve.payload.identity import (
    serve_agent_identity_payload,
    target_bound_actor,
)
from spice.serve.team.store import ServeTeamStore
from spice.serve.worktree.bindings import reconcile_target_thread_bindings
from spice.serve.worktree.target import discover_serve_worktrees

THREAD_ID = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
REBOUND_THREAD_ID = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"


def test_binding_resolution_resolves_one_git_state_root_per_target(
    tmp_path, monkeypatch
):
    import spice.paths as state_paths

    lane_a, lane_c = _linked_worktrees(tmp_path)
    _write_idle_binding(lane_a, started_at="2026-07-22T19:04:22.000000Z")
    _write_idle_binding(
        lane_c,
        started_at="2026-07-22T19:22:19.000000Z",
        thread_id=REBOUND_THREAD_ID,
    )
    targets = discover_serve_worktrees(cwd=lane_a)
    real_run_git_command = state_paths.run_git_command
    git_dir_roots: list[str] = []

    def counted_run_git_command(argv, *args, **kwargs):
        if "--git-dir" in argv:
            git_dir_roots.append(str(Path(argv[2]).resolve()))
        return real_run_git_command(argv, *args, **kwargs)

    monkeypatch.setattr(state_paths, "run_git_command", counted_run_git_command)

    resolved = reconcile_target_thread_bindings(targets)

    assert resolved == {
        next(target.id for target in targets if target.repo_root == lane_a): THREAD_ID,
        next(target.id for target in targets if target.repo_root == lane_c): (
            REBOUND_THREAD_ID
        ),
    }
    assert git_dir_roots == [str(target.repo_root.resolve()) for target in targets]


def test_newest_lane_owns_a_thread_reused_across_distinct_worktrees(tmp_path):
    """A moved session cannot collapse every worktree it previously inhabited."""
    lane_a, lane_c = _linked_worktrees(tmp_path)
    _write_idle_binding(lane_a, started_at="2026-07-22T19:04:22.000000Z")
    _write_idle_binding(lane_c, started_at="2026-07-22T19:22:19.000000Z")

    # This is the production failure: separate pointer files and thread-state
    # directories contain one reused session id, which used to derive one actor
    # and one oscillating agent_identities row for both lanes.
    assert [agent_status(root).thread_id for root in (lane_a, lane_c)] == [
        THREAD_ID,
        THREAD_ID,
    ]
    assert (
        len({agent_thread_state_dir(root, THREAD_ID) for root in (lane_a, lane_c)}) == 2
    )

    store = ServeTeamStore(tmp_path / "teams.sqlite3")
    state = ServeState(anchor_root=lane_a, team_store=store)
    targets = state.worktree_targets()
    target_a, target_c = sorted(targets, key=lambda target: target.name)

    assert [target.name for target in targets] == ["lane-a", "lane-c"]
    resolved_threads = [
        agent_status(target.repo_root).thread_id for target in (target_a, target_c)
    ]
    assert resolved_threads == ["", THREAD_ID]
    assert [
        target_bound_actor(target, thread)
        for target, thread in zip((target_a, target_c), resolved_threads)
    ] == [f"target:{target_a.id}", f"thread:{THREAD_ID}"]

    identities = [
        serve_agent_identity_payload(target, store=store)
        for target in (target_a, target_c)
    ]
    assert [item["actorId"] for item in identities] == [
        f"target:{target_a.id}",
        f"thread:{THREAD_ID}",
    ]
    assert [
        store.agent_identity_for_actor(item["actorId"]).target_id for item in identities
    ] == [target_a.id, target_c.id]


def test_rebound_pointer_survives_stale_cleanup_interleaving(tmp_path, monkeypatch):
    """A writer replacing the verified pathname cannot be deleted as stale."""
    from spice.serve.worktree import bindings

    lane_a, lane_c = _linked_worktrees(tmp_path)
    _write_idle_binding(lane_a, started_at="2026-07-22T19:04:22.000000Z")
    _write_idle_binding(lane_c, started_at="2026-07-22T19:22:19.000000Z")
    real_fingerprint = bindings._pointer_fingerprint
    real_clear_stale_binding = bindings._clear_stale_binding
    writer_done = Event()
    writer: Thread | None = None
    clearing_rebound_lane = False

    def rebind_lane_a() -> None:
        _write_idle_binding(
            lane_a,
            started_at="2026-07-22T19:23:00.000000Z",
            thread_id=REBOUND_THREAD_ID,
        )
        writer_done.set()

    def interleaved_fingerprint(stat):
        nonlocal writer
        fingerprint = real_fingerprint(stat)
        if clearing_rebound_lane and writer is None:
            writer = Thread(target=rebind_lane_a, daemon=True)
            writer.start()
            # The old code lets the writer finish inside this window and then
            # unlinks its replacement. The locked writer waits here until stale
            # cleanup removes only the snapshotted inode and releases the lock.
            writer_done.wait(1.0)
        return fingerprint

    def instrumented_clear_stale_binding(thread_id, binding) -> None:
        # Open the window across the revalidation of the one pointer this writer
        # replaces. Counting fingerprints to reach that moment instead spends the
        # ordinal on calls this fixture does not own: bind a third lane and the
        # third fingerprint is discovery, so the writer lands before any cleanup
        # and the run dies on the reconciler's own changed-pointer guard --
        # red against correct code, and no race run either way.
        nonlocal clearing_rebound_lane
        clearing_rebound_lane = binding.target.repo_root.resolve() == lane_a.resolve()
        try:
            real_clear_stale_binding(thread_id, binding)
        finally:
            clearing_rebound_lane = False

    monkeypatch.setattr(bindings, "_pointer_fingerprint", interleaved_fingerprint)
    monkeypatch.setattr(
        bindings, "_clear_stale_binding", instrumented_clear_stale_binding
    )
    state = ServeState(anchor_root=lane_a)

    state.worktree_targets()
    assert writer is not None
    writer.join(timeout=2.0)
    assert read_agent_thread_pointer(lane_a) == REBOUND_THREAD_ID

    state.worktree_targets()
    assert [agent_status(root).thread_id for root in (lane_a, lane_c)] == [
        REBOUND_THREAD_ID,
        THREAD_ID,
    ]


def _linked_worktrees(tmp_path: Path) -> tuple[Path, Path]:
    lane_a = tmp_path / "lane-a"
    lane_c = tmp_path / "lane-c"
    lane_a.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=lane_a, check=True)
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"], cwd=lane_a, check=True
    )
    subprocess.run(["git", "config", "user.name", "Test User"], cwd=lane_a, check=True)
    (lane_a / "seed.txt").write_text("seed\n", encoding="utf-8")
    subprocess.run(["git", "add", "seed.txt"], cwd=lane_a, check=True)
    subprocess.run(["git", "commit", "-qm", "seed"], cwd=lane_a, check=True)
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "lane-c", str(lane_c)],
        cwd=lane_a,
        check=True,
    )
    return lane_a, lane_c


def _write_idle_binding(
    repo_root: Path, *, started_at: str, thread_id: str = THREAD_ID
) -> None:
    write_agent_state(
        repo_root,
        {
            "pid": 0,
            "process_group_id": 0,
            "started_at": started_at,
            "mode": "bind",
            "command": [],
            "driver": "codex",
            "model": "gpt-5.6-sol",
            "reasoning_effort": "xhigh",
            "service_tier": "",
            "thread_id": thread_id,
            "prompt_skill_path": str(repo_root / ".agents/skills/spice/SKILL.md"),
            "log_path": "",
        },
    )
