"""`spice config` — show and set harness configuration."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from spice import config
from spice.errors import SpiceError
from spice.paths import require_repo_root


def configure_config_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser("config", help="Show and set harness configuration.")
    actions = parser.add_subparsers(dest="config_action", required=True)

    show = actions.add_parser("show", help="Print the current configuration.")
    show.set_defaults(func=handle_config)

    say = actions.add_parser("say", help="Configure speech voice and rate.")
    say.add_argument("--voice", help="macOS `say` voice name.")
    say.add_argument("--words-per-minute", type=int)
    say.add_argument("--clear", action="store_true")
    say.set_defaults(func=handle_config)

    judge = actions.add_parser("judge", help="Configure the maxim judge binary.")
    judge.add_argument("--bin", dest="judge_bin", help="Local LLM judge binary.")
    judge.add_argument("--clear", action="store_true")
    judge.set_defaults(func=handle_config)

    personality = actions.add_parser(
        "personality", help="Configure the agent personality."
    )
    personality.add_argument(
        "value", nargs="?", choices=config.AGENT_PERSONALITY_CHOICES
    )
    personality.add_argument("--clear", action="store_true")
    personality.set_defaults(func=handle_config)

    agent = actions.add_parser(
        "agent",
        help="Configure agent launch settings (driver, model, thinking).",
    )
    agent.add_argument("--model", help="Model override for agent launches.")
    agent.add_argument("--thinking", help="Thinking effort for agent launches.")
    agent.add_argument(
        "--driver",
        choices=config.AGENT_DRIVER_CHOICES,
        help="Agent CLI this worktree drives when SPICE_AGENT_DRIVER is unset.",
    )
    agent.add_argument(
        "--scope",
        choices=("worktree", "project"),
        default="worktree",
        help=(
            "Write the current worktree's local state or tracked project "
            "defaults in pyproject.toml."
        ),
    )
    agent.add_argument("--clear", action="store_true")
    agent.set_defaults(func=handle_config)


def handle_config(args: argparse.Namespace) -> int:
    repo_root = require_repo_root()
    action = args.config_action
    if action == "show":
        print(json.dumps(config.config_overview(repo_root), indent=2, sort_keys=True))
        return 0
    if action == "say":
        if args.clear:
            config.clear_section(repo_root, config.SAY_KEY)
            print("say config cleared")
            return 0
        values: dict[str, Any] = {}
        if args.voice and args.voice.strip():
            values[config.SAY_VOICE_KEY] = args.voice.strip()
        if args.words_per_minute and args.words_per_minute > 0:
            values[config.SAY_WORDS_PER_MINUTE_KEY] = args.words_per_minute
        if not values:
            raise SpiceError("config say requires --voice or --words-per-minute")
        config.update_section(repo_root, config.SAY_KEY, values)
        print(f"say={' '.join(config.say_command_args(repo_root))}")
        return 0
    if action == "judge":
        if args.clear:
            config.clear_section(repo_root, config.JUDGE_KEY)
            print("judge config cleared")
            return 0
        if not args.judge_bin or not args.judge_bin.strip():
            raise SpiceError("config judge requires --bin")
        config.update_section(
            repo_root, config.JUDGE_KEY, {config.JUDGE_BIN_KEY: args.judge_bin.strip()}
        )
        print(f"judge_bin={config.configured_judge_bin(repo_root)}")
        return 0
    if action == "agent":
        scope = str(args.scope)
        if args.clear:
            if scope == "project":
                config.clear_project_agent_config(repo_root)
            else:
                config.clear_section(repo_root, config.AGENT_KEY)
            print(f"agent {scope} config cleared")
            return 0
        values: dict[str, str] = {}
        if args.model and args.model.strip():
            values[config.AGENT_MODEL_KEY] = args.model.strip()
        if args.thinking and args.thinking.strip():
            values[config.AGENT_THINKING_KEY] = args.thinking.strip()
        if getattr(args, "driver", None):
            values[config.AGENT_DRIVER_KEY] = str(args.driver)
        if not values:
            print(_agent_config_summary(repo_root))
            return 0
        if scope == "project":
            config.update_project_agent_config(repo_root, values)
        else:
            config.update_section(repo_root, config.AGENT_KEY, values)
        print(_agent_config_summary(repo_root))
        return 0
    if action == "personality":
        if args.clear:
            config.clear_section(repo_root, config.AGENT_KEY)
            print("personality config cleared")
            return 0
        if not args.value:
            print(f"personality={config.configured_agent_personality(repo_root)}")
            return 0
        config.update_section(
            repo_root, config.AGENT_KEY, {config.AGENT_PERSONALITY_KEY: args.value}
        )
        print(f"personality={args.value}")
        return 0
    raise SpiceError(f"unknown config action {action!r}")


def _agent_config_summary(repo_root: Path) -> str:
    project = config.project_agent_config(repo_root)
    worktree = config.worktree_agent_config(repo_root)
    effective = config.effective_agent_config(repo_root)
    return "\n".join(
        [
            _agent_scope_line("project", project),
            _agent_scope_line("worktree", worktree),
            _agent_scope_line("effective", effective),
        ]
    )


def _agent_scope_line(scope: str, values: dict[str, str]) -> str:
    return (
        f"agent {scope} "
        f"driver={values.get(config.AGENT_DRIVER_KEY) or '-'} "
        f"model={values.get(config.AGENT_MODEL_KEY) or '-'} "
        f"thinking={values.get(config.AGENT_THINKING_KEY) or '-'}"
    )
