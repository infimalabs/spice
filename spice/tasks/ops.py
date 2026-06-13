"""Task mutations and the allocator: add, claim, done, review, oops, next.

Every operation is a thin, guard-railed compile from agent intent to native
Taskwarrior.
"""

from __future__ import annotations

import os
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any, Sequence

from spice.agent.driver import DRIVER
from spice.agent.identity import canonical_thread_id
from spice.errors import SpiceError
from spice.mail.attachments import durable_inbox_attachment_references
from spice.policy import COMMIT_MESSAGE_WRAP_LIMIT
from spice.tasks import config, gitsync, identity, tw

TASK_TITLE_LIMIT = COMMIT_MESSAGE_WRAP_LIMIT


def annotate(target: str, text: str) -> None:
    """Annotate via `-- ` so attribute-like text (e.g. "depends: X") stays
    literal."""
    text = _task_text(text)
    tw.run([target, "annotate", "--", text])


# ---- flow / phase slots -------------------------------------------------


def flow_args(phases: list[str]) -> list[str]:
    args = [f"phase_{i}:{phase}" for i, phase in enumerate(phases)]
    args.append(f"phase:{phases[0]}")
    args.append("phase_i:0")
    return args


def phases_of(row: dict[str, Any]) -> list[str]:
    phases: list[str] = []
    for i in range(config.PHASE_SLOT_COUNT):
        value = str(row.get(f"phase_{i}") or "").strip()
        if not value:
            break
        phases.append(value)
    return phases


def phase_index(row: dict[str, Any]) -> int:
    return int(row.get("phase_i") or 0)


CLAIM_CLEAR = [
    f"{name}:"
    for name in (
        "claim_by",
        "claim_at",
        "claim_until",
        "claim_thread",
        "claim_worktree",
        "claim_branch",
        "claim_head",
        "claim_context_start",
        "claim_context_end",
        "claim_context_link",
        "claim_context_turn",
    )
]


def _iso(when: datetime) -> str:
    return when.isoformat(timespec="microseconds").replace("+00:00", "Z")


def claim_meta(actor: str) -> list[str]:
    at_dt = datetime.now(UTC)
    at = _iso(at_dt)
    until = _iso(at_dt + timedelta(seconds=config.CLAIM_TTL_SECONDS))
    start = _iso(at_dt - timedelta(seconds=config.CLAIM_CONTEXT_SECONDS))
    end = _iso(at_dt + timedelta(seconds=config.CLAIM_CONTEXT_SECONDS))
    thread = canonical_thread_id(os.environ.get(DRIVER.thread_id_env))
    if not thread:
        thread = tw.canonical_actor(config.SENTINEL_ACTOR)
    turn = (
        os.environ.get("CODEX_TURN_ID")
        or os.environ.get("CODEX_SESSION_TURN_ID")
        or thread
    ).strip()
    link = f"spice-session://{thread}?start={start}&end={end}"
    return [
        f"claim_by:{actor}",
        f"claim_at:{at}",
        f"claim_until:{until}",
        f"claim_thread:{thread}",
        f"claim_worktree:{config.repo_root()}",
        f"claim_branch:{tw.current_branch()}",
        f"claim_head:{tw.claim_head()}",
        f"claim_context_start:{start}",
        f"claim_context_end:{end}",
        f"claim_context_link:{link}",
        f"claim_context_turn:{turn}",
    ]


def _require_pending(row: dict[str, Any], action: str) -> None:
    status = str(row.get("status") or "")
    if status in ("completed", "deleted"):
        raise SpiceError(
            f"cannot {action} a {status} task: {identity.render_handle(row)}"
        )


