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

    system = actions.add_parser(
        "system",
        help="Print the effective agent configuration.",
    )
    system.set_defaults(func=handle_config)

    defaults_action = actions.add_parser(
        "defaults",
        help="Print the classification inventory for exported defaults.",
    )
    defaults_action.set_defaults(func=handle_config)

    say = actions.add_parser("say", help="Configure speech playback.")
    say.add_argument(
        "--backend",
        choices=config.SAY_BACKEND_CHOICES,
        help="Speech backend: macOS say or an external stdin/stdout command.",
    )
    say.add_argument(
        "--command",
        help="External speech command that reads text on stdin and writes audio to stdout.",
    )
    say.add_argument(
        "--content-type",
        help="Content-Type for external backend audio, such as audio/wav.",
    )
    say.add_argument("--voice", help="macOS `say` voice name.")
    say.add_argument("--words-per-minute", type=int)
    _add_scope_argument(say)
    say.add_argument("--clear", action="store_true")
    say.set_defaults(func=handle_config)

    judge = actions.add_parser("judge", help="Configure the maxim judge binary.")
    judge.add_argument("--bin", dest="judge_bin", help="Local LLM judge binary.")
    _add_scope_argument(judge)
    judge.add_argument("--clear", action="store_true")
    judge.set_defaults(func=handle_config)

    personality = actions.add_parser(
        "personality", help="Configure the agent personality."
    )
    personality.add_argument(
        "value", nargs="?", choices=config.AGENT_PERSONALITY_CHOICES
    )
    _add_scope_argument(personality)
    personality.add_argument("--clear", action="store_true")
    personality.set_defaults(func=handle_config)

    agent = actions.add_parser(
        "agent",
        help="Configure agent launch settings (driver, model, effort).",
    )
    agent.add_argument("--model", help="Model override for agent launches.")
    agent.add_argument("--effort", help="Reasoning effort for agent launches.")
    from spice.agent.driver import driver_choices

    agent.add_argument(
        "--driver",
        choices=driver_choices(),
        help="Agent CLI this worktree drives when SPICE_AGENT_DRIVER is unset.",
    )
    _add_scope_argument(agent)
    agent.add_argument("--clear", action="store_true")
    agent.set_defaults(func=handle_config)


def _add_scope_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--scope",
        choices=config.CONFIG_SCOPE_NAMES,
        default=config.WORKTREE_SOURCE,
        help="Configuration layer: system, pyproject, repository, or worktree.",
    )


def handle_config(args: argparse.Namespace) -> int:
    repo_root = require_repo_root()
    handler = _CONFIG_ACTIONS.get(args.config_action)
    if handler is None:
        raise SpiceError(f"unknown config action {args.config_action!r}")
    return handler(args, repo_root)


def _handle_show(args: argparse.Namespace, repo_root: Path) -> int:
    print(json.dumps(config.config_overview(repo_root), indent=2, sort_keys=True))
    return 0


def _handle_system(args: argparse.Namespace, repo_root: Path) -> int:
    print(json.dumps(config.agent_config_overview(repo_root), indent=2, sort_keys=True))
    return 0


def _handle_defaults(args: argparse.Namespace, repo_root: Path) -> int:
    _ = repo_root
    print(json.dumps(config.default_classifications(), indent=2, sort_keys=True))
    return 0


