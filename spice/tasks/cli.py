"""`spice task ...` — the phase-native Taskwarrior task control plane."""

from __future__ import annotations

import argparse
import sys
from datetime import datetime
from typing import Any

from spice.errors import SpiceError
from spice.tasks import config, identity, ops, render

_TASK_LIST_STATUSES = ("pending", "waiting", "completed", "deleted")
_TASK_LIST_NEWEST_FIELDS = ("end", "modified", "entry", "incepted", "claim_at")


def configure_task_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "task", help="Phase-native Taskwarrior task control plane."
    )
    parser.add_argument("--backend", help="Backend selector (name or absolute path).")
    actions = parser.add_subparsers(dest="task_action", required=True)
    _configure_task_read_parsers(actions)
    _configure_add_parser(actions)
    _configure_task_phase_parsers(actions)
    _configure_task_edit_parsers(actions)


def _configure_task_read_parsers(actions: Any) -> None:
    for name, helptext in (
        ("status", "Agent briefing."),
        ("next", "Select, claim, and render the next task."),
        ("doctor", "Prove allocator coherence."),
        ("stale", "List active claims whose deadline has elapsed."),
    ):
        actions.add_parser(
            name,
            help=helptext,
            recovery_examples=(f"spice task {name}",),
        ).set_defaults(func=handle)

    ls = actions.add_parser(
        "list",
        help="List tasks.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  spice task list --limit 20\n"
            "  spice task list --project serve.ui --status pending --limit 20\n"
            "  spice task list --project serve --status waiting"
        ),
        recovery_examples=(
            "spice task list --limit 20",
            "spice task list --project serve.ui --status pending --limit 20",
            "spice task list --all",
        ),
    )
    ls.add_argument(
        "--all",
        action="store_true",
        help="Include all task rows unless --status narrows the result.",
    )
    ls.add_argument(
        "--limit",
        type=_positive_int,
        metavar="N",
        help="Show at most N tasks, newest first.",
    )
    ls.add_argument(
        "--project",
        type=_project_filter,
        metavar="PROJECT",
        help=(
            "Filter by board project stem or exact project, for example "
            "serve or serve.ui. A leading project: prefix is accepted."
        ),
    )
    ls.add_argument(
        "--status",
        choices=_TASK_LIST_STATUSES,
        help="Filter by Taskwarrior status.",
    )
    ls.set_defaults(func=handle)

    show = actions.add_parser(
        "show",
        help="Render a task execution packet.",
        recovery_examples=("spice task show TASK-20260609T203539640394Z",),
    )
    show.add_argument("handle")
    show.set_defaults(func=handle)


def _configure_task_phase_parsers(actions: Any) -> None:
    done = actions.add_parser(
        "done",
        help="Complete the current phase (advances or completes).",
        recovery_examples=(
            'spice task done TASK-20260609T203539640394Z --validation "tests passed"',
        ),
    )
    done.add_argument("handle")
    done.add_argument("--validation", action="append", default=[])
    done.add_argument("--judgment")
    done.add_argument("--note", action="append", default=[], dest="notes")
    done.set_defaults(func=handle)

    review = actions.add_parser(
        "review",
        help="Record review evidence and complete the review phase.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "--then expects a follow-up task title in the same key=value batch "
            "grammar as task add; it creates a task that depends on the "
            "reviewed task. --followup accepts an existing task handle and "
            "adds the reviewed task as its dependency. Findings other than "
            "clean require durable follow-up tracking through either --then "
            "or --followup. Reviewers must verify the task description is "
            "current before recording a clean finding.\n\n"
            "Examples:\n"
            "  spice task review CLI-20260609T203539640394Z "
            '--finding clean --note "looks good"\n'
            "  spice task review CLI-20260609T203539640394Z "
            '--finding changes --then "title=Add coverage | project=task.cli '
            '| description=Why it matters | acceptance=Focused tests cover it"\n'
            "  spice task review CLI-20260609T203539640394Z "
            "--finding changes --followup TASK-20260609T203527316867Z"
        ),
        recovery_examples=(
            'spice task review TASK-20260609T203539640394Z --finding clean --note "looks good"',
            'spice task review TASK-20260609T203539640394Z --finding changes --then "title=Add coverage | project=task.cli | acceptance=Focused tests cover it"',
            "spice task review TASK-20260609T203539640394Z --finding changes --followup TASK-20260609T203527316867Z",
        ),
    )
    review.add_argument("handle")
    review.add_argument("--finding", default="clean")
    review.add_argument("--note")
    review.add_argument(
        "--then",
        action="append",
        default=[],
        dest="then",
        metavar="FOLLOWUP",
        help=(
            "Create a dependent follow-up. FOLLOWUP must include title=... and "
            "may include project=..., description=..., acceptance=..., "
            "priority=..., flow=..., tags=..., after=..., due=...."
        ),
    )
    review.add_argument(
        "--followup",
        action="append",
        default=[],
        metavar="HANDLE",
        help=(
            "Use an existing task as requested-change tracking by adding the "
            "reviewed task as its dependency. Repeat for multiple tasks."
        ),
    )
    review.set_defaults(func=handle)


