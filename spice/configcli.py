"""`spice config` — show and set harness configuration."""

from __future__ import annotations

import argparse
import json
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from spice.cli.effects import (
    AuthoredInputInvocation,
    EffectRead,
    MutationDecision,
    mark_authored_input,
)
from spice.config import edit, layers, values
from spice.config.schema import validate_config_keys
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

    _configure_trust_parser(actions)

    set_action = actions.add_parser(
        "set",
        help="Set any schema-backed configuration leaf by dotted key.",
    )
    set_action.add_argument(
        "key",
        help=(
            "TOML dotted key; quote a segment inside the argument when its name "
            "contains a dot."
        ),
    )
    set_action.add_argument(
        "value",
        help=(
            "Value: true/false, a number, a TOML array or inline table, or bare "
            "text for a string."
        ),
    )
    _add_scope_argument(set_action)
    set_action.set_defaults(func=handle_config)

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


def _configure_trust_parser(actions: Any) -> None:
    from spice.config.trust import EXECUTABLE_REPOSITORY_CAPABILITIES

    trust = actions.add_parser(
        "trust",
        help="Inspect or authorize executable repository configuration.",
    )
    trust_actions = trust.add_subparsers(dest="trust_action", required=True)
    show = trust_actions.add_parser(
        "show",
        help="Show shared exact approvals and the active standing grant.",
    )
    show.set_defaults(func=handle_config)

    grant = trust_actions.add_parser(
        "grant",
        help="Preview a signed-provenance standing grant.",
    )
    grant.add_argument(
        "--path",
        action="append",
        choices=EXECUTABLE_REPOSITORY_CAPABILITIES,
        default=[],
        help=(
            "Executable capability to delegate; repeat as needed. The default "
            "is every capability currently defined by repository configuration."
        ),
    )
    grant.add_argument(
        "--signer",
        action="append",
        default=[],
        help="Trusted Git signature fingerprint; repeat for multiple signers.",
    )
    _add_trust_apply_arguments(grant)
    mark_authored_input(
        grant,
        AuthoredInputInvocation(
            reads=(
                EffectRead.AUTHORED_REPOSITORY,
                EffectRead.AUTHORED_CONFIGURATION,
                EffectRead.OWNERSHIP_RECEIPT,
            ),
            decision=MutationDecision.PREVIEW_APPLY,
            mutation_args=("--apply",),
        ),
    )
    grant.set_defaults(func=handle_config)

    revoke = trust_actions.add_parser(
        "revoke",
        help="Preview revocation of all exact and standing authority.",
    )
    revoke.add_argument(
        "--reason",
        default="operator revocation",
        help="Audit reason stored with the append-only authority revocation.",
    )
    _add_trust_apply_arguments(revoke)
    mark_authored_input(
        revoke,
        AuthoredInputInvocation(
            reads=(
                EffectRead.AUTHORED_REPOSITORY,
                EffectRead.OWNERSHIP_RECEIPT,
            ),
            decision=MutationDecision.PREVIEW_APPLY,
            mutation_args=("--apply",),
        ),
    )
    revoke.set_defaults(func=handle_config)


def _add_trust_apply_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--apply",
        nargs="?",
        const=True,
        metavar="PLAN_DIGEST",
        help=(
            "Append the common-Git-dir authority fact, optionally asserting "
            "the previewed plan digest."
        ),
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit the versioned authority plan as JSON without applying it.",
    )


def _add_scope_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--scope",
        choices=layers.CONFIG_SCOPE_NAMES,
        default=layers.WORKTREE_SOURCE,
        help="Configuration layer: system, repository, or worktree.",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help=(
            "Apply a system-scope change to the installed package; bare system "
            "writes only preview. Other scopes already apply directly."
        ),
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


def _handle_trust(args: argparse.Namespace, repo_root: Path) -> int:
    from spice.commandplan import assert_plan_digest
    from spice.config.trust import (
        apply_standing_trust_plan,
        plan_standing_repository_revocation,
        plan_standing_repository_trust,
        repository_config_approval,
        repository_config_trust_state,
        standing_trust_plan_rows,
    )
    from spice.config.trustpolicy import repository_trust_log_path

    if args.trust_action == "show":
        approval = repository_config_approval(repo_root)
        state = repository_config_trust_state(repo_root)
        print(
            json.dumps(
                _trust_state_payload(repo_root, state, approval),
                indent=2,
                sort_keys=True,
            )
        )
        return 0
    plan = (
        plan_standing_repository_trust(
            repo_root,
            capabilities=args.path,
            trusted_signers=args.signer,
        )
        if args.trust_action == "grant"
        else plan_standing_repository_revocation(repo_root, reason=args.reason)
    )
    apply_requested = args.apply is not None
    if apply_requested and args.json:
        raise SpiceError(
            "`spice config trust --apply` cannot be combined with `--json`"
        )
    if not apply_requested:
        if args.json:
            print(json.dumps(plan.payload, indent=2, sort_keys=True))
        else:
            print("\n".join(standing_trust_plan_rows(plan)))
        return 0
    expected = args.apply if isinstance(args.apply, str) else None
    assert_plan_digest(plan.payload, expected)
    apply_standing_trust_plan(plan)
    rows = standing_trust_plan_rows(plan)
    print(
        "\n".join(
            (*rows[:-1], f"authority-recorded={repository_trust_log_path(repo_root)}")
        )
    )
    return 0


