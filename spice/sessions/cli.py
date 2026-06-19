"""`spice session` — transcript forensics: briefing, slices, ledgers, replay.

The no-arg invocation renders the briefing for the ambient agent's own
transcript: the primary rehydration product. Subcommands slice the same
records differently; every input is a transcript path or a thread id.
"""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Any

from spice.agent.driver import driver_for_transcript
from spice.agent.identity import canonical_thread_id
from spice.sessions import analysis
from spice.sessions import commandaudit, commandrecords
from spice.sessions import records
from spice.sessions import slices as session_slices
from spice.sessions.briefing import (
    DEFAULT_BRIEFING_MAX_BYTES,
    DEFAULT_BRIEFING_MAX_LINES,
    clip,
    render_briefing,
    render_sweep,
)
from spice.sessions.meter import (
    collect_context_meter,
    context_meter_instruction,
)
from spice.sessions.resolve import resolve_files, resolve_thread_transcript
from spice.sessions.util import format_int, normalize_timestamp

DEFAULT_SWEEP_WINDOWS = 4
DEFAULT_SUMMARY_RECENT = 8
DEFAULT_TIMELINE_LIMIT = 50
DEFAULT_TIMELINE_TEXT_CHARS = 180
DEFAULT_TURNS_LIMIT = 20
DEFAULT_COMPACTIONS_LIMIT = 25
DEFAULT_SLICES_LIMIT = 25
DEFAULT_SLICE_TEXT_CHARS = 180
DEFAULT_PHASE_EXAMPLES = 2
DEFAULT_PHASE_TEXT_CHARS = 180
DEFAULT_MESSAGES_LIMIT = 80
DEFAULT_MESSAGE_TEXT_CHARS = 180
DEFAULT_COMMITS_LIMIT = 25
DEFAULT_COMMANDS_LIMIT = 80
DEFAULT_COMMAND_TEXT_CHARS = 220
COMMIT_LINE_PREVIEW_CHARS = 160
COMMIT_USER_PREVIEW_CHARS = 120


def configure_session_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "session",
        help="Transcript forensics: briefing, summary, tokens, turns, sweep.",
    )
    actions = parser.add_subparsers(dest="session_action")
    parser.set_defaults(func=handle_session, session_action=None)
    _configure_briefing_parsers(actions)
    _configure_turn_parsers(actions)
    _configure_log_parsers(actions)


def _configure_briefing_parsers(actions: Any) -> None:
    briefing = actions.add_parser(
        "briefing",
        help="Rehydration briefing (the no-arg default).",
        recovery_examples=(
            "spice session briefing --max-lines 120 --explain-pruning",
            "spice session briefing THREAD_ID --contains validation",
        ),
    )
    _add_files_argument(briefing)
    _add_filter_arguments(briefing)
    _add_budget_arguments(briefing)
    briefing.set_defaults(func=handle_session)

    sweep = actions.add_parser(
        "sweep",
        help="Briefings across the last N compaction windows.",
        recovery_examples=("spice session sweep --count 4 --contains validation",),
    )
    _add_files_argument(sweep)
    _add_filter_arguments(sweep)
    sweep.add_argument("--count", type=int, default=DEFAULT_SWEEP_WINDOWS)
    sweep.set_defaults(func=handle_session)

    summary = actions.add_parser(
        "summary",
        help="Counts plus recent dialogue.",
        recovery_examples=("spice session summary --recent 8",),
    )
    _add_files_argument(summary)
    summary.add_argument("--recent", type=int, default=DEFAULT_SUMMARY_RECENT)
    summary.set_defaults(func=handle_session)

    tokens = actions.add_parser(
        "tokens",
        help="Cumulative token ledger.",
        recovery_examples=("spice session tokens --by-file",),
    )
    _add_files_argument(tokens)
    tokens.add_argument("--by-file", action="store_true")
    tokens.set_defaults(func=handle_session)


