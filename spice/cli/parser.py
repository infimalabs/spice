"""Top-level argument parser: one subcommand per harness domain."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from typing import Literal

from spice.cli.recovery import RecoveringArgumentParser, set_recovery
from spice.version import runtime_version

# The reserved verb set: mounted repo commands may not shadow these, and the
# mount dispatcher short-circuits on them without reading any configuration.
# `init`, `deinit`, and `dev` register together in `configure_dev_parser`.
BUILTIN_COMMANDS = (
    "agent",
    "task",
    "session",
    "serve",
    "watch",
    "demo",
    "maxim",
    "config",
    "lock",
    "study",
    "doctor",
    "init",
    "deinit",
    "dev",
)


@dataclass(frozen=True)
class CommandPathRegistration:
    path: tuple[str, ...]
    source: Literal["builtin", "extension"]
    provider: str = ""


def build_parser(*, include_mounted_epilog: bool = True) -> argparse.ArgumentParser:
    parser = RecoveringArgumentParser(
        prog="spice",
        description=(
            "Simultaneous Production, Integration, and Control Environment "
            "for the enclosing repository: the agent command wrapper, inbox "
            "steering, worktree-bound agent lifecycle, the task control plane, "
            "session forensics, the supervisor web UI, maxim judging, "
            "code-health studies, and git hooks."
        ),
        epilog=_mounted_commands_epilog() if include_mounted_epilog else None,
    )
    set_recovery(
        parser,
        hints=("Choose one top-level command before passing command-specific flags.",),
        examples=(
            "spice task status",
            "spice session briefing",
            "spice serve --host 127.0.0.1 --port 8765",
        ),
    )
    parser.add_argument(
        "--worktree",
        metavar="TARGET",
        help=(
            "Run from a registered git worktree selected by branch, basename, or path."
        ),
    )
    parser.add_argument(
        "-V",
        "--version",
        action="version",
        version=f"%(prog)s {runtime_version()}",
        help="Show the installed Spice runtime version and exit.",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    from spice.agent.cli import configure_agent_parser
    from spice.agent.maximcli import configure_maxim_parser
    from spice.configcli import configure_config_parser
    from spice.doctor import configure_doctor_parser
    from spice.hooks.cli import configure_dev_parser
    from spice.resourcelocks import configure_lock_parser
    from spice.serve.cli import configure_serve_parser, configure_watch_parser
    from spice.serve.demo import configure_demo_parser
    from spice.sessions.cli import configure_session_parser
    from spice.studies.cli import configure_study_parser
    from spice.tasks.cli import configure_task_parser

    configure_agent_parser(subparsers)
    configure_task_parser(subparsers)
    configure_session_parser(subparsers)
    configure_serve_parser(subparsers)
    configure_watch_parser(subparsers)
    configure_demo_parser(subparsers)
    configure_maxim_parser(subparsers)
    configure_config_parser(subparsers)
    configure_lock_parser(subparsers)
    configure_study_parser(subparsers)
    configure_doctor_parser(subparsers)
    configure_dev_parser(subparsers)
    return parser


def command_path_registry() -> dict[tuple[str, ...], CommandPathRegistration]:
    parser = build_parser(include_mounted_epilog=False)
    registry = {
        path: CommandPathRegistration(path=path, source="builtin")
        for path in _parser_command_paths(parser)
    }
    from spice.studies.cli import extension_study_actions

    for entry in extension_study_actions():
        path = ("study", entry.name)
        registry[path] = CommandPathRegistration(
            path=path, source="extension", provider=entry.distribution
        )
    return registry


def _parser_command_paths(
    parser: argparse.ArgumentParser, prefix: tuple[str, ...] = ()
) -> set[tuple[str, ...]]:
    paths: set[tuple[str, ...]] = set()
    for action in parser._actions:
        if not isinstance(action, argparse._SubParsersAction):
            continue
        for name, child in action.choices.items():
            path = (*prefix, name)
            paths.add(path)
            paths.update(_parser_command_paths(child, path))
    return paths


def _mounted_commands_epilog() -> str | None:
    from spice.cli.mounts import mounted_command_names

    names = mounted_command_names()
    if not names:
        return None
    return "mounted commands (from [tool.spice.commands]): " + ", ".join(names)