def _configure_task_edit_parsers(actions: Any) -> None:
    _configure_oops_parser(actions)

    note = actions.add_parser(
        "note",
        help="Append a note annotation.",
        recovery_examples=(
            'spice task note TASK-20260609T203539640394Z "observed in review"',
        ),
    )
    note.add_argument("handle")
    note.add_argument("text", nargs="?")
    note.set_defaults(func=handle)

    depends = actions.add_parser(
        "depends",
        help="Add native dependency edges.",
        usage=(
            "spice task depends [-h] <handle> --after <dependency> [<dependency> ...]"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Example:\n"
            "  spice task depends CLI-20260609T203539640394Z "
            "--after SERVE-20260609T203527316867Z"
        ),
        recovery_examples=(
            "spice task depends TASK-20260609T203539640394Z --after SERVE-20260609T203527316867Z",
        ),
    )
    depends.add_argument(
        "handle",
        metavar="handle",
        help="Task that should wait for the dependency handle(s).",
    )
    depends.add_argument(
        "--after",
        nargs="+",
        required=True,
        metavar="dependency",
        help="Prerequisite task handle(s) that must complete first.",
    )
    depends.set_defaults(func=handle)

    claim = actions.add_parser(
        "claim",
        help="Claim a task for this actor.",
        recovery_examples=("spice task claim TASK-20260609T203539640394Z --steal",),
    )
    claim.add_argument("handle")
    claim.add_argument("--steal", action="store_true")
    claim.set_defaults(func=handle)

    unclaim = actions.add_parser(
        "unclaim",
        help="Release a claim.",
        recovery_examples=("spice task unclaim TASK-20260609T203539640394Z",),
    )
    unclaim.add_argument("handle")
    unclaim.set_defaults(func=handle)

    delete = actions.add_parser(
        "delete",
        help="Delete a task with a reason.",
        recovery_examples=(
            'spice task delete TASK-20260609T203539640394Z --reason "duplicate"',
        ),
    )
    delete.add_argument("handle")
    delete.add_argument("--reason", required=True)
    delete.set_defaults(func=handle)

    adopt = actions.add_parser(
        "adopt",
        help="Fold an existing orphan commit into a (new or given) task.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "A commit made outside a claimed task's window (after task done, or "
            "before a claim) is an orphan: task next refuses new work while it "
            "sits ahead of the baseline. adopt claims a task over the orphan "
            "WITHOUT the baseline fast-forward a normal claim performs, so the "
            "work is captured through the usual task done/review flow instead of "
            "a reset+redo. With no handle it mints a task (title defaults to the "
            "orphan commit subject); with a handle it claims that task.\n\n"
            "Examples:\n"
            "  spice task adopt\n"
            '  spice task adopt --project task.cli --title "Capture orphan fix"\n'
            '  spice task adopt --done --validation "tests passed"\n'
            "  spice task adopt TASK-20260609T203539640394Z"
        ),
        recovery_examples=(
            "spice task adopt",
            "spice task adopt TASK-20260609T203539640394Z",
        ),
    )
    adopt.add_argument("handle", nargs="?")
    adopt.add_argument("--title")
    adopt.add_argument("--project")
    adopt.add_argument("--description", action="append", default=[])
    adopt.add_argument("--priority", default=config.DEFAULT_PRIORITY)
    adopt.add_argument(
        "--done",
        action="store_true",
        help="Immediately complete the adopted implementation phase.",
    )
    adopt.add_argument("--validation", action="append", default=[])
    adopt.set_defaults(func=handle)


