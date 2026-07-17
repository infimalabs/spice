"""Read-side rendering: list, show, status, next packet, doctor."""

from __future__ import annotations

import shlex
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd
from spice.tasks import (
    alloc,
    artifacts,
    claimstate,
    config,
    effort,
    identity,
    lanes,
    ops,
    opslog,
    tw,
)


SHOW_ANNOTATIONS_LIMIT = 6
SECONDS_PER_MINUTE = 60
MINUTES_PER_HOUR = 60
_ACTIVE_CLAIM_FIELD_PROBLEMS = (
    ("claim_by", "active without claim_by"),
    ("claim_until", "active without claim deadline"),
    ("claim_context_link", "active without claim context link"),
)


def _f(row: dict[str, Any], key: str) -> str:
    value = row.get(key)
    return "" if value is None else str(value)


def _task_version_text(row: dict[str, Any]) -> str:
    try:
        return str(opslog.task_version(identity.uuid_of(row)))
    except SpiceError as exc:
        return f"unavailable ({exc})"


def render_row(row: dict[str, Any]) -> str:
    handle = identity.render_handle(row)
    bits = [handle, f"[{_list_state_label(row)}]"]
    if pri := _f(row, "priority"):
        bits.append(f"P:{pri}")
    if proj := _f(row, "project"):
        bits.append(proj)
    bits.append(_f(row, "description"))
    return " ".join(bits)


def _list_state_label(row: dict[str, Any]) -> str:
    status = _f(row, "status")
    if status == "completed":
        return "done"
    if status == "deleted":
        return "deleted"
    return _f(row, "phase") or "-"


def render_list(rows: list[dict[str, Any]]) -> str:
    if not rows:
        return "no tasks"
    return "\n".join(render_row(r) for r in rows)


def render_task_list(
    rows: list[dict[str, Any]], *, scope: str, detail: str = ""
) -> str:
    if scope == "actor-route":
        if not detail:
            raise ValueError("actor-route task-list scope requires a filter")
        scope_line = f"scope actor-route filter {detail}"
    elif scope == "explicit-project":
        if not detail:
            raise ValueError("explicit-project task-list scope requires a project")
        scope_line = f"scope explicit-project {detail}"
    elif scope == "global":
        if detail:
            raise ValueError("global task-list scope does not accept detail")
        scope_line = "scope global --all"
    else:
        raise ValueError(f"unknown task-list scope {scope!r}")
    if rows:
        return "\n".join([scope_line, *(render_row(row) for row in rows)])
    if scope == "global":
        return f"{scope_line}\nno tasks in global scope"
    return (
        f"{scope_line}\n"
        "no tasks in scope; use --all for global rows or --project PROJECT "
        "for one project"
    )


def _deps_lines(row: dict[str, Any]) -> list[str]:
    out: list[str] = []
    for dep_uuid in row.get("depends") or []:
        dep = tw.export([str(dep_uuid)])
        if not dep:
            out.append(f"  after {dep_uuid} (missing)")
            continue
        d = dep[0]
        state = _f(d, "status")
        if str(d.get("claim_by") or ""):
            state += f" claim={d.get('claim_by')}"
        out.append(f"  after {identity.render_handle(d)} {state}")
    return out


def _base_show_lines(
    row: dict[str, Any],
    rendered: str,
    flow: str,
    *,
    include_recovery_context: bool,
) -> list[str]:
    lines = [
        f"handle {rendered}",
        f"title {_f(row, 'description')}",
        f"description {_f(row, 'task_description')}",
        f"origin {_f(row, 'origin') or '-'}",
        f"project {_f(row, 'project')}",
        f"phase {_f(row, 'phase')} (i={_f(row, 'phase_i')})",
        f"flow {flow}",
        f"priority {_f(row, 'priority') or '-'}",
        f"urgency {_f(row, 'urgency')}",
        f"tags {' '.join('+' + t for t in row.get('tags') or [])}",
        f"status {_f(row, 'status')}",
        f"version {_task_version_text(row)}",
        f"claim {_f(row, 'claim_by') or '-'} until {_f(row, 'claim_until') or '-'}",
        f"claim_thread {_f(row, 'claim_thread') or '-'}",
    ]
    if include_recovery_context:
        lines.extend(
            [
                (
                    f"claim_context {_f(row, 'claim_context_start') or '-'} -> "
                    f"{_f(row, 'claim_context_end') or '-'}"
                ),
                f"claim_context_link {_f(row, 'claim_context_link') or '-'}",
            ]
        )
    lines.extend(
        [
            f"acceptance {_f(row, 'acceptance')}",
            f"validation {_f(row, 'validation')}",
            f"review_author {_f(row, 'review_author') or '-'}",
            f"review_by {_f(row, 'review_by') or '-'}",
            f"review_finding {_f(row, 'review_finding') or '-'}",
            f"review_note {_f(row, 'review_note')}",
            (
                f"timing wait={_f(row, 'wait') or '-'} "
                f"scheduled={_f(row, 'scheduled') or '-'} "
                f"due={_f(row, 'due') or '-'} until={_f(row, 'until') or '-'}"
            ),
            (
                f"creator_context {_f(row, 'origin_thread') or '-'} "
                f"{_f(row, 'origin_branch') or '-'} {_f(row, 'origin_worktree') or '-'}"
            ),
        ]
    )
    return lines


