"""`spice agent` — run wrapper, lifecycle, activation, supervision."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
from typing import Any

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.paths import require_repo_root


def configure_agent_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "agent",
        help="Start, resume, wrap, and inspect the agent bound to this worktree.",
    )
    actions = parser.add_subparsers(dest="agent_action", required=True)

    status = actions.add_parser("status", help="Show the bound agent's state.")
    status.set_defaults(func=handle_agent)

    activation = actions.add_parser(
        "activation",
        help="Bind the ambient agent and print the activation packet.",
    )
    activation.set_defaults(func=handle_agent)

    run = actions.add_parser(
        "run",
        help="Run an agent shell command with proxy routing and steering injection.",
    )
    run.add_argument("args", nargs=argparse.REMAINDER)
    run.set_defaults(func=handle_agent)

    steer = actions.add_parser(
        "steer",
        help="Emit side-channel steering for shell startup hooks.",
    )
    steer.add_argument("--repo-root", default="")
    steer.add_argument("--watch", action="store_true")
    steer.add_argument("--parent-pid", type=int, default=0)
    steer.set_defaults(func=handle_agent)

    ensure = actions.add_parser("ensure", help="Start or resume the worktree's agent.")
    ensure.add_argument("--dry-run", action="store_true")
    ensure.add_argument("--force-new", action="store_true")
    ensure.add_argument("--model", default="")
    ensure.add_argument("--thinking", default="")
    ensure.add_argument("--personality")
    ensure.add_argument("--agent-bin", default="")
    ensure.add_argument("--fast-mode", action="store_true")
    ensure.set_defaults(func=handle_agent)

    supervise = actions.add_parser(
        "supervise",
        help="Run the durable agent watchdog/supervisor process.",
    )
    supervise.add_argument("--repo-root", required=True)
    supervise.add_argument("--action", required=True)
    supervise.add_argument("--model", required=True)
    supervise.add_argument("--reasoning-effort", required=True)
    supervise.add_argument("--service-tier", default="")
    supervise.add_argument("--resume-thread-id", default="")
    supervise.add_argument("--log-path", required=True)
    supervise.add_argument("--fast-mode", action="store_true")
    supervise.add_argument("--command-json", required=True)
    supervise.set_defaults(func=handle_agent)


def handle_agent(args: argparse.Namespace) -> int:
    from spice.agent import lifecycle

    action = args.agent_action
    if action == "supervise":
        return lifecycle.run_agent_supervisor(args)
    if action == "steer":
        from spice.agent.wrap import emit_agent_side_channel, watch_agent_side_channel

        raw_repo_root = str(getattr(args, "repo_root", "") or "")
        repo_root = (
            Path(raw_repo_root).expanduser().resolve()
            if raw_repo_root
            else require_repo_root()
        )
        if bool(getattr(args, "watch", False)):
            parent_pid = int(getattr(args, "parent_pid", 0) or os.getppid())
            watch_agent_side_channel(repo_root, parent_pid=parent_pid)
            return 0
        emit_agent_side_channel(repo_root)
        return 0
    repo_root = require_repo_root()
    if action == "status":
        print(render_agent_status(lifecycle.agent_status(repo_root)))
        return 0
    if action == "activation":
        print(render_activation_packet(repo_root))
        return 0
    if action == "run":
        from spice.agent.wrap import run_agent_command

        return run_agent_command(repo_root, getattr(args, "args", []))
    if action == "ensure":
        result = lifecycle.ensure_agent(
            repo_root,
            dry_run=bool(getattr(args, "dry_run", False)),
            force_new=bool(getattr(args, "force_new", False)),
            model=str(args.model),
            reasoning_effort=str(args.thinking),
            personality=getattr(args, "personality", None),
            agent_bin=str(getattr(args, "agent_bin", "") or ""),
            fast_mode=bool(getattr(args, "fast_mode", False)),
        )
        print(render_ensure_result(result))
        return 0
    raise SpiceError(f"unknown agent action {action!r}")


def render_agent_status(status: Any) -> str:
    lines = [
        f"worktree={status.repo_root}",
        f"status={status.process_status}",
        f"pid={status.pid or '-'}",
        f"pgid={status.process_group_id or '-'}",
        f"thread={status.thread_id or '-'}",
        (
            f"model={status.model or '-'} "
            f"thinking={status.reasoning_effort or '-'} "
            f"service_tier={status.service_tier or '-'}"
        ),
        f"started_at={status.started_at or '-'}",
        f"skill={status.prompt_skill_path or '-'}",
        f"log={status.log_path or '-'}",
    ]
    return "\n".join(lines)


def render_ensure_result(result: Any) -> str:
    lines = [
        f"action={result.action}",
        f"status={result.status.process_status}",
        f"pid={result.status.pid or '-'}",
        f"pgid={result.status.process_group_id or '-'}",
        f"thread={result.status.thread_id or '-'}",
        f"service_tier={result.status.service_tier or '-'}",
        f"prompt={result.prompt}",
    ]
    if result.log_path:
        lines.append(f"log={result.log_path}")
    if result.command:
        lines.append(
            "command=" + " ".join(shell_display_part(part) for part in result.command)
        )
    return "\n".join(lines)


def shell_display_part(value: str) -> str:
    if value and all(char.isalnum() or char in "./_=-:" for char in value):
        return value
    return repr(value)


def render_activation_packet(repo_root: Path) -> str:
    from spice.agent.activation import (
        activation_browser_validation_lines,
        activation_command_surface_lines,
        activation_git_hygiene_lines,
        activation_source_root_lines,
    )
    from spice.agent.lifecycle import (
        bind_ambient_agent_activation,
        materialize_worktree_skill,
    )
    from spice.hooks.install import install_hooks_for_repo
    from spice.tasks import gitsync

    status = bind_ambient_agent_activation(repo_root)
    hook_rows = install_hooks_for_repo(repo_root)
    skill = materialize_worktree_skill(repo_root)
    refresh = gitsync.fast_forward_if_safe(repo_root)
    return "\n".join(
        [
            "spice_agent_activation",
            f"worktree={repo_root.resolve()}",
            f"thread={status.thread_id or '-'}",
            f"driver={DRIVER.name}",
            "dev_hooks=configured",
            *(f"dev_hooks_detail={row}" for row in hook_rows),
            *((f"skill={skill}",) if skill else ()),
            *(f"baseline_refresh={note}" for note in refresh.notes),
            *activation_git_hygiene_lines(),
            *activation_source_root_lines(repo_root),
            *activation_browser_validation_lines(),
            *activation_command_surface_lines(),
        ]
    )