def _configure_add_parser(actions: Any) -> None:
    add = actions.add_parser(
        "add",
        help="Create a task (positional title, or batch from stdin).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Private work:\n"
            "  Use --private, or omit --project, to create this agent's "
            "private task.\n"
            "  Do not pass agent.*; agent stems are reserved for automatic "
            "private task creation.\n\n"
            "Examples:\n"
            '  spice task add "Clarify CLI help" --project task.cli '
            '--description "Longer merge body context"\n'
            '  spice task add "Private scratch task" --private\n'
            '  spice task add "Private scratch task"'
        ),
        recovery_examples=(
            'spice task add "Clarify CLI help" --project task.cli',
            'spice task add "Private scratch task" --private',
        ),
    )
    add.add_argument("title", nargs="?")
    add.add_argument(
        "--title",
        dest="title_option",
        metavar="TITLE",
        help="Alias for the positional title; pass one form or the other.",
    )
    add.add_argument(
        "--description",
        action="append",
        default=[],
        help=(
            "Longer task description for review context and durable "
            "merge-commit context. Repeat to create separate paragraphs."
        ),
    )
    route = add.add_mutually_exclusive_group()
    route.add_argument(
        "--project",
        help=(
            "Assignable project stem such as task.cli or serve.web. "
            "Omit --project for private work; agent.* is reserved."
        ),
    )
    route.add_argument(
        "--private",
        action="store_true",
        help=(
            "Create this single task in this agent's private project "
            "agent.<actor>.task."
        ),
    )
    add.add_argument(
        "--priority",
        default=config.DEFAULT_PRIORITY,
        help="Native Taskwarrior priority: high/medium/low/none or H/M/L.",
    )
    add.add_argument("--flow")
    add.add_argument("--tag", action="append", default=[], dest="tags")
    add.add_argument(
        "--after",
        action="append",
        default=[],
        metavar="HANDLE",
        help="Dependency handle; repeat --after for multiple dependencies.",
    )
    add.add_argument("--acceptance", action="append", default=[])
    add.add_argument("--wait")
    add.add_argument(
        "--every", help="Pace a looping flow (e.g. 1d): re-enter phase_0 on completion."
    )
    add.add_argument("--scheduled")
    add.add_argument("--until")
    add.add_argument("--due", help="Native due date; defaults from priority SLA.")
    add.add_argument("--claim", action="store_true")
    add.set_defaults(func=handle)


def _configure_oops_parser(actions: Any) -> None:
    oops = actions.add_parser(
        "oops",
        help="Capture agent friction as deferred triage.",
        recovery_examples=(
            'spice task oops "parser output was misleading" --severity low --kind tooling',
        ),
    )
    oops.add_argument("text", nargs="*")
    oops.add_argument(
        "--description",
        default="",
        help="Longer description stored on the oops row for triage context.",
    )
    oops.add_argument(
        "--severity",
        default="medium",
        help="Severity: critical/high/medium/low or H/M/L.",
    )
    oops.add_argument("--kind", default="")
    oops.add_argument("--surface", default="")
    oops.add_argument("--command", dest="oops_command", default="")
    oops.add_argument("--workaround", default="")
    oops.add_argument("--origin", default="")
    oops.add_argument("--tag", action="append", default=[], dest="tags")
    oops.set_defaults(func=handle)