def _briefing_command(thread: str, *, start: str = "", end: str = "") -> str:
    command = f"spice session briefing {shlex.quote(thread)}"
    if start and end:
        command += f" --start {shlex.quote(start)} --end {shlex.quote(end)}"
    return command


def _is_sentinel_thread(thread: str) -> bool:
    return bool(thread) and tw.canonical_actor(thread) == tw.canonical_actor(
        config.SENTINEL_ACTOR
    )


def _sentinel_rehydrate_line(label: str) -> str:
    return f"  {label} context: unavailable (sentinel thread has no transcript)"


def _incepted_context_window(row: dict[str, Any]) -> tuple[str, str] | None:
    raw = _f(row, "incepted")
    if not identity.INCEPTED_RE.match(raw):
        return None
    instant = identity.incepted_datetime(raw)
    span = timedelta(seconds=config.CLAIM_CONTEXT_SECONDS)
    return _iso_for_render(instant - span), _iso_for_render(instant + span)


def _iso_for_render(when: datetime) -> str:
    return when.isoformat(timespec="microseconds").replace("+00:00", "Z")


def _creator_rehydrate_lines(row: dict[str, Any]) -> list[str]:
    thread = _f(row, "origin_thread")
    if not thread:
        return []
    if _is_sentinel_thread(thread):
        return [_sentinel_rehydrate_line("creator")]
    window = _incepted_context_window(row)
    if window is None:
        return [f"  creator context, run: {_briefing_command(thread)}"]
    start, end = window
    return [
        f"  creator context, run: {_briefing_command(thread, start=start, end=end)}"
    ]


def _claim_rehydrate_lines(row: dict[str, Any]) -> list[str]:
    thread = _f(row, "claim_thread")
    start = _f(row, "claim_context_start")
    end = _f(row, "claim_context_end")
    turn = _f(row, "claim_context_turn")
    lines: list[str] = []
    if thread and _is_sentinel_thread(thread):
        if start or end or turn:
            return [_sentinel_rehydrate_line("claim")]
        return []
    if thread and start and end:
        lines.append(
            f"  claim context, run: {_briefing_command(thread, start=start, end=end)}"
        )
    if thread and turn and turn != thread:
        lines.append(
            "  claim turn, run: "
            f"spice session turns {shlex.quote(thread)} "
            f"--turn-id {shlex.quote(turn)} --view full"
        )
    return lines


def _rehydrate_lines(row: dict[str, Any]) -> list[str]:
    lines = [*_creator_rehydrate_lines(row), *_claim_rehydrate_lines(row)]
    if not lines:
        return []
    return ["rehydrate:", *lines]


def _context_check_lines(
    row: dict[str, Any], *, has_rehydrate_commands: bool
) -> list[str]:
    phase = _f(row, "phase")
    if _f(row, "status") != "pending" or phase == "review" or alloc.is_hidden(row):
        return []
    first = (
        "  Before editing, run the rehydrate command(s) above and assert the "
        "task description/acceptance still match current repo and operator state."
        if has_rehydrate_commands
        else (
            "  Before editing, inspect the task description/acceptance and "
            "current repo state; no transcript rehydrate command is available."
        )
    )
    return [
        "context_check:",
        first,
        (
            "  If context shifted or the task is stale, stop and update, split, "
            "or return it before changing files."
        ),
    ]


