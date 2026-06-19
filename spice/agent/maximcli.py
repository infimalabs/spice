"""CLI surface for the maxim adjudication primitive."""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Any

from spice.agent.maxims import (
    ALL_MAXIM,
    DEFAULT_PROMPT_TEMPLATE,
    META_MAXIMS,
    builtin_maxim,
    evaluate_maxim,
    resolved_maxim_bags,
    resolve_maxim,
    triggered_maxims,
)
from spice.errors import SpiceError

CONDITION_MET_EXIT_CODE = 0
CONDITION_UNMET_EXIT_CODE = 1
DEFAULT_OUTPUT_FORMAT = "{maxim}"


def configure_maxim_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "maxim",
        help="Judge statements against a maxim, or show the built-in maxims.",
        description=(
            "`agree`/`disagree` adjudicate one or more statements against a "
            "maxim using the local judge model; `show` prints the configured "
            "maxims so a short name can be piped straight into a verb."
        ),
    )
    actions = parser.add_subparsers(dest="maxim_action", required=True)

    agree = actions.add_parser(
        "agree",
        help="Judge whether statement(s) agree with a maxim.",
        description=(
            "Argument 1 is the maxim; the remaining arguments are statements. "
            "Every statement must agree (logical AND, short-circuiting on the "
            "first that does not). Exit code follows the predicate convention: "
            "0 when all agree, 1 when one disagrees, 2 on error."
        ),
    )
    _add_verdict_arguments(agree)
    agree.set_defaults(func=run_maxim_agree_cli)

    disagree = actions.add_parser(
        "disagree",
        help="Judge whether statement(s) disagree with a maxim (inverts agree).",
        description=(
            "The inverse of `agree`: argument 1 is the maxim, the rest are "
            "statements, and every statement must disagree (logical AND, "
            "short-circuiting on the first that agrees). Exit code: 0 when all "
            "disagree, 1 when one agrees, 2 on error."
        ),
    )
    _add_verdict_arguments(disagree)
    disagree.set_defaults(func=run_maxim_disagree_cli)

    show = actions.add_parser(
        "show",
        help="Show configured maxims; name one to print it for use in a verb.",
    )
    show.add_argument(
        "name",
        nargs="?",
        help="Short name (e.g. fallback, alias). Omit to list every configured maxim.",
    )
    show.set_defaults(func=run_maxim_show_cli)


def _add_verdict_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "maxim",
        help=(
            "The maxim to judge against. A built-in short name (e.g. fallback) "
            "expands to its full maxim; 'all'/'any' scan the statements for "
            "configured trigger words and judge each matched maxim; otherwise pass "
            "full maxim text."
        ),
    )
    parser.add_argument(
        "statements",
        nargs="+",
        help="One or more statements to judge against the maxim.",
    )
    parser.add_argument(
        "--prompt-file",
        type=Path,
        dest="prompt_file",
        help=(
            "Override the prompt template with the contents of this file. The "
            "template may reference the {maxim} and {statement} fields."
        ),
    )
    parser.add_argument(
        "--format",
        dest="output_format",
        help=(
            "Message printed for each matching statement; may reference "
            "{maxim} and {statement}. "
            f"Default: {DEFAULT_OUTPUT_FORMAT!r}."
        ),
    )
    parser.add_argument(
        "-q",
        "--quiet",
        action="store_true",
        help="Suppress the message and convey the verdict only through the exit code.",
    )


def run_maxim_agree_cli(args: argparse.Namespace) -> int:
    return _judge_statements(args, want_agreement=True)


def run_maxim_disagree_cli(args: argparse.Namespace) -> int:
    return _judge_statements(args, want_agreement=False)


def _judge_statements(args: argparse.Namespace, *, want_agreement: bool) -> int:
    template = _load_template(getattr(args, "prompt_file", None))
    quiet = getattr(args, "quiet", False)
    output_format = getattr(args, "output_format", None) or DEFAULT_OUTPUT_FORMAT
    statements = args.statements
    selector = args.maxim.strip().lower()
    if selector in META_MAXIMS:
        return _judge_triggered_maxims(
            statements,
            mode=selector,
            want_agreement=want_agreement,
            template=template,
            quiet=quiet,
            output_format=output_format,
        )
    maxim = resolve_maxim(args.maxim)
    offending = _first_break(
        maxim, statements, want_agreement=want_agreement, template=template
    )
    if offending is not None and not quiet:
        print(_format_message(output_format, maxim=maxim, statement=offending))
    return CONDITION_MET_EXIT_CODE if offending is None else CONDITION_UNMET_EXIT_CODE


def _judge_triggered_maxims(
    statements: list[str],
    *,
    mode: str,
    want_agreement: bool,
    template: str,
    quiet: bool,
    output_format: str,
) -> int:
    unmet_flags: list[bool] = []
    for bag in triggered_maxims(statements):
        maxim = bag.message
        offending = _first_break(
            maxim, statements, want_agreement=want_agreement, template=template
        )
        if offending is not None and not quiet:
            print(_format_message(output_format, maxim=maxim, statement=offending))
        unmet_flags.append(offending is not None)
    if not unmet_flags:
        return CONDITION_MET_EXIT_CODE
    unmet = any(unmet_flags) if mode == ALL_MAXIM else all(unmet_flags)
    return CONDITION_UNMET_EXIT_CODE if unmet else CONDITION_MET_EXIT_CODE


def _first_break(
    maxim: str, statements: list[str], *, want_agreement: bool, template: str
) -> str | None:
    """Return the first statement that breaks the verb's condition, or None."""
    for statement in statements:
        verdict = evaluate_maxim(maxim, statement, template=template)
        if verdict.agrees is not want_agreement:
            return statement
    return None


def _format_message(output_format: str, *, maxim: str, statement: str) -> str:
    try:
        return output_format.format(maxim=maxim, statement=statement)
    except (KeyError, IndexError) as exc:
        raise SpiceError(
            "output format may only reference the {maxim} and {statement} "
            f"fields; offending placeholder {exc}"
        ) from exc


def run_maxim_show_cli(args: argparse.Namespace) -> int:
    name = getattr(args, "name", None)
    if name:
        print(builtin_maxim(name))
    else:
        print(_render_maxim_listing())
    return 0


def _render_maxim_listing() -> str:
    rows = [
        (
            f"{name} ({'/'.join(_render_trigger_key(key) for key in sorted(bag.words))})",
            bag.message,
        )
        for name, bag in resolved_maxim_bags().items()
    ]
    width = max(len(name) for name, _ in rows)
    return "\n".join(f"{name.ljust(width)}  {text}" for name, text in rows)


def _render_trigger_key(key: str) -> str:
    return f'"{key}"' if " " in key else key


def _load_template(prompt_file: Path | None) -> str:
    if prompt_file is None:
        return DEFAULT_PROMPT_TEMPLATE
    try:
        text = prompt_file.read_text(encoding="utf-8")
    except OSError as exc:
        raise SpiceError(f"cannot read prompt file {prompt_file}: {exc}") from exc
    if not text.strip():
        raise SpiceError(f"prompt file {prompt_file} is empty")
    return text
