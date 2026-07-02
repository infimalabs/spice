"""`spice task ...` — the phase-native Taskwarrior task control plane."""

from __future__ import annotations

import argparse
import sys
from datetime import datetime
from typing import Any

from spice.errors import SpiceError
from spice.tasks import (
    alloc,
    artifacts,
    config,
    create,
    identity,
    markdown,
    ops,
    render,
    sizing,
)

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

    sizing = actions.add_parser(
        "sizing",
        help="Report observational size labels for completed tasks.",
        recovery_examples=(
            "spice task sizing --limit 20",
            "spice task sizing --project serve.ui",
        ),
    )
    sizing.add_argument(
        "--limit",
        type=_positive_int,
        metavar="N",
        help="Show at most N completed tasks, newest first.",
    )
    sizing.add_argument(
        "--project",
        type=_project_filter,
        metavar="PROJECT",
        help="Filter by project stem or exact project.",
    )
    sizing.set_defaults(func=handle)

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
        recovery_examples=("spice task show TASK-1k4Q5gJw",),
    )
    show.add_argument("handle")
    show.set_defaults(func=handle)

    ledger = actions.add_parser(
        "ledger",
        help="Export a task dependency closure as canonical markdown.",
        recovery_examples=("spice task ledger TASK-1k4Q5gJw",),
    )
    ledger.add_argument("handle")
    ledger.set_defaults(func=handle)

    _configure_artifact_parser(actions)


def _configure_artifact_parser(actions: Any) -> None:
    artifact = actions.add_parser(
        "artifact",
        help="Manage task-addressed sidecar artifacts.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  spice task artifact add TASK-1k4Q5gJw notes.md "
            "--type text/markdown\n"
            "  spice task artifact list TASK-1k4Q5gJw\n"
            "  spice task artifact show TASK-1k4Q5gJw A1\n"
            "  spice task artifact prune --older-than 30d --apply"
        ),
        recovery_examples=(
            "spice task artifact list TASK-1k4Q5gJw",
            "spice task artifact show TASK-1k4Q5gJw A1",
        ),
    )
    subactions = artifact.add_subparsers(dest="artifact_action", required=True)
    add = subactions.add_parser(
        "add",
        help="Copy a file into the task sidecar artifact store.",
        recovery_examples=("spice task artifact add TASK-1k4Q5gJw notes.md",),
    )
    add.add_argument("handle")
    add.add_argument("path")
    add.add_argument("--name")
    add.add_argument("--type", dest="content_type")
    add.add_argument(
        "--retention",
        default=artifacts.DEFAULT_RETENTION,
        choices=sorted(artifacts.RETENTIONS),
    )
    add.set_defaults(func=handle)

    list_parser = subactions.add_parser(
        "list",
        help="List artifacts attached to a task.",
        recovery_examples=("spice task artifact list TASK-1k4Q5gJw",),
    )
    list_parser.add_argument("handle")
    list_parser.set_defaults(func=handle)

    show = subactions.add_parser(
        "show",
        help="Show a text artifact or print a binary artifact path.",
        recovery_examples=("spice task artifact show TASK-1k4Q5gJw A1",),
    )
    show.add_argument("handle")
    show.add_argument("artifact_id")
    show.set_defaults(func=handle)

    prune = subactions.add_parser(
        "prune",
        help="Prune prunable artifacts for completed tasks.",
        recovery_examples=("spice task artifact prune --older-than 30d",),
    )
    prune.add_argument("--older-than")
    prune.add_argument("--apply", action="store_true")
    prune.set_defaults(func=handle)


