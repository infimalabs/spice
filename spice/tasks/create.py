"""Task creation and inline TASK batch parsing."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

from spice.errors import SpiceError
from spice.policy import COMMIT_MESSAGE_WRAP_LIMIT
from spice.tasks import config, gitsync, identity, ops, tw

TASK_TITLE_LIMIT = COMMIT_MESSAGE_WRAP_LIMIT
TASK_BATCH_DIRECTIVE_TOKEN = "TASK"
TASK_BATCH_DIRECTIVE_SEPARATOR_CHARS = " \t:-"


@dataclass(frozen=True)
class TaskAddBatchRequest:
    title: str
    project: str
    acceptance: tuple[str, ...]
    description: str | None = None
    priority: str = config.DEFAULT_PRIORITY
    flow: tuple[str, ...] = ()
    tags: tuple[str, ...] = ()
    after: tuple[str, ...] = ()
    due: str | None = None
    deferred: bool = False


@dataclass(frozen=True)
class TaskAddResult:
    handle: str
    project: str
    route_feedback: str


def _task_title(title: str, *, context: str = "") -> str:
    value = title.strip()
    if len(value) > TASK_TITLE_LIMIT:
        raise SpiceError(
            f"{context}task title is {len(value)} chars; keep task titles at "
            f"{TASK_TITLE_LIMIT} chars or less and move detail into "
            "--description"
        )
    return value


def _task_text(text: str) -> str:
    return text


def _task_description(description: str | None) -> str:
    return _task_text((description or "").strip())


def _task_acceptance(acceptance: Sequence[str]) -> list[str]:
    return [_task_text(item) for item in acceptance]


def _task_creation_surface(value: str | None) -> str:
    return _task_text((value or "").strip())


def _resolved_wait(*, wait: str | None, deferred: bool, claim: bool) -> str | None:
    if not deferred:
        return wait
    if wait:
        raise SpiceError("task add --deferred cannot be combined with --wait")
    if claim:
        raise SpiceError("task add --deferred cannot be combined with --claim")
    return config.OOPS_WAIT


def _resolve_add_project(actor: str, project: str | None, system_project: bool) -> str:
    if project is None:
        return config.private_project(actor)
    if system_project:
        return config.validate_project(project)
    return config.validate_manual_creation_project(project)


def _build_add_args(
    *,
    title: str,
    body: str | None,
    actor: str,
    incepted: str,
    resolved_project: str,
    phases: list[str],
    priority: str,
    tags: list[str],
    after: list[str],
    acceptance: list[str],
    wait: str | None,
    every: str | None,
    scheduled: str | None,
    until: str | None,
    due: str | None,
    extra: list[str] | None,
    creation_surface: str | None,
) -> list[str]:
    mapped_priority = config.map_priority(priority)
    args = [
        "add",
        f"incepted:{incepted}",
        f"project:{resolved_project}",
        *ops.flow_args(phases),
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
    surface = _task_creation_surface(creation_surface)
    if surface:
        args.append(f"{config.TASK_CREATION_SURFACE_UDA}:{surface}")
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
    return args


def _add_result(
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
    deferred: bool = False,
    every: str | None = None,
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
    extra: list[str] | None = None,
    existing: set[str] | None = None,
    system_project: bool = False,
    actor_override: str | None = None,
    creation_surface: str | None = None,
) -> TaskAddResult:
    title = _task_title(title)
    body = _task_description(description)
    resolved_wait = _resolved_wait(wait=wait, deferred=deferred, claim=claim)
    actor = tw.canonical_actor(actor_override or tw.current_actor())
    if claim:
        ops._require_single_active_slot(actor, action="task add --claim")
        # Match a normal claim's baseline check before creating the task row.
        # If this fails, task add --claim must not leave unclaimed work behind.
        gitsync.prepare_for_claim()
    resolved_project = _resolve_add_project(actor, project, system_project)
    phases = config.resolve_flow(flow, resolved_project)
    incepted = identity.mint_incepted(existing)
    if existing is not None:
        existing.add(incepted)
    args = _build_add_args(
        title=title,
        body=body,
        actor=actor,
        incepted=incepted,
        resolved_project=resolved_project,
        phases=phases,
        priority=priority,
        tags=tags,
        after=after,
        acceptance=acceptance,
        wait=resolved_wait,
        every=every,
        scheduled=scheduled,
        until=until,
        due=due,
        extra=extra,
        creation_surface=creation_surface,
    )
    tw.run(args)
    route_feedback = ops._subscribe_created_project(resolved_project, actor)
    if claim:
        created = tw.export([f"incepted.is:{incepted}"])
        if created:
            ops.do_claim(identity.uuid_of(created[0]), actor, guard_unclaimed=False)
    key = identity.key_for(resolved_project, title)
    result = TaskAddResult(
        handle=f"{key}-{incepted}",
        project=resolved_project,
        route_feedback=route_feedback,
    )
    return result


def add_one(
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
    deferred: bool = False,
    every: str | None = None,
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
    extra: list[str] | None = None,
    existing: set[str] | None = None,
    system_project: bool = False,
    actor_override: str | None = None,
    creation_surface: str | None = None,
) -> str:
    return _add_result(
        title=title,
        description=description,
        project=project,
        priority=priority,
        flow=flow,
        tags=tags,
        after=after,
        acceptance=acceptance,
        wait=wait,
        claim=claim,
        deferred=deferred,
        every=every,
        scheduled=scheduled,
        until=until,
        due=due,
        extra=extra,
        existing=existing,
        system_project=system_project,
        actor_override=actor_override,
        creation_surface=creation_surface,
    ).handle


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
    deferred: bool = False,
    claim: bool = False,
    every: str | None = None,
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
    creation_surface: str | None = None,
) -> str:
    return add_one(
        title=title,
        description=description,
        project=project,
        priority=priority,
        flow=flow,
        tags=tags or [],
        after=after or [],
        acceptance=acceptance or [],
        wait=wait,
        deferred=deferred,
        claim=claim,
        every=every,
        scheduled=scheduled,
        until=until,
        due=due,
        creation_surface=creation_surface,
    )


def _parse_add_batch_request(
    raw: str, index: int
) -> tuple[TaskAddBatchRequest | None, list[str]]:
    """Parse one `key=value | ...` line and collect its validation errors.

    Dependencies are resolved here (in the validate pass) so a bad `after`
    rejects the whole batch instead of creating earlier lines first.
    """
    fields, errors = _parse_batch_fields(_strip_task_batch_directive(raw), index)
    errors.extend(_batch_field_errors(fields, index))
    if errors:
        return None, errors
    return _batch_request_from_fields(fields), []


def _parse_batch_fields(raw: str, index: int) -> tuple[dict[str, str], list[str]]:
    fields: dict[str, str] = {}
    errors: list[str] = []
    for part in raw.split("|"):
        if "=" not in part:
            errors.append(f"line {index}: field without '=': {part.strip()!r}")
            continue
        key, value = part.split("=", 1)
        fields[key.strip()] = value.strip()
    return fields, errors


def _batch_field_errors(fields: dict[str, str], index: int) -> list[str]:
    errors: list[str] = []
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
    if "deferred" in fields and not _batch_bool_field(fields["deferred"]):
        errors.append(f"line {index}: deferred must be true/false")
    flow = _batch_csv(fields.get("flow", ""))
    if flow and fields.get("project"):
        try:
            config.resolve_flow(list(flow), fields["project"])
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    for dep in _batch_csv(fields.get("after", "")):
        try:
            identity.resolve(dep)
        except SpiceError:
            errors.append(f"line {index}: unknown after handle {dep!r}")
    return errors


def _batch_request_from_fields(fields: dict[str, str]) -> TaskAddBatchRequest:
    return TaskAddBatchRequest(
        title=fields["title"],
        description=fields.get("description") or None,
        project=fields["project"],
        priority=fields.get("priority", config.DEFAULT_PRIORITY),
        flow=_batch_csv(fields.get("flow", "")),
        tags=_batch_csv(fields.get("tags", "")),
        after=_batch_csv(fields.get("after", "")),
        acceptance=(fields["acceptance"],),
        due=fields.get("due") or None,
        deferred=_batch_bool(fields.get("deferred", "")),
    )


def _batch_csv(raw: str) -> tuple[str, ...]:
    return tuple(part.strip() for part in raw.split(",") if part.strip())


def _batch_bool_field(raw: str) -> bool:
    return raw.strip().lower() in {
        "",
        "0",
        "1",
        "false",
        "no",
        "off",
        "on",
        "true",
        "yes",
    }


def _batch_bool(raw: str) -> bool:
    return raw.strip().lower() in {"1", "on", "true", "yes"}


def _strip_task_batch_directive(raw: str) -> str:
    stripped = raw.strip()
    token_end = len(TASK_BATCH_DIRECTIVE_TOKEN)
    if not stripped.startswith(TASK_BATCH_DIRECTIVE_TOKEN):
        return raw
    if len(stripped) > token_end and stripped[token_end] not in (
        TASK_BATCH_DIRECTIVE_SEPARATOR_CHARS
    ):
        return raw
    cursor = token_end
    while cursor < len(stripped) and stripped[cursor] in (
        TASK_BATCH_DIRECTIVE_SEPARATOR_CHARS
    ):
        cursor += 1
    return stripped[cursor:].strip()


def parse_add_batch(lines: Sequence[str]) -> list[TaskAddBatchRequest]:
    parsed: list[TaskAddBatchRequest] = []
    errors: list[str] = []
    for index, raw in enumerate(lines, start=1):
        if not raw.strip():
            continue
        request, line_errors = _parse_add_batch_request(raw, index)
        errors.extend(line_errors)
        if request is not None:
            parsed.append(request)
    if errors:
        raise SpiceError("batch add rejected:\n" + "\n".join(errors))
    return parsed


def add_batch_results(
    lines: list[str],
    *,
    actor_override: str | None = None,
    creation_surface: str | None = None,
) -> list[TaskAddResult]:
    parsed = parse_add_batch(lines)
    existing = {str(r.get("incepted") or "") for r in tw.export()}
    results: list[TaskAddResult] = []
    for request in parsed:
        result = _add_result(
            title=request.title,
            description=request.description,
            project=request.project,
            priority=request.priority,
            flow=list(request.flow) or None,
            tags=list(request.tags),
            after=list(request.after),
            acceptance=list(request.acceptance),
            wait=None,
            deferred=request.deferred,
            claim=False,
            due=request.due,
            existing=existing,
            actor_override=actor_override,
            creation_surface=creation_surface,
        )
        results.append(result)
    return results


def add_batch(lines: list[str], *, creation_surface: str | None = None) -> list[str]:
    return [
        result.handle
        for result in add_batch_results(lines, creation_surface=creation_surface)
    ]