def _configure_turn_parsers(actions: Any) -> None:
    timeline = actions.add_parser(
        "timeline",
        help="Chronological turns and compactions.",
        recovery_examples=(
            "spice session timeline --limit 20 --max-text 120",
            "spice session timeline --turn-id TURN_ID --tool exec_command",
        ),
    )
    _add_files_argument(timeline)
    timeline.add_argument("--start", help="UTC/ISO timestamp lower bound.")
    timeline.add_argument("--end", help="UTC/ISO timestamp upper bound.")
    timeline.add_argument("--contains", help="Case-insensitive rendered-text filter.")
    timeline.add_argument("--turn-id", action="append", dest="turn_ids")
    timeline.add_argument("--tool", action="append", dest="tools")
    timeline.add_argument("--limit", type=int, default=DEFAULT_TIMELINE_LIMIT)
    timeline.add_argument("--max-text", type=int, default=DEFAULT_TIMELINE_TEXT_CHARS)
    timeline.set_defaults(func=handle_session)

    turns = actions.add_parser(
        "turns",
        help="Render turns as dialogue.",
        recovery_examples=("spice session turns --limit 5 --view full",),
    )
    _add_files_argument(turns)
    _add_filter_arguments(turns)
    turns.add_argument("--limit", type=int, default=DEFAULT_TURNS_LIMIT)
    turns.add_argument("--view", choices=("dialogue", "full"), default="dialogue")
    turns.set_defaults(func=handle_session)

    compactions = actions.add_parser(
        "compactions",
        help="Context compactions with surrounding prose.",
        recovery_examples=("spice session compactions --limit 10",),
    )
    _add_files_argument(compactions)
    compactions.add_argument("--limit", type=int, default=DEFAULT_COMPACTIONS_LIMIT)
    compactions.set_defaults(func=handle_session)

    slices = actions.add_parser(
        "slices",
        help="Compaction-bounded recovery slices.",
        recovery_examples=("spice session slices --limit 5 --view full",),
    )
    _add_files_argument(slices)
    slices.add_argument("--limit", type=int, default=DEFAULT_SLICES_LIMIT)
    slices.add_argument(
        "--slice-id",
        action="append",
        default=[],
        help="Only print a specific derived slice id; repeat to select multiple.",
    )
    slices.add_argument("--view", choices=("summary", "full"), default="summary")
    slices.add_argument("--max-text", type=int, default=DEFAULT_SLICE_TEXT_CHARS)
    slices.set_defaults(func=handle_session)

    phases = actions.add_parser(
        "phases",
        help="Segment turns into contiguous working phases.",
        recovery_examples=("spice session phases --limit 3 --examples 1",),
    )
    _add_files_argument(phases)
    _add_filter_arguments(phases)
    phases.add_argument("--limit", type=int)
    phases.add_argument("--examples", type=int, default=DEFAULT_PHASE_EXAMPLES)
    phases.add_argument("--max-text", type=int, default=DEFAULT_PHASE_TEXT_CHARS)
    phases.set_defaults(func=handle_session)


def _configure_log_parsers(actions: Any) -> None:
    messages = actions.add_parser(
        "messages",
        help="Print individual user/assistant messages with phase and flavor filters.",
        recovery_examples=(
            "spice session messages --side assistant --limit 5",
            "spice session messages --phase-kind final_answer --oldest-first",
        ),
    )
    _add_files_argument(messages)
    messages.add_argument("--start", help="UTC/ISO timestamp lower bound.")
    messages.add_argument("--end", help="UTC/ISO timestamp upper bound.")
    messages.add_argument("--contains", help="Case-insensitive text filter.")
    messages.add_argument("--turn-id", action="append", dest="turn_ids")
    messages.add_argument("--side", action="append", choices=("user", "assistant"))
    messages.add_argument(
        "--phase-kind",
        action="append",
        dest="phase_kinds",
        choices=("prompt", "commentary", "final_answer"),
    )
    messages.add_argument("--flavor", action="append", dest="flavors")
    messages.add_argument("--limit", type=int, default=DEFAULT_MESSAGES_LIMIT)
    messages.add_argument("--oldest-first", action="store_true")
    messages.add_argument("--max-text", type=int, default=DEFAULT_MESSAGE_TEXT_CHARS)
    messages.set_defaults(func=handle_session)

    commits = actions.add_parser(
        "commits",
        help="Commit declarations harvested from assistant prose.",
        recovery_examples=("spice session commits --limit 10",),
    )
    _add_files_argument(commits)
    commits.add_argument("--limit", type=int, default=DEFAULT_COMMITS_LIMIT)
    commits.set_defaults(func=handle_session)

    commands = actions.add_parser(
        "commands",
        help="Completed shell commands with wrapper/pipeline audit.",
        recovery_examples=(
            "spice session commands --summary",
            "spice session commands --failed --newest-first",
        ),
    )
    _add_files_argument(commands)
    commands.add_argument("--limit", type=int, default=DEFAULT_COMMANDS_LIMIT)
    commands.add_argument("--newest-first", action="store_true")
    commands.add_argument(
        "--since-compaction",
        action="store_true",
        help="Only include commands after the latest compaction.",
    )
    commands.add_argument(
        "--failed", action="store_true", help="Only include nonzero-exit commands."
    )
    commands.add_argument(
        "--pipelines", action="store_true", help="Only include shell pipelines."
    )
    commands.add_argument(
        "--noncanonical-pipelines",
        dest="noncanonical_pipelines",
        action="store_true",
        help="Only include pipelines whose segments are not all wrapper-launched.",
    )
    commands.add_argument(
        "--summary",
        action="store_true",
        help="Print command and wrapper/pipeline counters instead of rows.",
    )
    commands.add_argument("--max-text", type=int, default=DEFAULT_COMMAND_TEXT_CHARS)
    commands.set_defaults(func=handle_session)

    thread = actions.add_parser(
        "thread",
        help="Resolve one agent thread and summarize latest activity.",
        recovery_examples=("spice session thread THREAD_ID",),
    )
    thread.add_argument("thread_id")
    thread.set_defaults(func=handle_session)


