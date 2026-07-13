"""`spice demo` — a zero-setup preview of the serve UI from a canned transcript.

The demo seeds an isolated, ephemeral environment (its own git repo, driver
home, and task backend under a throwaway directory) with one lane whose
transcript is a canned, in-repo conversation. Serve then renders that lane
exactly as it renders a live agent, so a newcomer experiences the product with
no agent subscription and no model calls at all. Nothing is written outside the
ephemeral root, so the operator's real worktrees and task board stay untouched.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.agent.driver import dashed_uuid
from spice.agent.lifecycle import write_agent_state
from spice.config import WORKTREE_SOURCE, set_scope_section
from spice.errors import SpiceError
from spice.gitprocess import run_git_command
from spice.serve.app import DEFAULT_SERVE_HOST, DEFAULT_SERVE_PORT, run_serve

# A fixed, obviously-synthetic thread id keeps the seeded transcript path and
# the rendered lane deterministic across runs (32 hex chars).
DEMO_THREAD_ID = "dec0dec0dec0dec0dec0dec0dec0dec0"
# Any subdirectory under the driver's projects root is discovered by the
# claude transcript glob, so the demo uses a stable, recognizable slug.
DEMO_PROJECT_SLUG = "spice-demo"
DEMO_STARTED_AT = "2026-07-11T05:00:00.000000Z"

# A curated single-lane conversation: an agent claims a small task, understands
# the code, implements behind one flag with a test, and hands the lane back.
# Static data only -- writing it makes no model call. Timestamps ascend so the
# serve reader orders the turns chronologically.
CANNED_TRANSCRIPT: tuple[tuple[str, str], ...] = (
    (
        "2026-07-11T05:00:00.000000Z",
        "Picking up the task to add a `--porcelain` flag to `spice status`. "
        "Reading the status command to find where the human-readable table is "
        "assembled, so a machine-readable path can branch from the same data.",
    ),
    (
        "2026-07-11T05:01:00.000000Z",
        "Found it: `render_status` builds the table from a single `StatusReport`. "
        "I'll serialize that same report to newline-delimited JSON behind "
        "`--porcelain`, so the table and the JSON always read from one source and "
        "cannot drift.",
    ),
    (
        "2026-07-11T05:02:00.000000Z",
        "Implemented and covered: `spice status --porcelain` now prints one JSON "
        "object per lane, and `test_status_porcelain_matches_table` asserts the "
        "JSON rows carry the same lane ids the table renders. Full suite is green "
        "and the worktree is clean.",
    ),
    (
        "2026-07-11T05:03:00.000000Z",
        "Done -- `spice status --porcelain` ships behind one flag, with a single "
        "renderer feeding both views and a test pinning them together. Handing "
        "the lane back.",
    ),
)


@dataclass(frozen=True)
class DemoEnvironment:
    """Every path the demo seeded, all rooted under one throwaway directory."""

    root: Path
    repo_root: Path
    thread_id: str
    target_name: str
    driver_home: Path
    task_backend: Path
    transcript_path: Path


def configure_demo_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "demo",
        help="Launch a zero-setup preview of the serve UI from a canned transcript.",
        description=(
            "Seed an isolated, ephemeral demo lane from a canned transcript and "
            "open the serve UI against it: a zero-setup way to experience spice "
            "with no agent subscription and no model calls. Everything is written "
            "under a throwaway directory, so real worktrees and the task board "
            "stay untouched."
        ),
    )
    parser.add_argument(
        "--root",
        type=Path,
        metavar="DIR",
        help="Seed the demo under DIR instead of a fresh temporary directory.",
    )
    parser.add_argument(
        "--host",
        default=DEFAULT_SERVE_HOST,
        help=f"Serve bind address. Default: {DEFAULT_SERVE_HOST}.",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=DEFAULT_SERVE_PORT,
        help=f"Serve bind port. Default: {DEFAULT_SERVE_PORT}.",
    )
    parser.add_argument(
        "--seed-only",
        action="store_true",
        help=(
            "Seed the demo and print the serve command and URL without starting "
            "the server."
        ),
    )
    parser.set_defaults(func=run_demo, serve_action=None)


def run_demo(args: argparse.Namespace) -> int:
    env = seed_demo_environment(root=args.root)
    url = f"http://{args.host}:{args.port}"
    print(f"spice demo: seeded ephemeral demo lane at {env.repo_root}")
    print(f"spice demo: canned transcript -> {env.transcript_path}")
    print(f"spice demo: open {url}")
    if args.seed_only:
        print(
            "spice demo: start the server with:\n"
            f"  {demo_serve_command(env, host=args.host, port=args.port)}"
        )
        return 0
    # The demo boots serve in-process; serve resolves the lane transcript from
    # the driver home and its team store from the ephemeral task backend.
    os.environ["CLAUDE_CONFIG_DIR"] = str(env.driver_home)  # env-policy: allow
    os.chdir(env.repo_root)
    serve_args = argparse.Namespace(
        host=args.host,
        port=args.port,
        allow_insecure_bind=False,
        auth_token=None,
        until=None,
        task_backend=str(env.task_backend),
        serve_action=None,
    )
    return run_serve(serve_args)


def seed_demo_environment(root: Path | None = None) -> DemoEnvironment:
    """Materialize a self-contained demo lane under one throwaway directory."""
    demo_root = _prepare_demo_root(root)
    repo_root = demo_root
    _git_init_demo_repo(repo_root)
    set_scope_section(repo_root, WORKTREE_SOURCE, "agent", {"driver": "claude"})
    driver_home = demo_root / "driver-home"
    transcript_path = _write_canned_transcript(driver_home)
    task_backend = demo_root / "task-backend"
    task_backend.mkdir(parents=True, exist_ok=True)
    _write_demo_agent_state(repo_root)
    return DemoEnvironment(
        root=demo_root,
        repo_root=repo_root,
        thread_id=DEMO_THREAD_ID,
        target_name=repo_root.name,
        driver_home=driver_home,
        task_backend=task_backend,
        transcript_path=transcript_path,
    )


def demo_serve_command(env: DemoEnvironment, *, host: str, port: int) -> str:
    """Render the seed-only launch command with its repo anchor and environment."""
    serve = shlex.join(
        [
            "env",
            f"CLAUDE_CONFIG_DIR={env.driver_home}",
            "spice",
            "serve",
            "--task-backend",
            str(env.task_backend),
            "--host",
            host,
            "--port",
            str(port),
        ]
    )
    return f"cd {shlex.quote(str(env.repo_root))} && {serve}"


def _prepare_demo_root(root: Path | None) -> Path:
    if root is None:
        return Path(tempfile.mkdtemp(prefix="spice-demo-")).resolve()
    candidate = root.resolve()
    if candidate.exists():
        if not candidate.is_dir():
            raise SpiceError(f"spice demo root is not a directory: {candidate}")
        if any(candidate.iterdir()):
            raise SpiceError(
                "spice demo refuses to modify a nonempty directory; "
                f"choose a new or empty disposable root: {candidate}"
            )
        return candidate
    candidate.mkdir(parents=True)
    return candidate


def _git_init_demo_repo(repo_root: Path) -> None:
    run_git_command(["git", "init", "-q", "-b", "main"], cwd=repo_root, check=True)
    run_git_command(
        [
            "git",
            "-c",
            "user.email=demo@spice.invalid",
            "-c",
            "user.name=spice demo",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "spice demo seed",
        ],
        cwd=repo_root,
        check=True,
    )


def _write_canned_transcript(driver_home: Path) -> Path:
    projects_dir = driver_home / "projects" / DEMO_PROJECT_SLUG
    projects_dir.mkdir(parents=True, exist_ok=True)
    transcript = projects_dir / f"{dashed_uuid(DEMO_THREAD_ID)}.jsonl"
    lines = [
        json.dumps(
            {
                "type": "assistant",
                "timestamp": timestamp,
                "message": {
                    "role": "assistant",
                    "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": text}],
                },
            },
            separators=(",", ":"),
        )
        for timestamp, text in CANNED_TRANSCRIPT
    ]
    transcript.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return transcript


def _write_demo_agent_state(repo_root: Path) -> None:
    # An authoritative-but-stopped state (pid 0) binds the demo thread to the
    # lane so serve resolves and renders its transcript, while process_status
    # stays idle -- a completed conversation, no live agent.
    write_agent_state(
        repo_root,
        {
            "thread_id": DEMO_THREAD_ID,
            "mode": "demo",
            "started_at": DEMO_STARTED_AT,
            "prompt_skill_path": str(
                repo_root / ".agents" / "skills" / "spice" / "SKILL.md"
            ),
            "driver": "claude",
            "model": "claude-opus-4-8",
            "reasoning_effort": "",
            "service_tier": "",
            "pid": 0,
            "process_group_id": 0,
        },
    )