def _require_owner(row: dict[str, Any], actor: str, action: str) -> None:
    owner = str(row.get("claim_by") or "")
    active = bool(row.get("start"))
    if owner == actor and active:
        return
    handle = identity.render_handle(row)
    if owner == actor:
        raise SpiceError(
            f"{action} requires native ACTIVE state on {handle}; "
            "run `spice task claim <handle>` to repair the claim"
        )
    if active and not owner:
        raise SpiceError(
            f"{action} blocked: {handle} is ACTIVE but has no claim_by; "
            "run `spice task claim <handle> --steal` to repair ownership"
        )
    if owner:
        raise SpiceError(f"task claimed by {owner}; not yours to {action}")
    raise SpiceError(
        f"{action} requires a claim; run `spice task next` (or `task claim`) first"
    )


def _is_same_author_review(row: dict[str, Any], actor: str) -> bool:
    return (
        str(row.get("phase") or "") == "review"
        and str(row.get("review_author") or "") == actor
    )


def _require_manual_claim_allowed(row: dict[str, Any], actor: str) -> None:
    if not _is_same_author_review(row, actor):
        return
    handle = identity.render_handle(row)
    raise SpiceError(
        f"cannot manually claim {handle}: this thread authored the review; "
        "run `spice task next`; proceed only if next assigns it"
    )


def _active_claims_for(actor: str) -> list[dict[str, Any]]:
    return [
        r
        for r in tw.export(["status:pending", "+ACTIVE"])
        if str(r.get("claim_by") or "") == actor
    ]


def _require_single_active_slot(
    actor: str, *, action: str, target: dict[str, Any] | None = None
) -> None:
    target_uuid = identity.uuid_of(target) if target else ""
    conflicts = [
        r for r in _active_claims_for(actor) if identity.uuid_of(r) != target_uuid
    ]
    if not conflicts:
        return
    active = max(conflicts, key=lambda r: str(r.get("claim_at") or ""))
    active_handle = identity.render_handle(active)
    if target:
        target_handle = identity.render_handle(target)
        raise SpiceError(
            f"{action} would create multiple active claims for {actor}; "
            f"finish or unclaim {active_handle} before claiming {target_handle}"
        )
    raise SpiceError(
        f"{action} would create multiple active claims for {actor}; "
        f"finish or unclaim {active_handle} before claiming new work"
    )


def do_claim(uuid: str, actor: str, *, guard_unclaimed: bool = True) -> bool:
    """Atomic claim: set the `start` date AND the claim metadata in one modify.

    A single locked write means a crash can never leave an active-but-
    unclaimed row (which would be orphaned: skipped by `next` yet resumable by
    no one). Idempotent — re-claiming (including a steal of an already-active
    row) just rewrites the owner and refreshes the deadline."""
    filters = (
        ["(", "status:pending", "or", "status:waiting", ")", "-ACTIVE"]
        if guard_unclaimed
        else []
    )
    try:
        tw.run([uuid, *filters, "modify", *claim_meta(actor), "wait:", "start:now"])
    except SpiceError:
        if guard_unclaimed:
            return False
        raise
    return True


# ---- creation -----------------------------------------------------------


def _task_title(title: str, *, context: str = "") -> str:
    value = title.strip()
    if len(value) > TASK_TITLE_LIMIT:
        raise SpiceError(
            f"{context}task title is {len(value)} chars; keep task titles at "
            f"{TASK_TITLE_LIMIT} chars or less and move detail into "
            "--description"
        )
    return value


def _task_artifact_root() -> Path:
    return config.backend_root() / "artifacts" / "attachments"


def _task_text(text: str) -> str:
    return durable_inbox_attachment_references(
        text,
        repo_root=config.repo_root(),
        artifact_root=_task_artifact_root(),
    )


def _task_description(description: str | None) -> str:
    return _task_text((description or "").strip())


def _task_acceptance(acceptance: Sequence[str]) -> list[str]:
    return [_task_text(item) for item in acceptance]


def default_project(actor: str) -> str:
    hexid = "".join(c for c in actor.lower() if c.isalnum())
    return f"agent.{hexid}.task"