def _add_files_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "files",
        nargs="*",
        help="Transcript paths or thread ids; defaults to the ambient agent.",
    )


def _add_filter_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--start", help="UTC/ISO timestamp lower bound.")
    parser.add_argument("--end", help="UTC/ISO timestamp upper bound.")
    parser.add_argument("--contains", help="Case-insensitive text filter.")
    parser.add_argument("--turn-id", action="append", dest="turn_ids")
    parser.add_argument("--tool", action="append", dest="tools")


def _add_budget_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--max-lines", type=int, default=DEFAULT_BRIEFING_MAX_LINES)
    parser.add_argument("--max-bytes", type=int, default=DEFAULT_BRIEFING_MAX_BYTES)
    parser.add_argument("--explain-pruning", action="store_true")


def handle_session(args: argparse.Namespace) -> int:
    action = args.session_action or "briefing"
    if action == "thread":
        print(render_thread_summary(str(args.thread_id)))
        return 0
    files = resolve_files(list(getattr(args, "files", []) or []))
    if action == "briefing":
        print(
            render_briefing(
                files,
                **_filter_kwargs(args),
                max_lines=max(
                    1, int(getattr(args, "max_lines", DEFAULT_BRIEFING_MAX_LINES))
                ),
                max_bytes=max(
                    1, int(getattr(args, "max_bytes", DEFAULT_BRIEFING_MAX_BYTES))
                ),
                explain_pruning=bool(getattr(args, "explain_pruning", False)),
            )
        )
        return 0
    if action == "sweep":
        print(render_sweep(files, count=max(1, args.count), **_filter_kwargs(args)))
        return 0
    if action == "summary":
        _print_summary(files, recent=max(1, args.recent))
        return 0
    if action == "tokens":
        _print_tokens(files, by_file=bool(args.by_file))
        return 0
    if action == "timeline":
        _print_timeline(
            files,
            start=_normalize_bound(args.start),
            end=_normalize_bound(args.end),
            contains=getattr(args, "contains", None),
            turn_ids=_clean_list(getattr(args, "turn_ids", None)),
            tools=_clean_list(getattr(args, "tools", None)),
            limit=max(1, args.limit),
            max_text=max(1, args.max_text),
        )
        return 0
    if action == "turns":
        _print_turns(args, files)
        return 0
    if action == "compactions":
        _print_compactions(files, limit=max(1, args.limit))
        return 0
    if action == "slices":
        _print_slices(args, files)
        return 0
    if action == "phases":
        _print_phases(args, files)
        return 0
    if action == "messages":
        _print_messages(args, files)
        return 0
    if action == "commits":
        _print_commits(files, limit=max(1, args.limit))
        return 0
    if action == "commands":
        _print_commands(args, files)
        return 0
    raise SystemExit(f"unknown session action {action!r}")