def _flow(raw: str | None) -> list[str] | None:
    if not raw:
        return None
    return [p.strip() for p in raw.split(",") if p.strip()]


def _description(parts: list[str]) -> str:
    return "\n\n".join(part.strip() for part in parts if part.strip())


def _positive_int(raw: str) -> int:
    try:
        value = int(raw)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("--limit must be a positive integer") from exc
    if value < 1:
        raise argparse.ArgumentTypeError("--limit must be a positive integer")
    return value


def _project_filter(raw: str) -> str:
    value = raw.strip()
    if value.startswith("project:"):
        value = value[len("project:") :]
    if not value:
        raise argparse.ArgumentTypeError("--project requires a project stem")
    return value


def _list(args: argparse.Namespace) -> str:
    from spice.tasks import tw

    filters = _list_status_filters(args)
    if args.all:
        rows = tw.export(filters)
    else:
        rows = ops.visible_rows(tw.current_actor(), filters or ["status:pending"])
        rows = [r for r in rows if not ops._is_oops(r)]
    rows = _apply_list_project_filter(rows, getattr(args, "project", None))
    rows = _apply_list_limit(rows, getattr(args, "limit", None))
    return render.render_list(rows)


def _list_status_filters(args: argparse.Namespace) -> list[str]:
    status = getattr(args, "status", None)
    if status:
        return [f"status:{status}"]
    if getattr(args, "all", False):
        return []
    return ["status:pending"]


def _apply_list_project_filter(
    rows: list[dict[str, Any]], project: str | None
) -> list[dict[str, Any]]:
    if not project:
        return rows
    return [r for r in rows if _project_matches(r, project)]


def _project_matches(row: dict[str, Any], project: str) -> bool:
    row_project = str(row.get("project") or "")
    return row_project == project or row_project.startswith(f"{project}.")


def _apply_list_limit(
    rows: list[dict[str, Any]], limit: int | None
) -> list[dict[str, Any]]:
    if limit is None:
        return rows
    newest = sorted(rows, key=_list_newest_key, reverse=True)
    return newest[:limit]


def _list_newest_key(row: dict[str, Any]) -> tuple[int, str, str]:
    for field in _TASK_LIST_NEWEST_FIELDS:
        raw = str(row.get(field) or "").strip()
        if raw:
            return (1, _normalize_task_timestamp(raw), identity.render_handle(row))
    return (0, "", identity.render_handle(row))


def _normalize_task_timestamp(raw: str) -> str:
    for fmt in (
        "%Y%m%dT%H%M%S%fZ",
        "%Y%m%dT%H%M%SZ",
        "%Y-%m-%dT%H:%M:%S.%fZ",
        "%Y-%m-%dT%H:%M:%SZ",
    ):
        try:
            return datetime.strptime(raw, fmt).isoformat(timespec="microseconds")
        except ValueError:
            pass
    return raw


def _note(args: argparse.Namespace) -> str:
    text = args.text or sys.stdin.read().strip()
    if not text:
        raise SpiceError("task note requires text")
    return ops.note(args.handle, text)


def _oops(args: argparse.Namespace) -> str:
    text = " ".join(args.text).strip()
    if not text:
        return render.render_list(ops.oops_rows())
    return ops.oops(
        text,
        description=args.description,
        severity=args.severity,
        kind=args.kind,
        surface=args.surface,
        command=args.oops_command,
        workaround=args.workaround,
        origin=args.origin,
        tags=list(args.tags),
    )