def _phase_guidance_lines(row: dict[str, Any], rendered: str) -> list[str]:
    phase = _f(row, "phase")
    if phase == "design":
        return [
            "phase_guidance:",
            (
                "  phase:design surveys the environment and may commit a deep "
                "repo-durable prose artifact under docs/design/accepted/ or "
                "docs/design/experimental/."
            ),
            (
                "  Design is higher-maturity than plan: it can create repository "
                "truth; plan stays task-local."
            ),
            (
                "  Design is the only phase that legitimizes committing design "
                "records; plan and other phases keep non-code reasoning on "
                "the board."
            ),
            (
                "  Spawn follow-up tasks for implementation work, then advance "
                "with the design artifact or explicit no-artifact rationale: "
                f'spice task done {rendered} --validation "..."'
            ),
        ]
    if phase == "plan":
        if claimstate.phases_of(row) == ["plan"]:
            return [
                "phase_guidance:",
                (
                    "  phase:plan is this task's entire flow, so the task must "
                    "decompose into at least one dependency-connected child; "
                    "acceptance on this task alone is insufficient."
                ),
                (
                    "  At least one connected child needs acceptance. Additional "
                    "children may omit it and enter their own plan phase."
                ),
                (
                    "  Record out-of-place discoveries as task notes, then "
                    "complete this planning bookend: "
                    f'spice task done {rendered} --validation "..."'
                ),
            ]
        return [
            "phase_guidance:",
            (
                "  phase:plan makes the execution contract explicit on the "
                "current task or decomposes it into dependency-connected child "
                "tasks; it does not write repo docs."
            ),
            (
                "  Add acceptance to this task when decomposition is unnecessary. "
                "When decomposing, connect child tasks with native dependencies; "
                "at least one child needs acceptance, and children without it "
                "enter their own plan phase."
            ),
            (
                "  Record out-of-place discoveries as task notes; advance once "
                "the current task or a connected child carries acceptance: "
                f'spice task done {rendered} --validation "..."'
            ),
        ]
    return []


def _wording_review_lines(row: dict[str, Any], rendered: str) -> list[str]:
    marker = _f(row, config.TASK_WORDING_REVIEW_UDA)
    if not marker:
        return []
    return [
        f"wording_review {marker}",
        (
            "wording_review_guidance suspect wording automatically prepended "
            "plan; matched wording remains in annotations. After enriching "
            "child tasks and acceptance, clear with "
            f'spice task reword {rendered} --reason "..."'
        ),
    ]


def _review_commit_lines(row: dict[str, Any]) -> list[str]:
    review_ref = _f(row, "done_ref")
    if not review_ref:
        return []
    merge_head = _f(row, "done_merge_head")
    agent_head = _f(row, "done_head")
    if not merge_head or not agent_head:
        raise SpiceError("task done_ref requires done_head and done_merge_head")
    if review_ref != merge_head:
        raise SpiceError("task done_ref must match done_merge_head")
    if merge_head != agent_head:
        upstream_head = _f(row, "done_upstream_head")
        diff_base = upstream_head or f"{review_ref}^1"
        base_source = "done_upstream_head" if upstream_head else "merge first parent"
        return [
            f"review_commit {review_ref} (task merge; agent_head {agent_head})",
            f"review_diff_base {diff_base} ({base_source})",
            (
                "review_diff_command "
                f"git show -m --first-parent --stat --patch {review_ref}"
            ),
            (
                "review_diff_note primary merge diff shows the integrated "
                f"reviewed patch; agent_head {agent_head} is provenance only "
                "because its ancestry can include already-integrated overlap"
            ),
        ]
    return [f"review_commit {review_ref} (task head)"]


def _phase_effort_lines(row: dict[str, Any]) -> list[str]:
    windows = effort.phase_effort_windows_for_tasks([row])
    if not windows:
        return []
    usage_rows = effort.phase_effort_usage_for_windows(
        windows, _phase_effort_transcript_files_by_thread(windows)
    )
    return ["phase_effort:", *[_phase_effort_row_line(usage) for usage in usage_rows]]


def _phase_effort_transcript_files_by_thread(
    windows: tuple[effort.PhaseEffortWindow, ...],
) -> dict[str, tuple[Path, ...]]:
    from spice.serve.messages import resolve_thread_transcript

    repo_root = repo_root_from_cwd()
    files_by_thread: dict[str, tuple[Path, ...]] = {}
    for thread_id in sorted(
        {window.thread_id for window in windows if window.thread_id}
    ):
        resolution = resolve_thread_transcript(thread_id, repo_root)
        if resolution is not None:
            files_by_thread[thread_id] = (resolution.path,)
    return files_by_thread


def _phase_effort_row_line(usage: effort.PhaseEffortUsage) -> str:
    parts = [
        f"  {usage.phase}[{usage.phase_index}]",
        *_phase_effort_token_parts(usage),
        f"turns={usage.turn_count}",
        f"msgs={usage.message_count}",
        f"renewals={usage.renewal_count}",
        f"wall={_format_wall_seconds(usage.wall_seconds)}",
    ]
    if usage.partial_markers:
        parts.append(f"partial={','.join(usage.partial_markers)}")
    return " ".join(parts)