def _add_one(
    *,
    title: str,
    description: str | None = None,
    project: str | None,
    priority: str,
    flow: list[str] | None,
    tags: list[str],
    after: list[str],
    acceptance: list[str],
    wait: str | None,
    claim: bool,
    every: str | None = None,
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
    extra: list[str] | None = None,
    existing: set[str] | None = None,
) -> str:
    title = _task_title(title)
    body = _task_description(description)
    actor = tw.current_actor()
    if claim:
        _require_single_active_slot(actor, action="task add --claim")
    private_project = default_project(actor)
    resolved_project = (
        private_project
        if project is None
        else config.validate_manual_creation_project(project)
    )
    phases = config.resolve_flow(flow, resolved_project)
    incepted = identity.mint_incepted(existing)
    if existing is not None:
        existing.add(incepted)
    mapped_priority = config.map_priority(priority)
    args = [
        "add",
        f"incepted:{incepted}",
        f"project:{resolved_project}",
        *flow_args(phases),
    ]
    if mapped_priority:
        args.append(f"priority:{mapped_priority}")
    if due:
        args.append(f"due:{due}")
    elif mapped_priority and mapped_priority in config.SLA_DUE_SECONDS:
        args.append(f"due:{tw.future_iso(config.SLA_DUE_SECONDS[mapped_priority])}")
    if wait:
        args.append(f"wait:{wait}")
    if every:
        config.parse_duration(every)  # validate the pacing duration up front
        args.append(f"pace:{every}")
    if scheduled:
        args.append(f"scheduled:{scheduled}")
    if until:
        args.append(f"until:{until}")
    if acceptance:
        args.append(f"acceptance:{' | '.join(_task_acceptance(acceptance))}")
    if body:
        args.append(f"task_description:{body}")
    args += [
        f"origin_thread:{actor}",
        f"origin_worktree:{config.repo_root()}",
        f"origin_branch:{tw.current_branch()}",
    ]
    for tag in tags:
        norm = "".join(c if c.isalnum() else "_" for c in tag.strip().lower()).strip(
            "_"
        )
        if norm:
            args.append(f"+{norm}")
    for handle in after:
        dep = identity.resolve(handle)
        args.append(f"depends:{identity.uuid_of(dep)}")
    args.extend(extra or [])
    args.append(title)
    tw.run(args)
    if claim:
        created = tw.export([f"incepted.is:{incepted}"])
        if created:
            # Same baseline refresh as any claim, so created-and-claimed work
            # starts from the current shared state.
            gitsync.prepare_for_claim()
            do_claim(identity.uuid_of(created[0]), actor, guard_unclaimed=False)
    key = identity.key_for(resolved_project, title)
    return f"{key}-{incepted}"


def add(
    title: str,
    *,
    description: str | None = None,
    project: str | None = None,
    priority: str = config.DEFAULT_PRIORITY,
    flow: list[str] | None = None,
    tags: list[str] | None = None,
    after: list[str] | None = None,
    acceptance: list[str] | None = None,
    wait: str | None = None,
    claim: bool = False,
    every: str | None = None,
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
) -> str:
    return _add_one(
        title=title,
        description=description,
        project=project,
        priority=priority,
        flow=flow,
        tags=tags or [],
        after=after or [],
        acceptance=acceptance or [],
        wait=wait,
        claim=claim,
        every=every,
        scheduled=scheduled,
        until=until,
        due=due,
    )


def _parse_batch_line(raw: str, index: int) -> tuple[dict[str, str], list[str]]:
    """Parse one `key=value | ...` line and collect its validation errors.

    Dependencies are resolved here (in the validate pass) so a bad `after`
    rejects the whole batch instead of creating earlier lines first."""
    fields: dict[str, str] = {}
    errors: list[str] = []
    for part in raw.split("|"):
        if "=" not in part:
            errors.append(f"line {index}: field without '=': {part.strip()!r}")
            continue
        key, value = part.split("=", 1)
        fields[key.strip()] = value.strip()
    for req in ("title", "project", "acceptance"):
        if not fields.get(req):
            errors.append(f"line {index}: missing required field {req!r}")
    if fields.get("title"):
        try:
            _task_title(fields["title"], context=f"line {index}: ")
        except SpiceError as exc:
            errors.append(str(exc))
    if fields.get("project"):
        try:
            config.validate_manual_creation_project(fields["project"])
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    if "priority" in fields:
        try:
            config.map_priority(fields["priority"])
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    if fields.get("flow") and fields.get("project"):
        try:
            config.resolve_flow(
                [p for p in fields["flow"].split(",") if p], fields["project"]
            )
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    for dep in [d.strip() for d in fields.get("after", "").split(",") if d.strip()]:
        try:
            identity.resolve(dep)
        except SpiceError:
            errors.append(f"line {index}: unknown after handle {dep!r}")
    return fields, errors