def render_thread_summary(thread_id: str) -> str:
    canonical = canonical_thread_id(thread_id)
    display_id = canonical or thread_id.strip()
    try:
        transcript = resolve_thread_transcript(display_id)
    except SystemExit as exc:
        raise SystemExit(f"Could not resolve thread {display_id}: {exc}") from exc
    driver = driver_for_transcript(transcript)
    turns = records.collect_turns([transcript])
    compactions = records.collect_compactions([transcript])
    meter = collect_context_meter([transcript])
    latest_turn = _latest_activity_turn(turns)
    lines = [
        "Thread",
        f"  id={display_id}",
        f"  driver={driver.name}",
        f"  transcript={transcript}",
    ]
    window_start = turns[0].start_ts if turns else "-"
    window_end = (
        (turns[-1].end_ts or turns[-1].last_activity_ts or turns[-1].start_ts)
        if turns
        else "-"
    )
    lines.append(
        f"  turns={len(turns)} compactions={len(compactions)} "
        f"window={window_start} -> {window_end}"
    )
    snapshot = meter.latest_snapshot
    if snapshot:
        lines.append(f"  keep_working={context_meter_instruction('available')}")
    lines.append("Latest Activity")
    if latest_turn is None:
        lines.append("  none")
        return "\n".join(lines)
    activity_ts = (
        latest_turn.end_ts or latest_turn.last_activity_ts or latest_turn.start_ts
    )
    lines.append(
        f"  ts={activity_ts} turn={latest_turn.turn_id or '-'} "
        f"completed={latest_turn.completed}"
    )
    user = _latest_text(latest_turn.user_messages)
    assistant = _latest_text(
        [*latest_turn.assistant_commentary, *latest_turn.final_answers]
    )
    final = _latest_text(latest_turn.final_answers)
    if user:
        lines.append(f"  latest_user={clip(user)}")
    if assistant:
        lines.append(f"  latest_assistant={clip(assistant)}")
    if final:
        lines.append(f"  latest_final={clip(final)}")
    lines.append(
        f"  commands={latest_turn.command_count} patches={latest_turn.patch_count} "
        f"errors={latest_turn.error_count} web_searches={latest_turn.web_search_count}"
    )
    return "\n".join(lines)


def _latest_activity_turn(
    turns: list[records.TurnRecord],
) -> records.TurnRecord | None:
    if not turns:
        return None
    return max(
        turns,
        key=lambda turn: turn.end_ts or turn.last_activity_ts or turn.start_ts,
    )


def _latest_text(values: list[str]) -> str:
    return next((text for text in reversed(values) if text.strip()), "")


def _filter_kwargs(args: argparse.Namespace) -> dict[str, object]:
    return {
        "start": _normalize_bound(getattr(args, "start", None)),
        "end": _normalize_bound(getattr(args, "end", None)),
        "contains": getattr(args, "contains", None),
        "turn_ids": _clean_list(getattr(args, "turn_ids", None)),
        "tools": _clean_list(getattr(args, "tools", None)),
    }


def _clean_list(values: list[str] | None) -> list[str]:
    return [value.strip() for value in values or [] if value.strip()]


def _print_summary(files: list, *, recent: int) -> None:
    turns = records.collect_turns(files)
    compactions = records.collect_compactions(files)
    meter = collect_context_meter(files)
    print("Summary")
    print(
        f"  turns={len(turns)} completed={sum(1 for t in turns if t.completed)} "
        f"compactions={len(compactions)}"
    )
    print(
        "  commands={c} patches={p} errors={e} web_searches={w}".format(
            c=sum(t.command_count for t in turns),
            p=sum(t.patch_count for t in turns),
            e=sum(t.error_count for t in turns),
            w=sum(t.web_search_count for t in turns),
        )
    )
    snapshot = meter.latest_snapshot
    if snapshot:
        print(f"  keep_working={context_meter_instruction('available')}")
    asks = [
        (turn.start_ts, text)
        for turn in turns
        for text in turn.user_messages
        if not records.is_scaffolding_text(text)
    ]
    print("Recent Prompts")
    for ts, text in asks[-recent:]:
        print(f"  {ts} {clip(text)}")
    finals = [(turn.start_ts, text) for turn in turns for text in turn.final_answers]
    print("Recent Finals")
    for ts, text in finals[-recent:]:
        print(f"  {ts} {clip(text)}")


