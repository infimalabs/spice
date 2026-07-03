"""CLI surface for the maxim adjudication primitive."""

from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

from spice.agent.maximmetrics import (
    MaximMetricCounts,
    maxim_metric_counts,
    maxim_metric_records,
    maxim_recurrence_counts,
)
from spice.agent.maxims import (
    ALL_MAXIM,
    DEFAULT_PROMPT_TEMPLATE,
    META_MAXIMS,
    MaximBag,
    FiledMaximProposalTask,
    MaximProposalDraft,
    MaximProposalSourceRecord,
    MaximProposalTheme,
    builtin_maxim,
    disabled_maxim_bag_names,
    evaluate_maxim,
    file_maxim_proposal_tasks,
    maxim_proposal_drafts,
    maxim_proposal_source_records,
    maxim_proposal_themes,
    render_maxim_proposal_draft_stanza,
    resolved_maxim_bags,
    resolve_maxim,
    set_maxim_bag_disabled,
    triggered_maxims,
)
from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd

CONDITION_MET_EXIT_CODE = 0
CONDITION_UNMET_EXIT_CODE = 1
DEFAULT_OUTPUT_FORMAT = "{maxim}"
SCOPE_DECISION_EVIDENCE_ROW = (
    "scope decisions: run `spice maxim report` and cite per-driver fire_rate, "
    "confirm_rate, and recurrence before editing "
    "[tool.spice.maxims.<bag>].drivers or using maxim disable/enable."
)
MAXIM_PROPOSAL_EVIDENCE_COMMENT_LIMIT = 8


@dataclass
class _MaximReportBucket:
    bag_name: str
    driver_name: str
    thread_id: str
    fire_count: int = 0
    judged_confirmed_count: int = 0
    judged_rejected_count: int = 0
    gate_suppressed_count: int = 0
    published_count: int = 0
    recurrence_count: int = 0


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

    report = actions.add_parser(
        "report",
        help="Show durable maxim metric counts for scope decisions.",
        description=(
            "Show per-bag, per-driver, per-thread maxim metric history. Cite "
            "this report before narrowing [tool.spice.maxims.<bag>].drivers "
            "or using worktree-local maxim disable/enable."
        ),
    )
    report.set_defaults(func=run_maxim_report_cli)

    sources = actions.add_parser(
        "sources",
        help="Show ACK ledger source records available for maxim proposal mining.",
    )
    sources.set_defaults(func=run_maxim_sources_cli)

    proposals = actions.add_parser(
        "proposals",
        help="Show TOML draft maxims from recurring ACK correction themes.",
        description=(
            "Cluster ACK-ledger correction sources into evidence-backed "
            "[tool.spice.maxims.<bag>] draft stanzas. This command only "
            "prints mergeable text; it does not edit repo config, install a "
            "bag, or call the maxim judge. Human triage remains mandatory."
        ),
    )
    proposals.set_defaults(func=run_maxim_proposals_cli)

    file_proposals = actions.add_parser(
        "file-proposals",
        help="File TOML draft maxims as hidden deferred triage tasks.",
        description=(
            "Mine recurring ACK-ledger correction sources, draft mergeable "
            "[tool.spice.maxims.<bag>] stanzas, and file each draft as a "
            "hidden deferred task for human triage. This command does not edit "
            "pyproject.toml, install a maxim bag, or call the maxim judge."
        ),
    )
    file_proposals.set_defaults(func=run_maxim_file_proposals_cli)

    disable = actions.add_parser(
        "disable",
        help="Disable one maxim bag for this git worktree.",
        description=(
            "Before disabling, run `spice maxim report` and cite per-driver "
            "fire_rate, confirm_rate, and recurrence evidence for the bag. "
            "The disable entry is worktree-local and stored outside tracked "
            "repo configuration. Other worktrees keep the bag enabled."
        ),
    )
    disable.add_argument("name", help="Configured maxim bag name to disable.")
    disable.set_defaults(func=run_maxim_disable_cli)

    enable = actions.add_parser(
        "enable",
        help="Re-enable one worktree-local disabled maxim bag.",
        description=(
            "Before re-enabling, run `spice maxim report` and cite per-driver "
            "fire_rate, confirm_rate, and recurrence evidence for the bag."
        ),
    )
    enable.add_argument("name", help="Configured maxim bag name to re-enable.")
    enable.set_defaults(func=run_maxim_enable_cli)

    disabled = actions.add_parser(
        "disabled",
        help="List maxim bags disabled in this git worktree.",
    )
    disabled.set_defaults(func=run_maxim_disabled_cli)


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