def add_batch(lines: list[str]) -> list[str]:
    parsed: list[dict[str, str]] = []
    errors: list[str] = []
    for index, raw in enumerate(lines, start=1):
        if not raw.strip():
            continue
        fields, line_errors = _parse_batch_line(raw, index)
        errors.extend(line_errors)
        parsed.append(fields)
    if errors:
        raise SpiceError("batch add rejected:\n" + "\n".join(errors))
    existing = {str(r.get("incepted") or "") for r in tw.export()}
    handles: list[str] = []
    for fields in parsed:
        handles.append(
            _add_one(
                title=fields["title"],
                description=fields.get("description"),
                project=fields.get("project"),
                priority=fields.get("priority", config.DEFAULT_PRIORITY),
                flow=[p for p in fields.get("flow", "").split(",") if p] or None,
                tags=[t for t in fields.get("tags", "").split(",") if t],
                after=[a for a in fields.get("after", "").split(",") if a],
                acceptance=[fields["acceptance"]],
                wait=None,
                claim=False,
                due=fields.get("due"),
                existing=existing,
            )
        )
    return handles


# ---- claim --------------------------------------------------------------


def claim(handle: str, *, steal: bool = False) -> str:
    row = identity.resolve(handle)
    _require_pending(row, "claim")
    actor = tw.current_actor()
    _require_manual_claim_allowed(row, actor)
    _require_single_active_slot(actor, action="task claim", target=row)
    owner = str(row.get("claim_by") or "")
    if owner and owner != actor and not steal:
        raise SpiceError(f"task already claimed by {owner}; use --steal to take it")
    if row.get("start") and not owner and not steal:
        raise SpiceError(
            "task is ACTIVE but has no claim_by; use --steal to repair ownership"
        )
    uuid = identity.uuid_of(row)
    guarded = not steal and owner != actor
    # A fresh claim (not a repair of our own already-active row) brings the
    # tree to the current baseline before the claim records its commit.
    is_repair = owner == actor and bool(row.get("start"))
    notes = [] if is_repair else gitsync.prepare_for_claim().notes
    if not do_claim(uuid, actor, guard_unclaimed=guarded):
        raise SpiceError(
            "claim lost a race: task became active before this claim landed; "
            "run task next again"
        )
    if owner and owner != actor:
        annotate(uuid, f"claim stolen: {owner} -> {actor}")
    handle_text = identity.render_handle(identity.resolve(handle))
    if notes:
        return "\n".join([*(f"task: {n}" for n in notes), handle_text])
    return handle_text


def unclaim(handle: str) -> str:
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    # Atomic: clear the start date (deactivate) and the claim metadata together.
    tw.run([uuid, "modify", "start:", *CLAIM_CLEAR])
    return identity.render_handle(row)


# ---- done / advance -----------------------------------------------------


def _publish_meta(
    row: dict[str, Any], actor: str, validation: list[str]
) -> dict[str, str]:
    """Harvest task facts for the programmatic merge commit message."""
    commit_validation = next((v for v in reversed(validation) if v), "")
    return {
        "title": str(row.get("description") or ""),
        "description": str(row.get("task_description") or ""),
        "uuid": str(row.get("uuid") or ""),
        "project": str(row.get("project") or ""),
        "phase": str(row.get("phase") or ""),
        "actor": str(row.get("claim_by") or actor),
        "validation": commit_validation,
    }