def _print_tokens(files: list, *, by_file: bool) -> None:
    usages = records.collect_token_usage(files)
    total = records.combine_token_usage(usages, label="TOTAL")
    rows = [*usages, total] if by_file or len(usages) > 1 else [total]
    print("Tokens")
    for usage in rows:
        uncached = usage.input_tokens - usage.cached_input_tokens
        print(
            f"  {usage.label}: total={format_int(usage.total_tokens)} "
            f"input={format_int(usage.input_tokens)} "
            f"cached={format_int(usage.cached_input_tokens)} "
            f"uncached={format_int(uncached)} "
            f"output={format_int(usage.output_tokens)} "
            f"reasoning={format_int(usage.reasoning_output_tokens)} "
            f"snapshots={usage.snapshot_count} "
            f"window={usage.first_snapshot_ts or '-'} -> "
            f"{usage.last_snapshot_ts or '-'}"
        )


TimelineRow = tuple[str, int, str]


def _print_timeline(
    files: list,
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
    turn_ids: list[str] | None,
    tools: list[str] | None,
    limit: int,
    max_text: int,
) -> None:
    turns = records.filter_turns(
        records.collect_turns(files),
        start=start,
        end=end,
        contains=contains,
        turn_ids=turn_ids,
        tools=tools,
    )
    rows = _filter_timeline_rows(
        _timeline_turn_rows(turns, max_text=max_text),
        start=start,
        end=end,
        contains=None,
    )
    if not turn_ids and not tools:
        rows.extend(
            _filter_timeline_rows(
                _timeline_compaction_rows(
                    records.collect_compactions(files), max_text=max_text
                ),
                start=start,
                end=end,
                contains=contains,
            )
        )
    rows.sort(key=lambda row: (row[0], row[1]))
    rows = rows[-limit:]
    if not rows:
        print("no matching timeline events")
        return
    for ts, _rank, text in rows:
        print(f"{ts} {text}")


def _timeline_turn_rows(
    turns: list[records.TurnRecord], *, max_text: int
) -> list[TimelineRow]:
    rows: list[TimelineRow] = []
    for turn in turns:
        pieces = [
            f"{Path(turn.source_file).name}",
            f"turn={turn.turn_id or '-'}",
            f"completed={turn.completed}",
            f"cmds={turn.command_count}",
            f"patches={turn.patch_count}",
            f"errors={turn.error_count}",
        ]
        user = next(
            (text for text in reversed(turn.user_messages) if text.strip()), None
        )
        if user:
            pieces.append(f"user={clip(user, max_text)}")
        if turn.final_answers:
            pieces.append(f"final={clip(turn.final_answers[-1], max_text)}")
        elif turn.assistant_commentary:
            pieces.append(f"assistant={clip(turn.assistant_commentary[-1], max_text)}")
        rows.append((turn.start_ts, 0, " ".join(pieces)))
    return rows


def _timeline_compaction_rows(
    compactions: list[records.CompactionRecord], *, max_text: int
) -> list[TimelineRow]:
    return [
        (
            record.ts,
            1,
            f"{Path(record.source_file).name} compaction "
            f"assistant_before={clip(record.last_assistant_before_text, max_text)} "
            f"user_after={clip(record.first_user_after_text, max_text)}",
        )
        for record in compactions
    ]


def _filter_timeline_rows(
    rows: list[TimelineRow],
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
) -> list[TimelineRow]:
    needle = (contains or "").lower()
    kept: list[TimelineRow] = []
    for row in rows:
        ts, _rank, text = row
        if start and ts < start:
            continue
        if end and ts > end:
            continue
        if needle and needle not in text.lower():
            continue
        kept.append(row)
    return kept


def _normalize_bound(value: str | None) -> str | None:
    return normalize_timestamp(value) if value else None


def _print_turns(args: argparse.Namespace, files: list) -> None:
    turns = records.filter_turns(
        records.collect_turns(files),
        **_filter_kwargs(args),
    )
    turns = turns[-max(1, args.limit) :]
    if not turns:
        print("no matching turns")
        return
    full = args.view == "full"
    for turn in turns:
        header = (
            f"{turn.start_ts} turn={turn.turn_id or '-'} "
            f"completed={turn.completed} cmds={turn.command_count} "
            f"patches={turn.patch_count} errors={turn.error_count}"
        )
        print(header)
        for side, text in turn.ordered_messages:
            if full:
                print(f"  {side}:")
                for line in text.splitlines():
                    print(f"    {line}")
            else:
                print(f"  {side}: {clip(text)}")
        print()