def run_maxim_report_cli(_args: argparse.Namespace) -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        raise SpiceError("not inside a git worktree")
    print(render_maxim_report(repo_root))
    return 0


def run_maxim_sources_cli(_args: argparse.Namespace) -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        raise SpiceError("not inside a git worktree")
    print(render_maxim_sources(maxim_proposal_source_records(repo_root)))
    return 0


def run_maxim_proposals_cli(_args: argparse.Namespace) -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        raise SpiceError("not inside a git worktree")
    records = maxim_proposal_source_records(repo_root)
    print(
        render_maxim_proposals(
            maxim_proposal_themes(records),
            existing_bags=resolved_maxim_bags(repo_root),
        )
    )
    return 0


def run_maxim_file_proposals_cli(_args: argparse.Namespace) -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        raise SpiceError("not inside a git worktree")
    records = maxim_proposal_source_records(repo_root)
    drafts = maxim_proposal_drafts(
        maxim_proposal_themes(records),
        existing_bags=resolved_maxim_bags(repo_root),
    )
    print(render_filed_maxim_proposal_tasks(file_maxim_proposal_tasks(drafts)))
    return 0


def run_maxim_disable_cli(args: argparse.Namespace) -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        raise SpiceError("not inside a git worktree")
    disabled = set_maxim_bag_disabled(args.name, disabled=True, repo_root=repo_root)
    print(_render_disabled_bags(disabled))
    print(_render_scope_decision_evidence_hint())
    return 0


def run_maxim_enable_cli(args: argparse.Namespace) -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        raise SpiceError("not inside a git worktree")
    disabled = set_maxim_bag_disabled(args.name, disabled=False, repo_root=repo_root)
    print(_render_disabled_bags(disabled))
    print(_render_scope_decision_evidence_hint())
    return 0


def run_maxim_disabled_cli(_args: argparse.Namespace) -> int:
    print(_render_disabled_bags(disabled_maxim_bag_names()))
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


def render_maxim_report(repo_root: Path) -> str:
    records = maxim_metric_records(repo_root)
    if not records:
        return "\n".join(
            ["maxim metric events: 0", _render_scope_decision_evidence_hint()]
        )
    buckets = _maxim_report_buckets(maxim_metric_counts(repo_root))
    recurrence_by_key = Counter()
    for item in maxim_recurrence_counts(repo_root):
        recurrence_by_key[(item.bag_name, item.driver_name, item.thread_id)] += (
            item.recurrence_count
        )
    for key, recurrence_count in recurrence_by_key.items():
        if key in buckets:
            buckets[key].recurrence_count = recurrence_count
    fire_totals_by_driver_thread = Counter()
    for bucket in buckets.values():
        fire_totals_by_driver_thread[(bucket.driver_name, bucket.thread_id)] += (
            bucket.fire_count
        )
    rows = sorted(
        (
            _maxim_report_row(
                bucket,
                total_driver_thread_fires=fire_totals_by_driver_thread[
                    (bucket.driver_name, bucket.thread_id)
                ],
            )
            for bucket in buckets.values()
        ),
        key=lambda row: (row[0], row[1], row[2]),
    )
    headers = (
        "bag",
        "driver",
        "thread",
        "fire_rate",
        "confirm_rate",
        "recurrence",
        "fire",
        "confirmed",
        "rejected",
        "suppressed",
        "published",
        "recur",
    )
    widths = [
        max(len(str(row[index])) for row in [headers, *rows])
        for index in range(len(headers))
    ]
    rendered = [
        " ".join(str(value).ljust(widths[index]) for index, value in enumerate(headers))
    ]
    rendered.extend(
        " ".join(str(value).ljust(widths[index]) for index, value in enumerate(row))
        for row in rows
    )
    return "\n".join(
        [
            f"maxim metric events: {len(records)}",
            *rendered,
            _render_scope_decision_evidence_hint(),
        ]
    )


def render_maxim_sources(records: tuple[MaximProposalSourceRecord, ...]) -> str:
    if not records:
        return "maxim proposal sources: 0"
    rows = ["maxim proposal sources: " + str(len(records)), "key disposition evidence"]
    rows.extend(
        f"{record.key} {record.disposition} {_render_source_evidence(record)}"
        for record in records
    )
    return "\n".join(rows)


