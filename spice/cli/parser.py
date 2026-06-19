"""Top-level argument parser: one subcommand per harness domain."""

from __future__ import annotations

import argparse

from spice.cli.recovery import RecoveringArgumentParser, set_recovery

# The reserved verb set: mounted repo commands may not shadow these, and the
# mount dispatcher short-circuits on them without reading any configuration.
# `init` and `dev` register together in `configure_dev_parser`.
BUILTIN_COMMANDS = (
    "agent",
    "task",
    "session",
    "serve",
    "maxim",
    "config",
    "study",
    "init",
    "dev",
)


def build_parser() -> argparse.ArgumentParser:
    parser = RecoveringArgumentParser(
        prog="spice",
        description=(
            "Simultaneous Production, Integration, and Control Environment "
            "for the enclosing repository: the agent command wrapper, inbox "
            "steering, worktree-bound agent lifecycle, the task control plane, "
            "session forensics, the supervisor web UI, maxim judging, "
            "code-health studies, and git hooks."
        ),
        epilog=_mounted_commands_epilog(),
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
    subparsers = parser.add_subparsers(dest="command", required=True)

    from spice.agent.cli import configure_agent_parser
    from spice.agent.maximcli import configure_maxim_parser
    from spice.configcli import configure_config_parser
    from spice.hooks.cli import configure_dev_parser
    from spice.serve.cli import configure_serve_parser
    from spice.sessions.cli import configure_session_parser
    from spice.studies.cli import configure_study_parser
    from spice.tasks.cli import configure_task_parser

    configure_agent_parser(subparsers)
    configure_task_parser(subparsers)
    configure_session_parser(subparsers)
    configure_serve_parser(subparsers)
    configure_maxim_parser(subparsers)
    configure_config_parser(subparsers)
    configure_study_parser(subparsers)
    configure_dev_parser(subparsers)
    return parser


def _mounted_commands_epilog() -> str | None:
    from spice.cli.mounts import mounted_command_names

    names = mounted_command_names()
    if not names:
        return None
    return "mounted commands (from [tool.spice.commands]): " + ", ".join(names)