def _phase_effort_token_parts(usage: effort.PhaseEffortUsage) -> list[str]:
    if not _phase_effort_has_model_tags(usage):
        return [
            "tokens=unattributed",
            "input=-",
            "cached=-",
            "output=-",
            "reasoning=-",
        ]
    return [
        f"tokens={usage.total_tokens}",
        f"input={usage.input_tokens}",
        f"cached={usage.cached_input_tokens}",
        f"output={usage.output_tokens}",
        f"reasoning={usage.reasoning_output_tokens}",
    ]


def _phase_effort_has_model_tags(usage: effort.PhaseEffortUsage) -> bool:
    return bool(usage.driver and usage.model and usage.effort)


def _format_wall_seconds(seconds: float | None) -> str:
    if seconds is None:
        return "-"
    rounded = int(round(seconds))
    if rounded < SECONDS_PER_MINUTE:
        return f"{rounded}s"
    minutes, remainder = divmod(rounded, SECONDS_PER_MINUTE)
    if minutes < MINUTES_PER_HOUR:
        return f"{minutes}m{remainder:02d}s"
    hours, minutes = divmod(minutes, MINUTES_PER_HOUR)
    return f"{hours}h{minutes:02d}m{remainder:02d}s"


def _next_command_line(row: dict[str, Any], rendered: str) -> str:
    phase = _f(row, "phase")
    if alloc.is_oops(row):
        return f'next: spice task note {rendered} "triage: ..."'
    if phase == "review":
        if not _f(row, "claim_by"):
            return ops.next_task_drain_line(review_assignment=True)
        return (
            f"next: spice task review {rendered} --finding clean "
            '--note "description current; ..."'
        )
    return f'next: spice task done {rendered} --validation "..."'


def _should_show_recovery_context(row: dict[str, Any]) -> bool:
    return _is_stale_claim(row, tw.now_iso())


def render_show(handle: str, *, include_recovery_context: bool | None = None) -> str:
    row = identity.resolve(handle)
    flow = ",".join(claimstate.phases_of(row))
    rendered = identity.render_handle(row)
    show_recovery_context = (
        _should_show_recovery_context(row)
        if include_recovery_context is None
        else include_recovery_context
    )
    lines = _base_show_lines(
        row,
        rendered,
        flow,
        include_recovery_context=show_recovery_context,
    )
    lines.extend(_review_commit_lines(row))
    lines.extend(_phase_effort_lines(row))
    rehydrate = _rehydrate_lines(row) if show_recovery_context else []
    lines.extend(rehydrate)
    if show_recovery_context:
        lines.extend(_context_check_lines(row, has_rehydrate_commands=bool(rehydrate)))
    lines.extend(_wording_review_lines(row, rendered))
    lines.extend(_phase_guidance_lines(row, rendered))
    deps = _deps_lines(row)
    if deps:
        lines.append("depends:")
        lines.extend(deps)
    lines.extend(artifacts.render_artifact_lines(rendered))
    annotations = row.get("annotations") or []
    if annotations:
        lines.append("annotations:")
        for ann in annotations[-SHOW_ANNOTATIONS_LIMIT:]:
            lines.append(f"  {ann.get('description', '')}")
    lines.append(_next_command_line(row, rendered))
    return "\n".join(lines)


def _visible_count(actor: str, filters: list[str]) -> int:
    return len(alloc.visible_rows(actor, filters))


def public_task_project_depth_label() -> str:
    min_depth, max_depth = config.project_depth_bounds()
    return f"public task project depth {min_depth}..{max_depth} dotted segments"


def _is_stale_claim(row: dict[str, Any], now: str) -> bool:
    until = str(row.get("claim_until") or "")
    return bool(until and until < now)