def _print_compactions(files: list, *, limit: int) -> None:
    rows = records.collect_compactions(files)[-limit:]
    if not rows:
        print("no compactions")
        return
    for record in rows:
        print(
            f"{record.ts} assistant_before={clip(record.last_assistant_before_text)} "
            f"user_after={clip(record.first_user_after_text)}"
        )


def _print_slices(args: argparse.Namespace, files: list[Path]) -> None:
    rows = list(
        reversed(
            session_slices.build_compaction_slices(
                records.collect_turns(files), records.collect_compactions(files)
            )
        )
    )
    selected = set(getattr(args, "slice_id", []) or [])
    if selected:
        rows = [record for record in rows if record.slice_id in selected]
    rows = rows[: max(0, int(args.limit))]
    if not rows:
        print("no matching slices")
        return
    for record in rows:
        _print_slice_record(record, args)


def _print_slice_record(
    record: session_slices.SliceRecord, args: argparse.Namespace
) -> None:
    print(f"slice={record.slice_id} {record.start_ts} -> {record.end_ts}")
    print(
        f"  basis={record.basis} anchor={record.anchor_kind} status={record.status} "
        f"compactions={record.compaction_count} turns={len(record.turn_ids)} "
        f"patches={record.patch_count}"
    )
    print(f"  crossing_turn_files={', '.join(record.crossing_turn_files) or '-'}")
    if args.view == "full" and record.ordered_messages:
        print("  messages=")
        for role, text in record.ordered_messages[-8:]:
            print(f"    {role}: {clip(text, max(1, args.max_text))}")
    print()


def _print_phases(args: argparse.Namespace, files: list[Path]) -> None:
    turns = records.filter_turns(records.collect_turns(files), **_filter_kwargs(args))
    if not turns:
        print("no matching turns")
        return
    phases = analysis.segment_phases(turns)
    limit = getattr(args, "limit", None)
    if limit is not None:
        phases = phases[: max(1, int(limit))]
    for phase in phases:
        payload = analysis.phase_payload(phase, max(0, int(args.examples)))
        print(
            f"phase={payload['index']} family={payload['family']} "
            f"primary={payload['primary_archetype']} turns={payload['turns']} "
            f"commands={payload['commands']} patches={payload['patches']} "
            f"compactions={payload['compactions']} errors={payload['errors']} "
            f"duration_seconds={payload['duration_seconds']} "
            f"{payload['start_ts']} -> {payload['end_ts']}"
        )
        print(f"  top_paths={', '.join(payload['top_paths']) or '-'}")
        for example in payload["examples"]:
            print(
                f"  {example['start_ts']} turn={example['turn_id']} "
                f"archetype={example['archetype']} path={example['path']} "
                f"user={clip(example['user'], max(1, args.max_text))} "
                f"final={clip(example['final'], max(1, args.max_text))}"
            )
        print()


def _print_messages(args: argparse.Namespace, files: list[Path]) -> None:
    rows = analysis.filter_messages(
        analysis.collect_messages(files),
        start=_normalize_bound(getattr(args, "start", None)),
        end=_normalize_bound(getattr(args, "end", None)),
        contains=getattr(args, "contains", None),
        turn_ids=_clean_list(getattr(args, "turn_ids", None)),
        sides=_clean_list(getattr(args, "side", None)),
        phase_kinds=_clean_list(getattr(args, "phase_kinds", None)),
        flavors=_clean_list(getattr(args, "flavors", None)),
    )
    if not args.oldest_first:
        rows = list(reversed(rows))
    rows = rows[: max(1, int(args.limit))]
    if not rows:
        print("no matching messages")
        return
    for row in rows:
        print(
            f"{row.ts} turn={analysis.short_turn_id(row.turn_id)} side={row.side} "
            f"phase={row.phase} flavor={row.primary_flavor} "
            f"tags={','.join(row.flavor_tags) or '-'} "
            f"cues={','.join(row.matched_cues) or '-'}"
        )
        print(f"  text: {clip(row.text, max(1, args.max_text))}")