def _configure_task_phase_parsers(actions: Any) -> None:
    done = actions.add_parser(
        "done",
        help=(
            "Complete the current phase (advances or completes). A task tagged "
            "gate:<coupling|reachability|symbol-reachability|assertion-free> "
            "cannot complete while that gate is dirty — the metric is read live, "
            "not asserted in prose."
        ),
        recovery_examples=(
            'spice task done TASK-1k4Q5gJw --validation "tests passed"',
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
            "  spice task review CLI-1k4Q5gJw "
            '--finding clean --note "looks good"\n'
            "  spice task review CLI-1k4Q5gJw "
            '--finding changes --then "title=Add coverage | project=task.cli '
            '| description=Why it matters | acceptance=Focused tests cover it"\n'
            "  spice task review CLI-1k4Q5gJw "
            "--finding changes --followup TASK-1k4Q5gh8"
        ),
        recovery_examples=(
            'spice task review TASK-1k4Q5gJw --finding clean --note "looks good"',
            'spice task review TASK-1k4Q5gJw --finding changes --then "title=Add coverage | project=task.cli | acceptance=Focused tests cover it"',
            "spice task review TASK-1k4Q5gJw --finding changes --followup TASK-1k4Q5gh8",
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
    _configure_ingest_parser(actions)
    _configure_oops_parser(actions)
    _configure_note_parser(actions)
    _configure_depends_parser(actions)
    _configure_wake_parser(actions)
    _configure_claim_parser(actions)
    _configure_unclaim_parser(actions)
    _configure_edit_parser(actions)
    _configure_delete_parser(actions)
    _configure_adopt_parser(actions)


def _configure_ingest_parser(actions: Any) -> None:
    ingest = actions.add_parser(
        "ingest",
        help="Import canonical or freeform markdown into task DAG rows.",
        recovery_examples=("spice task ingest backlog.md --project task.plan",),
    )
    ingest.add_argument("path")
    ingest.add_argument(
        "--project",
        help=(
            "Default assignable project for nodes without project fields; "
            "required for freeform markdown."
        ),
    )
    ingest.add_argument(
        "--priority",
        default=config.DEFAULT_PRIORITY,
        help="Default priority for nodes without priority fields.",
    )
    ingest.set_defaults(func=handle)


def _configure_note_parser(actions: Any) -> None:
    note = actions.add_parser(
        "note",
        help="Append a note annotation.",
        recovery_examples=('spice task note TASK-1k4Q5gJw "observed in review"',),
    )
    note.add_argument("handle")
    note.add_argument("text", nargs="?")
    note.set_defaults(func=handle)


def _configure_depends_parser(actions: Any) -> None:
    depends = actions.add_parser(
        "depends",
        help="Add native dependency edges.",
        usage=(
            "spice task depends [-h] <handle> --after <dependency> [<dependency> ...]"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=("Example:\n  spice task depends CLI-1k4Q5gJw --after SERVE-1k4Q5gh8"),
        recovery_examples=("spice task depends TASK-1k4Q5gJw --after SERVE-1k4Q5gh8",),
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


def _configure_wake_parser(actions: Any) -> None:
    wake = actions.add_parser(
        "wake",
        help="Make a waiting task current.",
        recovery_examples=(
            "spice task wake TASK-1k4Q5gJw",
            "spice task wake TASK-1k4Q5gJw TASK-1k4Q5h3N",
        ),
    )
    wake.add_argument("handles", nargs="+", metavar="handle")
    wake.set_defaults(func=handle)


def _configure_claim_parser(actions: Any) -> None:
    claim = actions.add_parser(
        "claim",
        help="Claim a task for this actor.",
        recovery_examples=("spice task claim TASK-1k4Q5gJw --steal",),
    )
    claim.add_argument("handle")
    claim.add_argument("--steal", action="store_true")
    claim.set_defaults(func=handle)


def _configure_unclaim_parser(actions: Any) -> None:
    unclaim = actions.add_parser(
        "unclaim",
        help="Release a claim.",
        recovery_examples=("spice task unclaim TASK-1k4Q5gJw",),
    )
    unclaim.add_argument("handle")
    unclaim.set_defaults(func=handle)


def _configure_edit_parser(actions: Any) -> None:
    edit = actions.add_parser(
        "edit",
        help="Change a task's priority and/or project in place.",
        recovery_examples=("spice task edit TASK-1k4Q5gJw --priority high",),
    )
    edit.add_argument("handle")
    edit.add_argument("--priority", help="high/medium/low/none or H/M/L.")
    edit.add_argument("--project", help="Reassign to an assignable dotted project.")
    edit.set_defaults(func=handle)


def _configure_delete_parser(actions: Any) -> None:
    delete = actions.add_parser(
        "delete",
        help="Delete a task with a reason.",
        recovery_examples=('spice task delete TASK-1k4Q5gJw --reason "duplicate"',),
    )
    delete.add_argument("handle")
    delete.add_argument("--reason", required=True)
    delete.set_defaults(func=handle)


def _configure_adopt_parser(actions: Any) -> None:
    adopt = actions.add_parser(
        "adopt",
        help="Capture an orphan commit (committed with no task claimed) into a task.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "An orphan commit is one you made while no task was claimed — before "
            "your first claim, or after the previous task completed. task next "
            "won't start new work while an orphan sits uncaptured on your branch.\n\n"
            "adopt wraps a task around the orphan instead of making you reset and "
            "redo it: it claims a task — newly minted, or the handle you pass — "
            "over the commit you already made, so you finish it through the normal "
            "done/review flow. Minting a new task always requires --project: adopt "
            "auto-claims regardless of lifetime, so there is no private fallback "
            "here. With no --title, the new task's title defaults to the orphan "
            "commit's subject.\n\n"
            "Examples:\n"
            '  spice task adopt --project task.cli --title "Capture orphan fix"\n'
            '  spice task adopt --project task.cli --done --validation "tests passed"\n'
            "  spice task adopt TASK-1k4Q5gJw"
        ),
        recovery_examples=(
            "spice task adopt --project task.example",
            "spice task adopt TASK-1k4Q5gJw",
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
            "private task. Only allowed in Steer lifetime; Drive and Drain "
            "require --project.\n"
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
            "Assignable dotted project such as task.cli or serve.web. "
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
        "--deferred",
        action="store_true",
        help="Create with the far-future wait used for deferred work.",
    )
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
        raise argparse.ArgumentTypeError("--project requires a project")
    return value


def _list(args: argparse.Namespace) -> str:
    from spice.tasks import tw

    filters = _list_status_filters(args)
    project = getattr(args, "project", None)
    explicit_hidden = bool(project and config.is_hidden_project(project))
    if args.all:
        rows = tw.export(filters)
    elif explicit_hidden:
        rows = tw.export(filters or ["status:pending"])
    else:
        rows = alloc.visible_rows(tw.current_actor(), filters or ["status:pending"])
        rows = [r for r in rows if not alloc.is_hidden(r)]
    rows = _apply_list_project_filter(rows, project)
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
    if identity.INCEPTED_RE.match(raw):
        return identity.incepted_datetime(raw).isoformat(timespec="microseconds")
    for fmt in (
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
        return render.render_list(alloc.oops_rows())
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
    "stale": lambda a: render.render_list(alloc.stale_rows()),
    "sizing": lambda a: sizing.completed_task_sizing_report(
        limit=a.limit, project=a.project
    ),
    "list": _list,
    "show": lambda a: render.render_show(a.handle),
    "ledger": lambda a: markdown.render_ledger(a.handle),
    "artifact": lambda a: _artifact(a),
    "ingest": lambda a: markdown.ingest_path(
        a.path,
        project=a.project,
        priority=a.priority,
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    ),
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
        creation_surface=config.TASK_CREATION_SURFACE_CLI,
    ),
    "oops": _oops,
    "note": _note,
    "depends": lambda a: ops.depends(a.handle, list(a.after)),
    "wake": lambda a: ops.wake(list(a.handles)),
    "claim": lambda a: ops.claim(a.handle, steal=a.steal),
    "unclaim": lambda a: ops.unclaim(a.handle),
    "edit": lambda a: ops.edit(a.handle, priority=a.priority, project=a.project),
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


# Work-driving packets carry the occasional rtk-feeding nudge (emitted only
# when rtk's own gain shows poor compaction); other actions stay quiet.
_RTK_NUDGE_ACTIONS = frozenset({"next", "claim"})


def _artifact(args: argparse.Namespace) -> str:
    action = args.artifact_action
    if action == "add":
        return artifacts.add_artifact(
            args.handle,
            args.path,
            name=args.name,
            content_type=args.content_type,
            retention=args.retention,
        )
    if action == "list":
        return artifacts.list_artifacts(args.handle)
    if action == "show":
        return artifacts.show_artifact(args.handle, args.artifact_id)
    if action == "prune":
        return artifacts.prune_artifacts(
            older_than=args.older_than,
            apply=args.apply,
        )
    raise SpiceError(f"unknown task artifact action {action!r}")


def handle(args: argparse.Namespace) -> int:
    config.set_backend(getattr(args, "backend", None))
    action = args.task_action
    if action == "add":
        return _handle_add(args)
    func = _DISPATCH.get(action)
    if func is None:
        raise SpiceError(f"unknown task action {action!r}")
    print(func(args))
    if action in _RTK_NUDGE_ACTIONS:
        nudge = ops.rtk_usage_nudge()
        if nudge:
            print(nudge)
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
    title = _resolve_add_title(args)
    if title:
        handle_text = create.add(
            title,
            project=None if getattr(args, "private", False) else args.project,
            description=_description(list(args.description)),
            priority=args.priority,
            flow=_flow(args.flow),
            tags=list(args.tags),
            after=list(args.after),
            acceptance=list(args.acceptance),
            wait=args.wait,
            deferred=args.deferred,
            claim=args.claim,
            every=args.every,
            scheduled=args.scheduled,
            until=args.until,
            due=args.due,
            creation_surface=config.TASK_CREATION_SURFACE_CLI,
        )
        print(render_add_result(handle_text, claimed=args.claim))
        return 0
    lines = sys.stdin.read().splitlines()
    if not any(line.strip() for line in lines):
        raise SpiceError("task add requires a title argument or batch lines on stdin")
    handles = create.add_batch(lines, creation_surface=config.TASK_CREATION_SURFACE_CLI)
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