def _advance(row: dict[str, Any], *, review_author: str | None = None) -> str:
    uuid = identity.uuid_of(row)
    phases = phases_of(row)
    index = phase_index(row)
    handle = identity.render_handle(row)
    if index + 1 >= len(phases):
        pace = str(row.get("pace") or "").strip()
        if pace:
            wait = tw.future_iso(config.parse_duration(pace))
            tw.run(
                [
                    uuid,
                    "modify",
                    "phase_i:0",
                    f"phase:{phases[0]}",
                    f"wait:{wait}",
                    "start:",
                    *CLAIM_CLEAR,
                ]
            )
            return f"looped {handle} -> {phases[0]} (paced {pace}, waits until {wait})"
        tw.run([uuid, "done"])
        return f"completed {handle}"
    nxt = phases[index + 1]
    # One atomic modify: advance the phase, deactivate, and release the claim.
    args = [
        uuid,
        "modify",
        f"phase_i:{index + 1}",
        f"phase:{nxt}",
        "start:",
        *CLAIM_CLEAR,
    ]
    if nxt == "review":
        author = review_author or str(row.get("claim_by") or "") or tw.current_actor()
        args.append(f"review_author:{author}")
    tw.run(args)
    return f"advanced {handle} -> {nxt}"


def done(
    handle: str,
    *,
    validation: list[str],
    judgment: str | None = None,
    notes: list[str] | None = None,
) -> str:
    if not validation:
        raise SpiceError("task done requires --validation")
    tw.require_clean_worktree("task done")
    row = identity.resolve(handle)
    _require_pending(row, "complete")
    actor = tw.current_actor()
    _require_owner(row, actor, "complete")
    uuid = identity.uuid_of(row)
    # Integrate and publish this agent's work before any task state changes; a
    # real conflict raises here, leaving the task claimed for the agent to fix.
    sync = gitsync.integrate_and_publish(
        identity.render_handle(row),
        meta=_publish_meta(row, actor, validation),
    )
    for note_text in notes or []:
        annotate(uuid, note_text)
    for item in validation:
        annotate(uuid, f"validation: {item}")
    modify = [
        uuid,
        "modify",
        f"validation:{' | '.join(validation)}",
        *sync.uda_args,
    ]
    if judgment:
        modify.append(f"judgment:{judgment}")
    tw.run(modify)
    result = _advance(identity.resolve(handle))
    next_line = "next: run spice task next"
    if result.endswith(" -> review"):
        next_line = (
            "next: run spice task next "
            "(reviewer assignment; self-review only if next assigns it)"
        )
    return f"{result}\n{next_line}"


# ---- review -------------------------------------------------------------


def review(
    handle: str,
    *,
    finding: str = "clean",
    note: str | None = None,
    then: list[str] | None = None,
) -> str:
    finding = (finding or "clean").strip()
    if finding.casefold() != "clean" and not then:
        raise SpiceError(
            "unclean task review requires --then follow-up tracking; "
            'use --then "title=... | project=... | acceptance=..."'
        )
    tw.require_clean_worktree("task review")
    row = identity.resolve(handle)
    _require_pending(row, "review")
    if str(row.get("phase") or "") != "review":
        raise SpiceError("task review requires a task in the review phase")
    actor = tw.current_actor()
    _require_owner(row, actor, "review")
    uuid = identity.uuid_of(row)
    at = tw.now_iso()
    modify = [
        uuid,
        "modify",
        f"review_by:{actor}",
        f"review_at:{at}",
        f"review_finding:{finding}",
    ]
    if note:
        modify.append(f"review_note:{note}")
    tw.run(modify)
    annotate(uuid, f"review: finding={finding}; by={actor}")
    spawned: list[str] = []
    for spec in then or []:
        spawned.append(_spawn_followup(spec, after_uuid=uuid))
    sync = gitsync.integrate_and_publish(
        identity.render_handle(row),
        meta=_publish_meta(row, actor, [note or ""]),
    )
    tw.run([uuid, "modify", *sync.uda_args])
    result = _advance(identity.resolve(handle))
    lines = [f"reviewed {identity.render_handle(row)} {finding}; {result}"]
    lines += [f"spawned {h}" for h in spawned]
    lines.append("next: run spice task next")
    return "\n".join(lines)


