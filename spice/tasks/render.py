"""Read-side rendering: list, show, status, next packet, doctor."""

from __future__ import annotations

import shlex
from datetime import UTC, datetime, timedelta
from typing import Any

from spice.errors import SpiceError
from spice.tasks import alloc, config, identity, lanes, ops, tw


SHOW_ANNOTATIONS_LIMIT = 6
_ACTIVE_CLAIM_FIELD_PROBLEMS = (
    ("claim_by", "active without claim_by"),
    ("claim_until", "active without claim deadline"),
    ("claim_context_link", "active without claim context link"),
)


def _f(row: dict[str, Any], key: str) -> str:
    value = row.get(key)
    return "" if value is None else str(value)


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


def _base_show_lines(row: dict[str, Any], rendered: str, flow: str) -> list[str]:
    return [
        f"handle {rendered}",
        f"title {_f(row, 'description')}",
        f"description {_f(row, 'task_description')}",
        f"project {_f(row, 'project')}",
        f"phase {_f(row, 'phase')} (i={_f(row, 'phase_i')})",
        f"flow {flow}",
        f"priority {_f(row, 'priority') or '-'}",
        f"urgency {_f(row, 'urgency')}",
        f"tags {' '.join('+' + t for t in row.get('tags') or [])}",
        f"status {_f(row, 'status')}",
        f"claim {_f(row, 'claim_by') or '-'} until {_f(row, 'claim_until') or '-'}",
        f"claim_thread {_f(row, 'claim_thread') or '-'}",
        (
            f"claim_context {_f(row, 'claim_context_start') or '-'} -> "
            f"{_f(row, 'claim_context_end') or '-'}"
        ),
        f"claim_context_link {_f(row, 'claim_context_link') or '-'}",
        f"acceptance {_f(row, 'acceptance')}",
        f"validation {_f(row, 'validation')}",
        f"review_author {_f(row, 'review_author') or '-'}",
        f"review_by {_f(row, 'review_by') or '-'}",
        (
            f"timing wait={_f(row, 'wait') or '-'} "
            f"scheduled={_f(row, 'scheduled') or '-'} "
            f"due={_f(row, 'due') or '-'} until={_f(row, 'until') or '-'}"
        ),
        (
            f"origin {_f(row, 'origin_thread') or '-'} "
            f"{_f(row, 'origin_branch') or '-'} {_f(row, 'origin_worktree') or '-'}"
        ),
    ]


def _briefing_command(thread: str, *, start: str = "", end: str = "") -> str:
    command = f"spice session briefing {shlex.quote(thread)}"
    if start and end:
        command += f" --start {shlex.quote(start)} --end {shlex.quote(end)}"
    return command


def _is_sentinel_thread(thread: str) -> bool:
    return bool(thread) and tw.canonical_actor(thread) == tw.canonical_actor(
        config.SENTINEL_ACTOR
    )


def _rehydrate_label(label: str) -> str:
    return "creator context" if label == "origin" else f"{label} context"


def _sentinel_rehydrate_line(label: str) -> str:
    return (
        f"  {_rehydrate_label(label)}: unavailable (sentinel thread has no transcript)"
    )


def _incepted_context_window(row: dict[str, Any]) -> tuple[str, str] | None:
    raw = _f(row, "incepted")
    if not identity.INCEPTED_RE.match(raw):
        return None
    instant = datetime.strptime(raw, "%Y%m%dT%H%M%S%fZ").replace(tzinfo=UTC)
    span = timedelta(seconds=config.CLAIM_CONTEXT_SECONDS)
    return _iso_for_render(instant - span), _iso_for_render(instant + span)


def _iso_for_render(when: datetime) -> str:
    return when.isoformat(timespec="microseconds").replace("+00:00", "Z")


def _origin_rehydrate_lines(row: dict[str, Any]) -> list[str]:
    thread = _f(row, "origin_thread")
    if not thread:
        return []
    if _is_sentinel_thread(thread):
        return [_sentinel_rehydrate_line("origin")]
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
    lines = [*_origin_rehydrate_lines(row), *_claim_rehydrate_lines(row)]
    if not lines:
        return []
    return ["rehydrate:", *lines]


def _context_check_lines(
    row: dict[str, Any], *, has_rehydrate_commands: bool
) -> list[str]:
    phase = _f(row, "phase")
    if _f(row, "status") != "pending" or phase in ("review", "oops"):
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
        return [
            f"review_commit {review_ref} (task merge; agent_head {agent_head})",
            (
                "review_diff_command "
                f"git show -m --first-parent --stat --patch {review_ref}"
            ),
            (
                "review_diff_note task merge commits need merge-aware diff; "
                "plain git show can omit the agent patch"
            ),
        ]
    return [f"review_commit {review_ref} (task head)"]


def _next_command_line(row: dict[str, Any], rendered: str) -> str:
    phase = _f(row, "phase")
    if phase == "review":
        if not _f(row, "claim_by"):
            return ops.next_task_drain_line(review_assignment=True)
        return (
            f"next: spice task review {rendered} --finding clean "
            '--note "description current; ..."'
        )
    if phase == "oops":
        return f'next: spice task note {rendered} "triage: ..."'
    return f'next: spice task done {rendered} --validation "..."'


def render_show(handle: str) -> str:
    row = identity.resolve(handle)
    flow = ",".join(ops.phases_of(row))
    rendered = identity.render_handle(row)
    lines = _base_show_lines(row, rendered, flow)
    lines.extend(_review_commit_lines(row))
    rehydrate = _rehydrate_lines(row)
    lines.extend(rehydrate)
    lines.extend(_context_check_lines(row, has_rehydrate_commands=bool(rehydrate)))
    deps = _deps_lines(row)
    if deps:
        lines.append("depends:")
        lines.extend(deps)
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
    waiting_count = sum(
        1 for r in alloc.visible_rows(actor, ["status:waiting"]) if not alloc.is_oops(r)
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
    row = alloc.next_task()
    if not row:
        return "no available tasks; run spice task status"
    rendered = identity.render_handle(row)
    lines = [
        "next task:",
        render_row(row),
        "",
        render_show(rendered),
        "",
        ops.claim_drive_line(rendered),
    ]
    return "\n".join(lines)


def _row_problems(r: dict[str, Any]) -> list[str]:
    handle = identity.render_handle(r)
    found: list[str] = []
    if not r.get("phase"):
        found.append(f"{handle} missing phase")
    phases = ops.phases_of(r)
    idx = ops.phase_index(r)
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
        f"approved phases {' '.join(config.APPROVED_PHASES)}",
    ]
    if problems:
        lines.append(f"PROBLEMS ({len(problems)}):")
        lines.extend(f"  {p}" for p in problems)
    else:
        lines.append("ok: no problems found")
    return "\n".join(lines)