def _trust_state_payload(
    repo_root: Path,
    state: Any,
    approval: Any,
) -> dict[str, Any]:
    from spice.config.trustpolicy import repository_trust_log_path

    grant = state.active_grant
    return {
        "authority_path": str(repository_trust_log_path(repo_root)),
        "record_count": state.record_count,
        "current": {
            "approved": approval.approved,
            "approved_digest": approval.approved_digest,
            "digest": approval.digest,
        },
        "exact_approvals": {
            capability: sorted(digests)
            for capability, digests in state.exact_approvals.items()
        },
        "active_grant": (
            {
                "grant_id": grant.grant_id,
                "repository_url": grant.repository_url,
                "remote": grant.remote,
                "ref": grant.ref,
                "anchor_commit": grant.anchor_commit,
                "capabilities": list(grant.capabilities),
                "trusted_signers": list(grant.trusted_signers),
                "delegated_approvals": {
                    capability: sorted(digests)
                    for capability, digests in state.delegated_approvals.items()
                },
            }
            if grant is not None
            else None
        ),
    }


def _handle_set(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    key_path = edit.parse_dotted_key(str(args.key))
    value = edit.parse_toml_value(str(args.value))
    _validate_set_leaf(repo_root, scope, key_path, value)
    if _preview_system_mutation(
        args,
        repo_root,
        f"set {'.'.join(key_path)}={json.dumps(value, sort_keys=True)}",
    ):
        return 0
    edit.set_scope_value(repo_root, scope, key_path, value)
    loaded = layers.load_config(repo_root)
    source = loaded.source_for(key_path)
    print(
        json.dumps(
            {
                "effective": _config_value_at(loaded.effective, key_path),
                "key": ".".join(key_path),
                "provenance": (
                    {"path": str(source.path), "scope": source.name}
                    if source is not None
                    else None
                ),
                "scope": scope,
                "value": _config_value_at(loaded.layer(scope).values, key_path),
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


def _handle_say(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        _validate_say_clear(repo_root, scope)
        if _preview_system_mutation(args, repo_root, "clear say"):
            return 0
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
    if _preview_system_mutation(
        args,
        repo_root,
        f"set say={json.dumps(section, sort_keys=True)}",
    ):
        return 0
    edit.set_scope_section(repo_root, scope, values.SAY_KEY, section)
    print(_say_config_summary(repo_root))
    return 0


def _handle_judge(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        keys = (values.JUDGE_BIN_KEY,)
        if scope == layers.WORKTREE_SOURCE:
            keys = (values.JUDGE_BIN_KEY, values.JUDGE_ENABLED_KEY)
        if _preview_system_mutation(args, repo_root, "clear judge"):
            return 0
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
    if _preview_system_mutation(
        args,
        repo_root,
        f"set judge={json.dumps(section, sort_keys=True)}",
    ):
        return 0
    edit.set_scope_section(repo_root, scope, values.JUDGE_KEY, section)
    print(f"judge_bin={values.configured_judge_bin(repo_root)}")
    print(f"judge_enabled={values.maxim_adjudication_enabled(repo_root)}")
    return 0


def _handle_agent(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        if _preview_system_mutation(args, repo_root, "clear agent"):
            return 0
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
    if _preview_system_mutation(
        args,
        repo_root,
        f"set agent={json.dumps(section, sort_keys=True)}",
    ):
        return 0
    edit.set_scope_section(repo_root, scope, values.AGENT_KEY, section)
    print(_agent_config_summary(repo_root))
    return 0


def _handle_personality(args: argparse.Namespace, repo_root: Path) -> int:
    scope = str(args.scope)
    if args.clear:
        if _preview_system_mutation(args, repo_root, "clear agent.personality"):
            return 0
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
    if _preview_system_mutation(
        args,
        repo_root,
        f"set agent.personality={json.dumps(str(args.value))}",
    ):
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


def _preview_system_mutation(
    args: argparse.Namespace,
    repo_root: Path,
    change: str,
) -> bool:
    """Render the install-wide mutation boundary unless explicitly applied."""
    if str(args.scope) != layers.SYSTEM_SOURCE or bool(getattr(args, "apply", False)):
        return False
    path = edit.config_scope_path(repo_root, layers.SYSTEM_SOURCE)
    rows = (
        f"configuration-plan scope=system action={args.config_action}",
        f"path={path}",
        f"change={change}",
        (
            "warning=system configuration modifies installed defaults for every "
            "repository and is lost on reinstall"
        ),
        "preview: no changes applied; pass --apply to execute",
    )
    print("\n".join(rows))
    return True


def _validate_set_leaf(
    repo_root: Path,
    scope: str,
    key_path: Sequence[str],
    value: Any,
) -> None:
    """Validate the requested schema path before rendering it as an executable plan."""
    candidate = value
    for part in reversed(key_path):
        candidate = {part: candidate}
    validate_config_keys(
        candidate,
        source_name=scope,
        source_path=edit.config_scope_path(repo_root, scope),
    )


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
    "trust": _handle_trust,
    "set": _handle_set,
    "say": _handle_say,
    "judge": _handle_judge,
    "agent": _handle_agent,
    "personality": _handle_personality,
}


def _config_value_at(values: Mapping[str, Any], path: Sequence[str]) -> Any:
    value: Any = values
    for part in path:
        if not isinstance(value, Mapping) or part not in value:
            return None
        value = value[part]
    return _json_config_value(value)


def _json_config_value(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _json_config_value(item) for key, item in value.items()}
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return [_json_config_value(item) for item in value]
    return value


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