def _handle_say(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        config.clear_scope_section(
            repo_root, scope, config.SAY_KEY, keys=config.SAY_MUTABLE_KEYS
        )
        print(f"say {scope} config cleared")
        return 0
    values: dict[str, Any] = {}
    if args.backend:
        values[config.SAY_BACKEND_KEY] = args.backend
    if args.command and args.command.strip():
        values[config.SAY_COMMAND_KEY] = args.command.strip()
        values.setdefault(config.SAY_BACKEND_KEY, "external")
    if args.content_type and args.content_type.strip():
        values[config.SAY_CONTENT_TYPE_KEY] = args.content_type.strip()
    if args.voice and args.voice.strip():
        values[config.SAY_VOICE_KEY] = args.voice.strip()
    if args.words_per_minute is not None and args.words_per_minute <= 0:
        raise SpiceError(
            _scope_error(
                repo_root, scope, "say.words_per_minute must be a positive integer"
            )
        )
    if args.words_per_minute is not None:
        values[config.SAY_WORDS_PER_MINUTE_KEY] = args.words_per_minute
    if not values:
        print(_say_config_summary(repo_root))
        return 0
    _validate_say_config(repo_root, scope, values)
    config.set_scope_section(repo_root, scope, config.SAY_KEY, values)
    print(_say_config_summary(repo_root))
    return 0


def _handle_judge(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        config.clear_scope_section(
            repo_root, scope, config.JUDGE_KEY, keys=(config.JUDGE_BIN_KEY,)
        )
        print(f"judge {scope} config cleared")
        return 0
    if not args.judge_bin or not args.judge_bin.strip():
        raise SpiceError(_scope_error(repo_root, scope, "config judge requires --bin"))
    config.set_scope_section(
        repo_root,
        scope,
        config.JUDGE_KEY,
        {config.JUDGE_BIN_KEY: args.judge_bin.strip()},
    )
    print(f"judge_bin={config.configured_judge_bin(repo_root)}")
    return 0


def _handle_agent(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        config.clear_scope_section(
            repo_root, scope, config.AGENT_KEY, keys=config.AGENT_LAUNCH_KEYS
        )
        print(f"agent {scope} config cleared")
        return 0
    values: dict[str, str] = {}
    if args.model and args.model.strip():
        values[config.AGENT_MODEL_KEY] = args.model.strip()
    if args.effort and args.effort.strip():
        values[config.AGENT_EFFORT_KEY] = args.effort.strip()
    if getattr(args, "driver", None):
        values[config.AGENT_DRIVER_KEY] = str(args.driver)
    if not values:
        print(_agent_config_summary(repo_root))
        return 0
    config.set_scope_section(repo_root, scope, config.AGENT_KEY, values)
    print(_agent_config_summary(repo_root))
    return 0


def _handle_personality(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        config.clear_scope_section(
            repo_root,
            scope,
            config.AGENT_KEY,
            keys=(config.AGENT_PERSONALITY_KEY,),
        )
        print(f"personality {scope} config cleared")
        return 0
    if not args.value:
        print(f"personality={config.configured_agent_personality(repo_root)}")
        return 0
    config.set_scope_section(
        repo_root,
        scope,
        config.AGENT_KEY,
        {config.AGENT_PERSONALITY_KEY: args.value},
    )
    print(f"personality={args.value}")
    return 0


_CONFIG_ACTIONS = {
    "show": _handle_show,
    "system": _handle_system,
    "defaults": _handle_defaults,
    "say": _handle_say,
    "judge": _handle_judge,
    "agent": _handle_agent,
    "personality": _handle_personality,
}


def _agent_config_summary(repo_root: Path) -> str:
    effective = config.effective_agent_config(repo_root)
    return "\n".join(
        [
            *(
                _agent_scope_line(
                    scope, config.layer_table(repo_root, scope, config.AGENT_KEY)
                )
                for scope in config.CONFIG_SCOPE_NAMES
            ),
            _agent_scope_line("effective", effective),
        ]
    )


def _validate_say_config(repo_root: Path, scope: str, values: dict[str, Any]) -> None:
    candidate = config.layer_table(repo_root, scope, config.SAY_KEY)
    candidate.update(values)
    backend = str(
        candidate.get(config.SAY_BACKEND_KEY) or config.DEFAULT_SAY_BACKEND
    ).strip()
    command = str(candidate.get(config.SAY_COMMAND_KEY) or "").strip()
    command = command or config.configured_say_command(repo_root)
    if backend == "external" and not command:
        raise SpiceError(
            _scope_error(
                repo_root, scope, "config say --backend external requires --command"
            )
        )


def _say_config_summary(repo_root: Path) -> str:
    backend = config.configured_say_backend(repo_root)
    if backend == "external":
        command = config.configured_say_command(repo_root) or "-"
        content_type = config.configured_say_content_type(repo_root)
        return f"say backend=external command={command} content_type={content_type}"
    return f"say backend=say argv={' '.join(config.say_command_args(repo_root))}"


def _agent_scope_line(scope: str, values: dict[str, str]) -> str:
    return (
        f"agent {scope} "
        f"driver={values.get(config.AGENT_DRIVER_KEY) or '-'} "
        f"model={values.get(config.AGENT_MODEL_KEY) or '-'} "
        f"effort={values.get(config.AGENT_EFFORT_KEY) or '-'}"
    )


def _scope_error(repo_root: Path, scope: str, detail: str) -> str:
    path = config.config_scope_path(repo_root, scope)
    return f"{detail} (scope={scope} path={path})"