def _print_commits(files: list, *, limit: int) -> None:
    turns = records.collect_turns(files)
    rows = records.collect_commit_records(turns)[-limit:]
    if not rows:
        print("no commit-bearing assistant messages or finals")
        return
    for record in rows:
        print(
            f"{record.start_ts} turn={record.turn_id or '-'} sha={record.sha} "
            f"line={clip(record.line, COMMIT_LINE_PREVIEW_CHARS)} "
            f"user={clip(record.user, COMMIT_USER_PREVIEW_CHARS)}"
        )


def _print_commands(args: argparse.Namespace, files: list[Path]) -> None:
    population = commandrecords.completed_command_records(files)
    rows = _filter_command_rows(population, args, files)
    if getattr(args, "summary", False):
        _print_command_summary(
            rows,
            population_total=len(population),
            filter_label=_command_filter_label(args),
        )
        return
    rows = _limit_command_rows(
        rows,
        limit=max(1, args.limit),
        newest_first=bool(getattr(args, "newest_first", False)),
    )
    if not rows:
        print("no matching commands")
        return
    for record in rows:
        _print_command_row(record, max_text=max(1, args.max_text))


def _filter_command_rows(
    rows: list[commandrecords.CommandRecord],
    args: argparse.Namespace,
    files: list[Path],
) -> list[commandrecords.CommandRecord]:
    if getattr(args, "since_compaction", False):
        latest_compaction_ts = _latest_compaction_ts(files)
        if latest_compaction_ts:
            rows = [record for record in rows if record.ts > latest_compaction_ts]
    if getattr(args, "failed", False):
        rows = [
            record for record in rows if commandrecords.command_record_failed(record)
        ]
    if getattr(args, "pipelines", False):
        rows = [
            record
            for record in rows
            if commandaudit.command_has_shell_pipeline(record.command)
        ]
    if getattr(args, "noncanonical_pipelines", False):
        rows = [
            record
            for record in rows
            if commandaudit.command_is_noncanonical_pipeline(record.command)
        ]
    return rows


def _latest_compaction_ts(files: list[Path]) -> str | None:
    compactions = records.collect_compactions(files)
    return compactions[-1].ts if compactions else None


def _limit_command_rows(
    rows: list[commandrecords.CommandRecord], *, limit: int, newest_first: bool
) -> list[commandrecords.CommandRecord]:
    if newest_first:
        return list(reversed(rows))[:limit]
    return rows[-limit:]


def _command_filter_label(args: argparse.Namespace) -> str:
    labels = []
    if getattr(args, "since_compaction", False):
        labels.append("since_compaction")
    if getattr(args, "failed", False):
        labels.append("failed")
    if getattr(args, "pipelines", False):
        labels.append("pipelines")
    if getattr(args, "noncanonical_pipelines", False):
        labels.append("noncanonical_pipelines")
    return ",".join(labels) if labels else "all"


def _print_command_summary(
    rows: list[commandrecords.CommandRecord],
    *,
    population_total: int,
    filter_label: str,
) -> None:
    audit = commandaudit.audit_command_records(rows)
    failed = sum(1 for record in rows if commandrecords.command_record_failed(record))
    print("Commands")
    print(
        "  "
        f"total={population_total} matched={audit.total} filters={filter_label} "
        f"failed={failed} "
        f"wrapper={audit.wrapper_commands} non_wrapper={audit.non_wrapper_commands} "
        f"pipelines={audit.shell_pipelines} "
        f"canonical_pipelines={audit.canonical_pipelines} "
        f"noncanonical_pipelines={audit.noncanonical_pipelines}"
    )
    if audit.top_noncanonical:
        print(f"  top_noncanonical={', '.join(audit.top_noncanonical)}")


def _print_command_row(record: commandrecords.CommandRecord, *, max_text: int) -> None:
    pipeline = ""
    if commandaudit.command_is_noncanonical_pipeline(record.command):
        pipeline = " pipeline=noncanonical"
    elif commandaudit.command_has_shell_pipeline(record.command):
        pipeline = " pipeline=canonical"
    wrapper = (
        "yes" if commandaudit.command_starts_with_wrapper(record.command) else "no"
    )
    exit_code = record.exit_code if record.exit_code is not None else "-"
    print(
        f"{record.ts} turn={record.turn_id or '-'} exit={exit_code} "
        f"status={record.status or '-'} wrapper={wrapper}{pipeline} "
        f"cwd={record.cwd or '-'} cmd={clip(record.command, max_text)}"
    )