def render_status() -> str:
    actor = tw.current_actor()
    now = tw.now_iso()
    active_rows = alloc.visible_active_rows(actor)
    active = [r for r in active_rows if str(r.get("claim_by") or "") == actor]
    active_count = sum(1 for r in active_rows if not _is_stale_claim(r, now))
    ready_rows = alloc.visible_ready_rows(actor)
    review_rows = [r for r in ready_rows if _f(r, "phase") == "review"]
    non_review_ready_rows = [r for r in ready_rows if _f(r, "phase") != "review"]
    blocked_count = _visible_count(actor, ["status:pending", "+BLOCKED"])
    # -ACTIVE: a claimed deferred task keeps its wait and would otherwise be
    # double-counted as both active and waiting.
    waiting_count = sum(
        1
        for r in alloc.visible_rows(actor, ["status:waiting", "-ACTIVE"])
        if not alloc.is_hidden(r)
    )
    stale_count = sum(1 for r in active_rows if _is_stale_claim(r, now))
    lines = [
        f"claim {identity.render_handle(active[0]) if active else '-'}",
        f"actor {actor}",
        f"active {active_count}",
        f"ready {len(non_review_ready_rows)}",
        f"review {len(review_rows)}",
        f"blocked {blocked_count}",
        f"waiting {waiting_count}",
        f"stale {stale_count}",
        f"oops {len(alloc.oops_rows())}",
    ]
    route = lanes.team_route_for_actor(actor)
    effective_filter = alloc.effective_route_filter_args(actor, route)
    if effective_filter:
        lane_filter_label = " ".join(effective_filter)
    else:
        lane_filter_label = f"project:{config.private_project(actor)}"
    lines.insert(2, f"filter {lane_filter_label}")
    lines.insert(3, public_task_project_depth_label())
    return "\n".join(lines)


def render_next() -> str:
    renewal = claimstate.renew_claim()
    row = alloc.next_task()
    if not row:
        return "\n".join(
            [
                claimstate.claim_renewal_status_line(renewal),
                "no available tasks; run spice task status",
            ]
        )
    rendered = identity.render_handle(row)
    lines = [
        claimstate.claim_renewal_status_line(renewal),
        "next task:",
        render_row(row),
        "",
        render_show(rendered, include_recovery_context=True),
        "",
        ops.claim_drive_line(rendered),
    ]
    return "\n".join(lines)


def _row_problems(r: dict[str, Any]) -> list[str]:
    handle = identity.render_handle(r)
    found: list[str] = []
    if not r.get("phase"):
        found.append(f"{handle} missing phase")
    phases = claimstate.phases_of(r)
    idx = claimstate.phase_index(r)
    if phases and idx < len(phases) and str(r.get("phase")) != phases[idx]:
        found.append(f"{handle} phase != slot[{idx}]")
    for label in _row_claim_problem_labels(r):
        found.append(f"{handle} {label}")
    return found


def _row_claim_problem_labels(r: dict[str, Any]) -> tuple[str, ...]:
    if str(r.get("claim_by") or "") and not r.get("start"):
        return ("claimed but not active",)
    if not r.get("start"):
        return ()
    return tuple(
        label
        for key, label in _ACTIVE_CLAIM_FIELD_PROBLEMS
        if not str(r.get(key) or "")
    )


def _identity_problems(rows: list[dict[str, Any]]) -> list[str]:
    found: list[str] = []
    seen: set[str] = set()
    for r in rows:
        inc = str(r.get("incepted") or "")
        if not inc and str(r.get("status")) != "deleted":
            found.append(f"row {r.get('uuid')} missing incepted")
        if inc and inc in seen:
            found.append(f"duplicate incepted {inc}")
        seen.add(inc)
    return found


def render_doctor() -> str:
    return render_doctor_report()[0]


def render_doctor_report() -> tuple[str, list[str]]:
    """The allocator-coherence readout plus the raw problem list behind it.

    The list is the failure signal an aggregate `spice doctor` rolls up: empty
    means healthy. `render_doctor` keeps its string contract by dropping it.
    """
    rows = tw.export()
    pending = [r for r in rows if str(r.get("status")) in ("pending", "waiting")]
    problems = _identity_problems(rows)
    for r in pending:
        problems.extend(_row_problems(r))
    active_by_actor: dict[str, int] = {}
    for r in pending:
        actor = str(r.get("claim_by") or "")
        if actor and r.get("start"):
            active_by_actor[actor] = active_by_actor.get(actor, 0) + 1
    for actor, count in sorted(active_by_actor.items()):
        if count > 1:
            problems.append(f"actor {actor} has {count} active claims")
    lines = [
        f"backend {config.backend_root()}",
        f"taskrc {config.taskrc_path()}",
        f"rows {len(rows)} pending {len(pending)}",
        f"stale claims {len(alloc.stale_rows())}",
        f"reports {' '.join(config.REPORTS)}",
        f"analytics {' '.join(config.ANALYTICS_COMMANDS)}",
        public_task_project_depth_label(),
        f"assignable stems {' '.join(config.assignable_stems())}",
        f"internal stems {' '.join(config.INTERNAL_STEMS)}",
        f"hidden stems {' '.join(config.hidden_stems())}",
        f"approved phases {' '.join(config.APPROVED_PHASES)}",
    ]
    if problems:
        lines.append(f"PROBLEMS ({len(problems)}):")
        lines.extend(f"  {p}" for p in problems)
    else:
        lines.append("ok: no problems found")
    return "\n".join(lines), problems
