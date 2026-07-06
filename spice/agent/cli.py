"""`spice agent` — run wrapper, lifecycle, activation, supervision."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from spice.agent.driver import DRIVER, POST_TOOL_HOOK_EVENT
from spice.errors import SpiceError
from spice.paths import require_repo_root


def configure_agent_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "agent",
        help="Start, resume, wrap, and inspect the agent bound to this worktree.",
    )
    actions = parser.add_subparsers(dest="agent_action", required=True)

    show = actions.add_parser("show", help="Show the bound agent's state.")
    show.set_defaults(func=handle_agent)

    status = actions.add_parser(
        "status",
        help="Compatibility alias for agent show.",
    )
    status.set_defaults(func=handle_agent)

    activation = actions.add_parser(
        "activation",
        help="Bind the ambient agent and print the activation packet.",
    )
    activation.set_defaults(func=handle_agent)

    requeue_deadletter = actions.add_parser(
        "requeue-deadletter",
        help="Move a deadlettered inbox item back to pending.",
    )
    requeue_deadletter.add_argument("key")
    requeue_deadletter.set_defaults(func=handle_agent)

    run = actions.add_parser(
        "run",
        help="Run an agent shell command with steering injection.",
    )
    run.add_argument("args", nargs=argparse.REMAINDER)
    run.set_defaults(func=handle_agent)

    post_tool_hook = actions.add_parser(
        "post-tool-hook",
        help="Emit pending steering as PostToolUse hook additional context.",
    )
    post_tool_hook.add_argument("--repo-root", default="")
    post_tool_hook.add_argument("--event-name", default=POST_TOOL_HOOK_EVENT)
    post_tool_hook.set_defaults(func=handle_agent)

    import_agent = actions.add_parser(
        "import",
        help="Bind an external agent (dashed or dashless UUID) to this worktree.",
    )
    import_agent.add_argument("uuid", metavar="UUID")
    import_agent.set_defaults(func=handle_agent)

    for verb, verb_help in (
        ("ack", "Accept steering keys (a reason is required for each)."),
        ("nack", "Refuse steering keys, with the reason why."),
    ):
        keyed = actions.add_parser(verb, help=verb_help)
        keyed.add_argument("keys", nargs="*", metavar="KEY")
        keyed.add_argument(
            "-m", "--message", default="", help="Reason recorded against the key(s)."
        )
        keyed.add_argument(
            "--stdin",
            action="store_true",
            help="Read 'KEY [reason]' lines from stdin; a bare KEY uses --message.",
        )
        keyed.set_defaults(func=handle_agent)

    ensure = actions.add_parser("ensure", help="Start or resume the worktree's agent.")
    ensure.add_argument("--dry-run", action="store_true")
    ensure.add_argument("--force-new", action="store_true")
    ensure.add_argument("--model", default="")
    ensure.add_argument("--effort", default="")
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
    if action == "post-tool-hook":
        repo_root = (
            Path(str(args.repo_root)).expanduser().resolve()
            if str(getattr(args, "repo_root", "") or "").strip()
            else require_repo_root()
        )
        response = render_post_tool_hook_response(
            repo_root, hook_event_name=str(args.event_name or POST_TOOL_HOOK_EVENT)
        )
        if response:
            print(response)
        return 0
    repo_root = require_repo_root()
    if action in {"show", "status"}:
        print(render_agent_status(lifecycle.agent_status(repo_root)))
        return 0
    if action == "activation":
        print(render_activation_packet(repo_root))
        return 0
    if action == "requeue-deadletter":
        from spice.mail.inbox import requeue_deadlettered_inbox_item

        path = requeue_deadlettered_inbox_item(repo_root, str(args.key))
        if path is None:
            raise SpiceError(f"deadlettered inbox item not found: {args.key}")
        print(f"requeued_deadletter key={path.stem} path={path}")
        return 0
    if action == "run":
        from spice.agent.wrap import run_agent_command

        return run_agent_command(
            repo_root,
            getattr(args, "args", []),
        )
    if action in {"ack", "nack"}:
        return _archive_steering_keys(repo_root, args)
    if action == "import":
        status = lifecycle.import_agent(repo_root, str(args.uuid))
        print(render_agent_status(status))
        return 0
    if action == "ensure":
        result = lifecycle.ensure_agent(
            repo_root,
            dry_run=bool(getattr(args, "dry_run", False)),
            force_new=bool(getattr(args, "force_new", False)),
            model=str(args.model),
            reasoning_effort=str(args.effort),
            personality=getattr(args, "personality", None),
            agent_bin=str(getattr(args, "agent_bin", "") or ""),
            fast_mode=bool(getattr(args, "fast_mode", False)),
        )
        print(render_ensure_result(result))
        return 0
    raise SpiceError(f"unknown agent action {action!r}")


def _steering_key_reasons(args: argparse.Namespace) -> list[tuple[str, str]]:
    """Collect (key, reason) pairs from stdin lines and positional keys.

    A reason is mandatory for every key: the agent may not retire steering
    without saying what it did with it. A stdin line is 'KEY [reason]'; a bare
    KEY (stdin or positional) inherits --message, and it is an error for a key
    to end up with no reason at all.
    """
    action = args.agent_action
    message = str(getattr(args, "message", "") or "").strip()
    pairs: list[tuple[str, str]] = []
    if getattr(args, "stdin", False):
        for lineno, raw in enumerate(sys.stdin.read().splitlines(), start=1):
            line = raw.strip()
            if not line:
                continue
            head, _, tail = line.partition(" ")
            reason = tail.strip() or message
            if not reason:
                raise SpiceError(
                    f"{action} stdin line {lineno} ({head!r}) needs a reason; "
                    "add it after the key or pass --message"
                )
            pairs.append((head, reason))
    for key in getattr(args, "keys", []) or []:
        if not message:
            raise SpiceError(f"{action} {key} requires a reason (--message)")
        pairs.append((key, message))
    return pairs


def _archive_steering_keys(repo_root: Path, args: argparse.Namespace) -> int:
    from spice.mail.acks import archive_ackd_inbox_items, archive_nackd_inbox_items
    from spice.mail.inbox import inbox_item_key_aliases

    action = args.agent_action
    pairs = _steering_key_reasons(args)
    if not pairs:
        raise SpiceError(f"{action} requires at least one key")
    keys = list(dict.fromkeys(key for key, _ in pairs))
    content_by_key = {key: reason for key, reason in pairs}
    label = action.upper()
    joined = "; ".join(f"{key}: {reason}" for key, reason in pairs)
    if action == "ack":
        retired = archive_ackd_inbox_items(
            repo_root,
            keys,
            ack_text=f"{label} {joined}",
            ack_content_by_key=content_by_key,
        )
    else:
        retired = archive_nackd_inbox_items(
            repo_root,
            keys,
            nack_text=f"{label} {joined}",
            nack_content_by_key=content_by_key,
        )
    retired_aliases: set[str] = set()
    for key in retired:
        retired_aliases |= inbox_item_key_aliases(key)
    for key in keys:
        matched = bool(inbox_item_key_aliases(key) & retired_aliases)
        print(f"{action} {key}: {'retired' if matched else 'no pending item matched'}")
    return 0


def render_agent_status(status: Any) -> str:
    lines = [
        f"worktree={status.repo_root}",
        f"status={status.process_status}",
        f"pid={status.pid or '-'}",
        f"pgid={status.process_group_id or '-'}",
        f"thread={status.thread_id or '-'}",
        (
            f"model={status.model or '-'} "
            f"effort={status.reasoning_effort or '-'} "
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


def render_post_tool_hook_response(
    repo_root: Path, *, hook_event_name: str = POST_TOOL_HOOK_EVENT
) -> str:
    from spice.agent.sidechannel import render_post_tool_hook_payload

    additional_context = render_post_tool_hook_payload(repo_root).strip()
    if not additional_context:
        return ""
    return json.dumps(
        {
            "hookSpecificOutput": {
                "hookEventName": hook_event_name or POST_TOOL_HOOK_EVENT,
                "additionalContext": additional_context,
            }
        },
        sort_keys=True,
    )
