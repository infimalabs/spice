"""`spice config` — show and set harness configuration."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from spice.config import edit, layers, values
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
        choices=values.SAY_BACKEND_CHOICES,
        help="Speech backend: macOS say or an external stdin/stdout command.",
    )
    say.add_argument(
        "--command",
        help=(
            "External speech command that reads text on stdin and writes audio "
            "to stdout; a {words_per_minute} token in it receives the rate."
        ),
    )
    say.add_argument(
        "--content-type",
        help="Content-Type for external backend audio, such as audio/wav.",
    )
    say.add_argument("--voice", help="macOS `say` voice name.")
    say.add_argument(
        "--words-per-minute",
        type=int,
        help="Speech rate the UI multiplier scales, for either backend.",
    )
    _add_scope_argument(say)
    say.add_argument("--clear", action="store_true")
    say.set_defaults(func=handle_config)

    judge = actions.add_parser("judge", help="Configure the maxim judge.")
    judge.add_argument("--bin", dest="judge_bin", help="Local LLM judge binary.")
    _add_scope_argument(judge)
    adjudicate = judge.add_mutually_exclusive_group()
    adjudicate.add_argument(
        "--enable",
        dest="judge_enabled",
        action="store_true",
        default=None,
        help="Opt into judge adjudication of maxim trigger hits.",
    )
    adjudicate.add_argument(
        "--disable",
        dest="judge_enabled",
        action="store_false",
        default=None,
        help="Publish maxim trigger hits judge-free (the default).",
    )
    judge.add_argument("--clear", action="store_true")
    judge.set_defaults(func=handle_config)

    personality = actions.add_parser(
        "personality", help="Configure the agent personality."
    )
    personality.add_argument(
        "value", nargs="?", choices=values.AGENT_PERSONALITY_CHOICES
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
        choices=layers.CONFIG_SCOPE_NAMES,
        default=layers.WORKTREE_SOURCE,
        help="Configuration layer: system, repository, or worktree.",
    )


def handle_config(args: argparse.Namespace) -> int:
    repo_root = require_repo_root()
    handler = _CONFIG_ACTIONS.get(args.config_action)
    if handler is None:
        raise SpiceError(f"unknown config action {args.config_action!r}")
    return handler(args, repo_root)


def _handle_show(args: argparse.Namespace, repo_root: Path) -> int:
    print(json.dumps(values.config_overview(repo_root), indent=2, sort_keys=True))
    return 0


def _handle_system(args: argparse.Namespace, repo_root: Path) -> int:
    print(json.dumps(values.agent_config_overview(repo_root), indent=2, sort_keys=True))
    return 0


def _handle_defaults(args: argparse.Namespace, repo_root: Path) -> int:
    _ = repo_root
    print(json.dumps(values.default_classifications(), indent=2, sort_keys=True))
    return 0


def _handle_say(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        _validate_say_clear(repo_root, scope)
        edit.clear_scope_section(
            repo_root, scope, values.SAY_KEY, keys=values.SAY_MUTABLE_KEYS
        )
        print(f"say {scope} config cleared")
        return 0
    section: dict[str, Any] = {}
    if args.backend:
        section[values.SAY_BACKEND_KEY] = args.backend
    if args.command and args.command.strip():
        section[values.SAY_COMMAND_KEY] = args.command.strip()
        section.setdefault(values.SAY_BACKEND_KEY, "external")
    if args.content_type and args.content_type.strip():
        section[values.SAY_CONTENT_TYPE_KEY] = args.content_type.strip()
    if args.voice and args.voice.strip():
        section[values.SAY_VOICE_KEY] = args.voice.strip()
    if args.words_per_minute is not None and args.words_per_minute <= 0:
        raise SpiceError(
            _scope_error(
                repo_root, scope, "say.words_per_minute must be a positive integer"
            )
        )
    if args.words_per_minute is not None:
        section[values.SAY_WORDS_PER_MINUTE_KEY] = args.words_per_minute
    if not section:
        print(_say_config_summary(repo_root))
        return 0
    _validate_say_config(repo_root, scope, section)
    edit.set_scope_section(repo_root, scope, values.SAY_KEY, section)
    print(_say_config_summary(repo_root))
    return 0


def _handle_judge(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        keys = (values.JUDGE_BIN_KEY,)
        if scope == layers.WORKTREE_SOURCE:
            keys = (values.JUDGE_BIN_KEY, values.JUDGE_ENABLED_KEY)
        edit.clear_scope_section(repo_root, scope, values.JUDGE_KEY, keys=keys)
        print(f"judge {scope} config cleared")
        return 0
    section: dict[str, Any] = {}
    if args.judge_bin and args.judge_bin.strip():
        section[values.JUDGE_BIN_KEY] = args.judge_bin.strip()
    if args.judge_enabled is not None:
        if scope != layers.WORKTREE_SOURCE:
            raise SpiceError(
                _scope_error(
                    repo_root,
                    scope,
                    "config judge --enable and --disable are worktree-local",
                )
            )
        section[values.JUDGE_ENABLED_KEY] = args.judge_enabled
    if not section:
        raise SpiceError(
            _scope_error(
                repo_root,
                scope,
                "config judge requires --bin, --enable, or --disable",
            )
        )
    edit.set_scope_section(repo_root, scope, values.JUDGE_KEY, section)
    print(f"judge_bin={values.configured_judge_bin(repo_root)}")
    print(f"judge_enabled={values.maxim_adjudication_enabled(repo_root)}")
    return 0


def _handle_agent(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        edit.clear_scope_section(
            repo_root, scope, values.AGENT_KEY, keys=values.AGENT_LAUNCH_KEYS
        )
        print(f"agent {scope} config cleared")
        return 0
    section: dict[str, str] = {}
    if args.model and args.model.strip():
        section[values.AGENT_MODEL_KEY] = args.model.strip()
    if args.effort and args.effort.strip():
        section[values.AGENT_EFFORT_KEY] = args.effort.strip()
    if getattr(args, "driver", None):
        section[values.AGENT_DRIVER_KEY] = str(args.driver)
    if not section:
        print(_agent_config_summary(repo_root))
        return 0
    edit.set_scope_section(repo_root, scope, values.AGENT_KEY, section)
    print(_agent_config_summary(repo_root))
    return 0


def _handle_personality(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        edit.clear_scope_section(
            repo_root,
            scope,
            values.AGENT_KEY,
            keys=(values.AGENT_PERSONALITY_KEY,),
        )
        print(f"personality {scope} config cleared")
        return 0
    if not args.value:
        print(f"personality={values.configured_agent_personality(repo_root)}")
        print(_personality_driver_note(repo_root))
        return 0
    edit.set_scope_section(
        repo_root,
        scope,
        values.AGENT_KEY,
        {values.AGENT_PERSONALITY_KEY: args.value},
    )
    print(f"personality={args.value}")
    print(_personality_driver_note(repo_root))
    return 0


def _personality_driver_note(repo_root: Path) -> str:
    """Whether the driver this worktree launches acts on a set personality.

    Personality is a launch knob, and not every agent CLI has a flag to carry
    one. Saying so where the value is written keeps a setting that cannot take
    from reading as one that did.
    """
    from spice.agent.driver import PERSONALITY_LAUNCH_KNOB, driver_for

    driver = driver_for(repo_root)
    if PERSONALITY_LAUNCH_KNOB in driver.honored_launch_knobs:
        return f"driver={driver.name} carries personality into every launch"
    return (
        f"driver={driver.name} has no launch-time seam for personality, "
        "so this value does not reach the agent"
    )


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
    effective = values.effective_agent_config(repo_root)
    return "\n".join(
        [
            *(
                _agent_scope_line(
                    scope, layers.layer_table(repo_root, scope, values.AGENT_KEY)
                )
                for scope in layers.CONFIG_SCOPE_NAMES
            ),
            _agent_scope_line("effective", effective),
        ]
    )


def _validate_say_config(repo_root: Path, scope: str, section: dict[str, Any]) -> None:
    candidate = _prospective_say_config(repo_root, scope, section=section)
    _validate_say_candidate(repo_root, scope, candidate)


def _validate_say_clear(repo_root: Path, scope: str) -> None:
    candidate = _prospective_say_config(
        repo_root,
        scope,
        clear_keys=values.SAY_MUTABLE_KEYS,
        include_later=True,
    )
    _validate_say_candidate(repo_root, scope, candidate)


def _prospective_say_config(
    repo_root: Path,
    scope: str,
    *,
    section: dict[str, Any] | None = None,
    clear_keys: tuple[str, ...] = (),
    include_later: bool = False,
) -> dict[str, Any]:
    scopes = layers.CONFIG_SCOPE_NAMES
    stop = len(scopes) if include_later else scopes.index(scope) + 1
    effective: dict[str, Any] = {}
    for current_scope in scopes[:stop]:
        layer = layers.layer_table(repo_root, current_scope, values.SAY_KEY)
        if current_scope == scope:
            layer.update(section or {})
            for key in clear_keys:
                layer.pop(key, None)
        effective.update(layer)
    return effective


def _validate_say_candidate(
    repo_root: Path, scope: str, candidate: dict[str, Any]
) -> None:
    backend = str(
        candidate.get(values.SAY_BACKEND_KEY) or values.DEFAULT_SAY_BACKEND
    ).strip()
    command = str(candidate.get(values.SAY_COMMAND_KEY) or "").strip()
    if backend == "external" and not command:
        raise SpiceError(
            _scope_error(
                repo_root, scope, "config say --backend external requires --command"
            )
        )


def _say_config_summary(repo_root: Path) -> str:
    backend = values.configured_say_backend(repo_root)
    if backend == "external":
        command = values.configured_say_command(repo_root) or "-"
        content_type = values.configured_say_content_type(repo_root)
        return f"say backend=external command={command} content_type={content_type}"
    return f"say backend=say argv={' '.join(values.say_command_args(repo_root))}"


def _agent_scope_line(scope: str, section: dict[str, str]) -> str:
    return (
        f"agent {scope} "
        f"driver={section.get(values.AGENT_DRIVER_KEY) or '-'} "
        f"model={section.get(values.AGENT_MODEL_KEY) or '-'} "
        f"effort={section.get(values.AGENT_EFFORT_KEY) or '-'}"
    )


def _scope_error(repo_root: Path, scope: str, detail: str) -> str:
    path = edit.config_scope_path(repo_root, scope)
    return f"{detail} (scope={scope} path={path})"