def render_maxim_proposals(
    themes: tuple[MaximProposalTheme, ...],
    *,
    existing_bags: Mapping[str, MaximBag] | None = None,
) -> str:
    drafts = maxim_proposal_drafts(themes, existing_bags=existing_bags)
    if not drafts:
        return "# maxim proposals: 0"
    rows = ["# maxim proposals: " + str(len(drafts))]
    for index, draft in enumerate(drafts):
        if index:
            rows.append("")
        rows.extend(_render_maxim_proposal_draft(draft))
    return "\n".join(rows)


def _maxim_report_buckets(
    counts: list[MaximMetricCounts],
) -> dict[tuple[str, str, str], _MaximReportBucket]:
    buckets: dict[tuple[str, str, str], _MaximReportBucket] = {}
    for count in counts:
        buckets[(count.bag_name, count.driver_name, count.thread_id)] = (
            _MaximReportBucket(
                bag_name=count.bag_name,
                driver_name=count.driver_name,
                thread_id=count.thread_id,
                fire_count=count.fire_count,
                judged_confirmed_count=count.judged_confirmed_count,
                judged_rejected_count=count.judged_rejected_count,
                gate_suppressed_count=count.gate_suppressed_count,
                published_count=count.published_count,
            )
        )
    return buckets


def _render_disabled_bags(names: frozenset[str]) -> str:
    if not names:
        return "disabled maxim bags: none"
    return "disabled maxim bags: " + ", ".join(sorted(names))


def _render_scope_decision_evidence_hint() -> str:
    return SCOPE_DECISION_EVIDENCE_ROW


def _maxim_report_row(
    bucket: _MaximReportBucket, *, total_driver_thread_fires: int
) -> tuple[str, str, str, str, str, str, int, int, int, int, int, int]:
    judged_count = bucket.judged_confirmed_count + bucket.judged_rejected_count
    return (
        bucket.bag_name,
        bucket.driver_name,
        bucket.thread_id or "-",
        _format_percent(bucket.fire_count, total_driver_thread_fires),
        _format_percent(bucket.judged_confirmed_count, judged_count),
        _format_percent(bucket.recurrence_count, bucket.published_count),
        bucket.fire_count,
        bucket.judged_confirmed_count,
        bucket.judged_rejected_count,
        bucket.gate_suppressed_count,
        bucket.published_count,
        bucket.recurrence_count,
    )


def _format_percent(numerator: int, denominator: int) -> str:
    if denominator <= 0:
        return "-"
    return f"{numerator / denominator:.0%}"


def _render_source_evidence(record: MaximProposalSourceRecord) -> str:
    fields = [item.field for item in record.evidence]
    return ",".join(fields) if fields else "-"


def _render_maxim_proposal_draft(draft: MaximProposalDraft) -> list[str]:
    rows = [
        f"# theme = {draft.theme_name}",
        f"# evidence_count = {draft.evidence_count}",
        f"# dispositions = {_render_proposal_dispositions(draft)}",
        f"# source_keys = {','.join(draft.source_keys)}",
    ]
    rows.extend(_render_proposal_evidence_comments(draft.evidence))
    rows.extend(render_maxim_proposal_draft_stanza(draft).splitlines())
    return rows


def _render_proposal_evidence_comments(
    evidence: tuple[Any, ...],
) -> list[str]:
    rows = [
        f"# evidence {index} {item.field}: {_render_toml_comment(item.text)}"
        for index, item in enumerate(
            evidence[:MAXIM_PROPOSAL_EVIDENCE_COMMENT_LIMIT], start=1
        )
    ]
    omitted = len(evidence) - MAXIM_PROPOSAL_EVIDENCE_COMMENT_LIMIT
    if omitted > 0:
        rows.append(f"# evidence omitted = {omitted}")
    return rows


def _render_proposal_dispositions(
    proposal: MaximProposalTheme | MaximProposalDraft,
) -> str:
    return ",".join(
        f"{item.disposition}={item.count}" for item in proposal.dispositions
    )


def render_filed_maxim_proposal_tasks(
    filed: tuple[FiledMaximProposalTask, ...],
) -> str:
    if not filed:
        return "filed maxim proposal tasks: 0"
    rows = ["filed maxim proposal tasks: " + str(len(filed))]
    rows.extend(f"{item.handle} {item.project} {item.bag_name}" for item in filed)
    return "\n".join(rows)


def _render_toml_comment(value: str) -> str:
    return " ".join(value.split())


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