_DISPATCH = {
    "status": lambda a: render.render_status(),
    "next": lambda a: render.render_next(),
    "doctor": lambda a: render.render_doctor(),
    "stale": lambda a: render.render_list(ops.stale_rows()),
    "list": _list,
    "show": lambda a: render.render_show(a.handle),
    "done": lambda a: ops.done(
        a.handle,
        validation=list(a.validation),
        judgment=a.judgment,
        notes=list(a.notes),
    ),
    "review": lambda a: ops.review(
        a.handle,
        finding=a.finding,
        note=a.note,
        then=list(a.then),
        followup=list(a.followup),
    ),
    "oops": _oops,
    "note": _note,
    "depends": lambda a: ops.depends(a.handle, list(a.after)),
    "claim": lambda a: ops.claim(a.handle, steal=a.steal),
    "unclaim": lambda a: ops.unclaim(a.handle),
    "delete": lambda a: ops.delete(a.handle, a.reason),
    "adopt": lambda a: ops.adopt(
        a.handle,
        title=a.title,
        project=a.project,
        description=_description(list(a.description)) or None,
        priority=a.priority,
        complete=a.done,
        validation=list(a.validation),
    ),
}


def _canonicalize_cli_task_handles(args: argparse.Namespace) -> list[str]:
    notices: list[str] = []
    raw_handle = getattr(args, "handle", None)
    if raw_handle:
        canonical, added_z = identity.canonicalize_zulu_free_handle(str(raw_handle))
        if added_z:
            args.handle = canonical
            notices.append(
                f"task_handle_added_z original={raw_handle} canonical={canonical}"
            )
    raw_after = getattr(args, "after", None)
    if isinstance(raw_after, list):
        canonical_after: list[str] = []
        for item in raw_after:
            canonical, added_z = identity.canonicalize_zulu_free_handle(str(item))
            canonical_after.append(canonical)
            if added_z:
                notices.append(
                    f"task_handle_added_z original={item} canonical={canonical}"
                )
        args.after = canonical_after
    return notices


def handle(args: argparse.Namespace) -> int:
    config.set_backend(getattr(args, "backend", None))
    action = args.task_action
    if action == "add":
        return _handle_add(args)
    func = _DISPATCH.get(action)
    if func is None:
        raise SpiceError(f"unknown task action {action!r}")
    for notice in _canonicalize_cli_task_handles(args):
        print(notice)
    print(func(args))
    return 0


def _resolve_add_title(args: argparse.Namespace) -> str:
    positional = (args.title or "").strip()
    option = (getattr(args, "title_option", None) or "").strip()
    if positional and option:
        raise SpiceError(
            "task add accepts a positional title or --title, not both; "
            "pass the title once"
        )
    return positional or option


def _handle_add(args: argparse.Namespace) -> int:
    notices = _canonicalize_cli_task_handles(args)
    title = _resolve_add_title(args)
    if title:
        for notice in notices:
            print(notice)
        handle_text = ops.add(
            title,
            project=None if getattr(args, "private", False) else args.project,
            description=_description(list(args.description)),
            priority=args.priority,
            flow=_flow(args.flow),
            tags=list(args.tags),
            after=list(args.after),
            acceptance=list(args.acceptance),
            wait=args.wait,
            claim=args.claim,
            every=args.every,
            scheduled=args.scheduled,
            until=args.until,
            due=args.due,
        )
        print(render_add_result(handle_text, claimed=args.claim))
        return 0
    lines = sys.stdin.read().splitlines()
    if not any(line.strip() for line in lines):
        raise SpiceError("task add requires a title argument or batch lines on stdin")
    handles = ops.add_batch(lines)
    for handle_text in handles:
        print(f"created {handle_text}")
    if handles:
        print("claim_state unclaimed")
        print("next: spice task next")
    return 0


def render_add_result(handle_text: str, *, claimed: bool) -> str:
    claim_state = "claimed" if claimed else "unclaimed"
    next_command = f"spice task show {handle_text}" if claimed else "spice task next"
    lines = [
        f"created {handle_text}",
        f"claim_state {claim_state}",
        f"next: {next_command}",
    ]
    if claimed:
        lines.append(ops.claim_drive_line(handle_text))
    return "\n".join(lines)