def _spawn_followup(spec: str, *, after_uuid: str) -> str:
    fields: dict[str, str] = {}
    for part in spec.split("|"):
        if "=" in part:
            key, value = part.split("=", 1)
            fields[key.strip()] = value.strip()
    if not fields.get("title"):
        raise SpiceError(
            "--then needs a follow-up title=... entry: "
            f"{spec!r} (example: --then "
            '"title=Add coverage | project=task.cli | '
            "description=Why the follow-up matters | "
            'acceptance=Focused tests cover it")'
        )
    return _add_one(
        title=fields["title"],
        description=fields.get("description"),
        project=fields.get("project"),
        priority=fields.get("priority", config.DEFAULT_PRIORITY),
        flow=[p for p in fields.get("flow", "").split(",") if p] or None,
        tags=[t for t in fields.get("tags", "").split(",") if t],
        after=[a for a in fields.get("after", "").split(",") if a],
        acceptance=[fields["acceptance"]] if fields.get("acceptance") else [],
        wait=None,
        claim=False,
        due=fields.get("due"),
        extra=[f"depends:{after_uuid}"],
    )


# ---- oops / note / depends / delete --------------------------------------


def oops(
    text: str,
    *,
    description: str = "",
    severity: str = "medium",
    kind: str = "",
    surface: str = "",
    command: str = "",
    workaround: str = "",
    origin: str = "",
    tags: list[str] | None = None,
) -> str:
    severity = config.map_severity(severity)
    oops_tags = ["oops", severity, *([kind] if kind else []), *(tags or [])]
    handle = _add_one(
        title=text,
        description=description or None,
        project=config.OOPS_PROJECT,
        priority=config.SEVERITY_PRIORITY[severity],
        flow=["oops"],
        tags=oops_tags,
        after=[],
        acceptance=[],
        wait=config.OOPS_WAIT,
        claim=False,
    )
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    for label, value in (
        ("surface", surface),
        ("command", command),
        ("workaround", workaround),
        ("origin", origin),
    ):
        if value:
            annotate(uuid, f"{label}: {value}")
    return f"oops {handle} [{severity}]"


def note(handle: str, text: str) -> str:
    row = identity.resolve(handle)
    annotate(identity.uuid_of(row), text)
    return f"noted {identity.render_handle(row)}"


def depends(handle: str, after: list[str]) -> str:
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    for dep in after:
        dep_row = identity.resolve(dep)
        dep_uuid = identity.uuid_of(dep_row)
        if dep_uuid == uuid:
            raise SpiceError("a task cannot depend on itself")
        try:
            tw.run([uuid, "modify", f"depends:{dep_uuid}"])
        except SpiceError as exc:
            raise SpiceError(
                f"could not add dependency on {identity.render_handle(dep_row)} "
                "(would it create a cycle?)"
            ) from exc
        annotate(uuid, f"depends: {identity.render_handle(dep_row)}")
    return identity.render_handle(row)


def delete(handle: str, reason: str) -> str:
    row = identity.resolve(handle)
    _require_pending(row, "delete")
    uuid = identity.uuid_of(row)
    annotate(uuid, f"deleted: {reason}")
    tw.run([uuid, "modify", f"delete_reason:{reason}"])
    tw.run([uuid, "delete"])
    return identity.render_handle(row)


# ---- allocator (next) -----------------------------------------------------


def _is_oops(row: dict[str, Any]) -> bool:
    return "oops" in (row.get("tags") or [])


def ready_rows() -> list[dict[str, Any]]:
    """Available work: READY, not already claimed (claimed rows are +ACTIVE),
    and not oops."""
    rows = tw.export(["status:pending", "+READY", "-ACTIVE"])
    return [r for r in rows if not _is_oops(r) and not str(r.get("claim_by") or "")]


