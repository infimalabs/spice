"""Seed an ephemeral, fully isolated N-lane environment for the live-bus probe.

The live-bus latency probe must fire a REAL ``lane.send`` to measure the submit
path, but a lane.send delivers a steer message to the target worktree's agent
inbox. Run against the operator's real worktrees, that wakes real agents. This
seeder builds the same kind of throwaway environment ``spice demo`` uses -- a
fresh git repo, a private driver home with canned transcripts, and a scratch
task backend, all under one disposable root -- but with ``lanes`` git worktrees
instead of one. ``spice serve`` launched with ``cwd`` at the seeded repo and
``CLAUDE_CONFIG_DIR`` at the seeded driver home discovers ONLY these scratch
worktrees, so every task-add, every watch push, and every lane.send stays inside
the ephemeral root. Nothing outside it is touched.

Each seeded worktree resolves as a ``bound`` lane (an authoritative pid-0 agent
state supplies the thread id) whose canned transcript makes it renderable and
watcher-armable, exactly like a real lane, minus any live process.

Usage (invoked by the Node probe harness):
  python tests/browser/test_livebusseed.py --root DIR --lanes 8
Prints one JSON object: {repoRoot, driverHome, taskBackend, lanes:[...]}.

A pytest case in this module exercises ``seed`` directly, so the seeding the
probe depends on is covered without the browser in the loop.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from spice.agent.driver import dashed_uuid
from spice.agent.lifecycle import write_agent_state
from spice.config.edit import set_scope_section
from spice.config.layers import WORKTREE_SOURCE
from spice.process.git import run_git_command
from spice.serve.demo import CANNED_TRANSCRIPT, DEMO_PROJECT_SLUG, DEMO_STARTED_AT

# A 32-hex base whose last byte is overwritten per lane, so every seeded lane
# owns a distinct, obviously-synthetic thread id that renders deterministically.
_THREAD_ID_BASE = "5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e"


def _thread_id(index: int) -> str:
    return _THREAD_ID_BASE[:-2] + f"{index:02x}"


def _init_repo(repo_root: Path) -> None:
    run_git_command(["git", "init", "-q", "-b", "main"], cwd=repo_root, check=True)
    run_git_command(
        [
            "git",
            "-c",
            "user.email=probe@spice.invalid",
            "-c",
            "user.name=spice livebus probe",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "livebus probe seed",
        ],
        cwd=repo_root,
        check=True,
    )


def _write_transcript(driver_home: Path, thread_id: str, name: str) -> Path:
    projects = driver_home / "projects" / DEMO_PROJECT_SLUG
    projects.mkdir(parents=True, exist_ok=True)
    path = projects / f"{dashed_uuid(thread_id)}.jsonl"
    lines = [
        json.dumps(
            {
                "type": "assistant",
                "timestamp": timestamp,
                "message": {
                    "role": "assistant",
                    "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": f"[{name}] {text}"}],
                },
            },
            separators=(",", ":"),
        )
        for timestamp, text in CANNED_TRANSCRIPT
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def _write_state(worktree_root: Path, thread_id: str) -> None:
    # An authoritative-but-stopped state (pid 0) binds the thread to the lane so
    # serve resolves and renders it, while process_status stays idle: a completed
    # conversation, never a live process to steer.
    write_agent_state(
        worktree_root,
        {
            "thread_id": thread_id,
            "mode": "demo",
            "started_at": DEMO_STARTED_AT,
            "prompt_skill_path": str(
                worktree_root / ".agents" / "skills" / "spice" / "SKILL.md"
            ),
            "driver": "claude",
            "model": "claude-opus-4-8",
            "reasoning_effort": "",
            "service_tier": "",
            "pid": 0,
            "process_group_id": 0,
        },
    )


def seed(root: Path, lane_count: int) -> dict:
    root = root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    repo_root = root / "repo"
    repo_root.mkdir()
    _init_repo(repo_root)
    driver_home = root / "driver-home"
    task_backend = root / "task-backend"
    task_backend.mkdir(parents=True, exist_ok=True)

    lanes = []
    for index in range(lane_count):
        if index == 0:
            worktree = repo_root
        else:
            worktree = root / f"lane-{index}"
            run_git_command(
                [
                    "git",
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    f"lane-{index}",
                    str(worktree),
                    "HEAD",
                ],
                cwd=repo_root,
                check=True,
            )
        # Each worktree carries its own driver scope so discovery and identity
        # resolve it as an agent lane regardless of whether the scope file is
        # shared with the base repo.
        set_scope_section(worktree, WORKTREE_SOURCE, "agent", {"driver": "claude"})
        thread_id = _thread_id(index)
        _write_state(worktree, thread_id)
        _write_transcript(driver_home, thread_id, worktree.name)
        lanes.append(
            {
                "index": index,
                "threadId": thread_id,
                "worktree": str(worktree),
                "name": worktree.name,
            }
        )

    return {
        "repoRoot": str(repo_root),
        "driverHome": str(driver_home),
        "taskBackend": str(task_backend),
        "lanes": lanes,
    }


def test_seed_builds_isolated_distinct_lanes(tmp_path):
    result = seed(tmp_path / "seed", 3)

    repo_root = Path(result["repoRoot"])
    driver_home = Path(result["driverHome"])
    task_backend = Path(result["taskBackend"])
    lanes = result["lanes"]

    # Three lanes, each an obviously-synthetic thread id distinct from the rest.
    assert len(lanes) == 3
    thread_ids = [lane["threadId"] for lane in lanes]
    assert len(set(thread_ids)) == 3
    assert thread_ids[0] != thread_ids[1]

    # Every seeded path is a sibling under the one throwaway root -- the
    # isolation the probe relies on so a real lane.send stays in the fixture.
    seed_root = repo_root.parent
    assert (repo_root.name, driver_home.name, task_backend.name) == (
        "repo",
        "driver-home",
        "task-backend",
    )
    assert driver_home.parent == seed_root
    assert task_backend.parent == seed_root

    # Lane 0 is the base repo; the others are linked worktrees. Each exists on
    # disk with a canned transcript serve can render.
    assert Path(lanes[0]["worktree"]) == repo_root
    for lane in lanes:
        assert Path(lane["worktree"]).is_dir()
        transcript = (
            driver_home
            / "projects"
            / DEMO_PROJECT_SLUG
            / f"{dashed_uuid(lane['threadId'])}.jsonl"
        )
        assert transcript.is_file()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--lanes", type=int, default=8)
    args = parser.parse_args()
    print(json.dumps(seed(args.root, args.lanes)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