def oops_rows() -> list[dict[str, Any]]:
    """Deferred oops items carry a far-future wait, so they are `waiting`."""
    return [
        r
        for r in tw.export(["+oops"])
        if str(r.get("status")) in ("pending", "waiting")
    ]


def stale_rows() -> list[dict[str, Any]]:
    """Active claims whose deadline has elapsed (claim_until < now). ISO-8601
    timestamps share a format here, so a lexicographic compare is
    chronological."""
    now = tw.now_iso()
    out: list[dict[str, Any]] = []
    for r in tw.export(["+ACTIVE"]):
        until = str(r.get("claim_until") or "")
        if until and until < now:
            out.append(r)
    return out


def _scope_filter(actor: str, lane_filter: list[str] | None) -> list[str]:
    private = f"project:{default_project(actor)}"
    if not lane_filter:
        return [private]
    if private in lane_filter:
        return lane_filter
    return ["(", private, "or", *lane_filter, ")"]


def effective_filter_args(actor: str, lane_filter: list[str] | None) -> list[str]:
    return _scope_filter(actor, lane_filter)


def _team_route_filter(actor: str) -> list[str] | None:
    from spice.tasks import lanes

    return lanes.filter_args(lanes.team_route_for_actor(actor))


def visible_rows(actor: str, filters: list[str]) -> list[dict[str, Any]]:
    return tw.export([*filters, *_scope_filter(actor, _team_route_filter(actor))])


def visible_ready_rows(actor: str) -> list[dict[str, Any]]:
    rows = visible_rows(actor, ["status:pending", "+READY", "-ACTIVE"])
    return [r for r in rows if not _is_oops(r) and not str(r.get("claim_by") or "")]


def visible_active_rows(actor: str) -> list[dict[str, Any]]:
    rows = visible_rows(actor, ["status:pending", "+ACTIVE"])
    return [r for r in rows if not _is_oops(r) and str(r.get("claim_by") or "")]


def visible_pending_rows(actor: str) -> list[dict[str, Any]]:
    rows = visible_rows(actor, ["status:pending"])
    return [r for r in rows if not _is_oops(r)]


def _candidate_rows(
    actor: str, lane_filter: list[str] | None, overrides: list[str]
) -> list[dict[str, Any]]:
    base_filter = ["status:pending", "+READY", "-ACTIVE"]
    return tw.export(
        [*base_filter, *_scope_filter(actor, lane_filter)],
        overrides=overrides,
    )


def next_task() -> dict[str, Any] | None:
    from spice.tasks import alloc, lanes

    actor = tw.current_actor()
    active_rows = tw.export(["status:pending", "+ACTIVE"])
    own_active = [r for r in active_rows if str(r.get("claim_by") or "") == actor]
    if own_active:
        return max(own_active, key=lambda r: str(r.get("claim_at") or ""))

    route = lanes.team_route_for_actor(actor)
    overrides = alloc.actor_overrides(actor, route)
    lane_filter = lanes.filter_args(route)
    candidates = [
        r
        for r in _candidate_rows(actor, lane_filter, overrides)
        if not _is_oops(r) and not str(r.get("claim_by") or "")
    ]
    if not candidates:
        return None
    # We intend to claim: bring the tree to the current baseline once before
    # the claim records HEAD, so new work starts from the latest shared state.
    for note_text in gitsync.prepare_for_claim().notes:
        print(f"task: {note_text}")
    claimed_rows = tw.export([f"claim_by.is:{actor}"])
    for chosen in alloc.order(candidates, actor, claimed_rows, active_rows):
        if not do_claim(identity.uuid_of(chosen), actor, guard_unclaimed=True):
            continue
        fresh = identity.resolve(identity.render_handle(chosen))
        if str(fresh.get("claim_by") or "") == actor:
            return fresh
        # lost the race to a concurrent agent; fall through to the next one
    return None
