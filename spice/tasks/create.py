"""Task creation and inline TASK batch parsing."""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

from spice.errors import SpiceError
from spice.policy import COMMIT_MESSAGE_WRAP_LIMIT
from spice.tasks import config, gitsync, identity, ops, tw, wording

TASK_TITLE_LIMIT = COMMIT_MESSAGE_WRAP_LIMIT
TASK_BATCH_DIRECTIVE_TOKEN = "TASK"
TASK_BATCH_DIRECTIVE_SEPARATOR_CHARS = " \t:-"
# Inbox keys are UTC stamps like 20260104T000000000004Z; agents transcribing
# one sometimes drop the trailing Z (see inbox_item_key_aliases).
TASK_ORIGIN_ACK_KEY_RE = re.compile(r"^\d{8}T\d{6,}Z?$")
TASK_ORIGIN_REQUIRED_ERROR = (
    "task creation requires an origin: reference the acknowledgment that "
    "steered it (--origin ack:<inbox-key> / origin=ack:<inbox-key>) or the "
    "task it descends from (--origin task:<handle>); work created while "
    "holding an active claim inherits that claim automatically"
)
MISSING_ACCEPTANCE_PLAN_PHASE = "plan"
TASK_WORDING_REVIEW_MARKER = "required"
TASK_WORDING_REVIEW_ANNOTATION_PREFIX = "suspect wording:"


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
    origin: str | None = None


TaskWordingMatch = wording.TaskWordingMatch


@dataclass(frozen=True)
class TaskAddResult:
    handle: str
    project: str
    route_feedback: str
    wording_matches: tuple[TaskWordingMatch, ...] = ()


def detect_suspect_wording(
    *,
    title: str,
    description: str | None = None,
    acceptance: Sequence[str] = (),
    project: str | None = None,
    flow: Sequence[str] = (),
    repo_root: Path | None = None,
    driver_name: str | None = None,
) -> tuple[TaskWordingMatch, ...]:
    return wording.detect_task_creation_wording(
        title=title,
        description=description,
        acceptance=acceptance,
        project=project,
        flow=flow,
        repo_root=repo_root,
        driver_name=driver_name,
    )


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


def validated_task_origin(value: str) -> str:
    """Canonicalize an origin reference into ack:<inbox-key> or task:<HANDLE>.

    Bare values are auto-realmed: an inbox-key-shaped value is an ack
    reference, anything else must resolve to an existing task (any status --
    completed ancestors are valid provenance). Ack keys are validated by
    shape, not archival state: inline TASK capture may run before or after
    the acknowledgment itself is archived by the supervisor.
    """
    raw = str(value or "").strip()
    if not raw:
        raise SpiceError(TASK_ORIGIN_REQUIRED_ERROR)
    realm, _, rest = raw.partition(":")
    if realm == "ack" and rest:
        return f"ack:{_validated_origin_ack_key(rest)}"
    if realm == "task" and rest:
        return f"task:{_validated_origin_task_handle(rest)}"
    if TASK_ORIGIN_ACK_KEY_RE.match(raw):
        return f"ack:{_validated_origin_ack_key(raw)}"
    return f"task:{_validated_origin_task_handle(raw)}"


def _validated_origin_ack_key(key: str) -> str:
    key = key.strip()
    if not TASK_ORIGIN_ACK_KEY_RE.match(key):
        raise SpiceError(
            "task origin ack key must be an inbox key like "
            f"20260104T000000000004Z: {key!r}"
        )
    return key if key.endswith("Z") else f"{key}Z"


def _validated_origin_task_handle(handle: str) -> str:
    handle = handle.strip()
    try:
        return identity.render_handle(identity.resolve(handle))
    except SpiceError as exc:
        raise SpiceError(
            "task origin must reference ack:<inbox-key> or an existing "
            f"task handle: {handle!r} ({exc})"
        ) from exc


def _resolved_task_origin(origin: str | None, actor: str) -> str:
    # Every single task carries an origin -- there is almost never truly no
    # origination to point at. Derivation keeps the common cases hands-free
    # (explicit reference, else the actor's active claim); anything truly
    # context-free names the acknowledgment or task that prompted it.
    if origin:
        return validated_task_origin(origin)
    claim = ops.active_claim(actor)
    if claim is not None:
        return f"task:{identity.render_handle(claim)}"
    raise SpiceError(TASK_ORIGIN_REQUIRED_ERROR)


def _creation_flow_policy(
    *,
    flow: list[str] | None,
    acceptance: list[str],
    resolved_project: str,
    creation_surface: str | None,
    system_project: bool,
) -> list[str] | None:
    if flow or acceptance:
        return flow
    if system_project:
        return flow
    if creation_surface != config.TASK_CREATION_SURFACE_CLI:
        return flow
    if config.is_internal_or_hidden_project(resolved_project):
        return flow
    default_flow = config.resolve_flow(None, resolved_project)
    if default_flow[0] == MISSING_ACCEPTANCE_PLAN_PHASE:
        return default_flow
    return [
        MISSING_ACCEPTANCE_PLAN_PHASE,
        *(phase for phase in default_flow if phase != MISSING_ACCEPTANCE_PLAN_PHASE),
    ]


def _suspect_wording_flow_policy(
    phases: list[str],
    matches: Sequence[TaskWordingMatch],
    *,
    system_project: bool,
) -> list[str]:
    if not matches or system_project:
        return phases
    if MISSING_ACCEPTANCE_PLAN_PHASE in phases:
        return phases
    return [
        MISSING_ACCEPTANCE_PLAN_PHASE,
        *(phase for phase in phases if phase != MISSING_ACCEPTANCE_PLAN_PHASE),
    ]


def _suspect_wording_extra_args(
    matches: Sequence[TaskWordingMatch],
    *,
    system_project: bool,
) -> list[str]:
    if not matches or system_project:
        return []
    return [f"{config.TASK_WORDING_REVIEW_UDA}:{TASK_WORDING_REVIEW_MARKER}"]


def _suspect_wording_annotation(
    matches: Sequence[TaskWordingMatch],
) -> str:
    details = "; ".join(
        f"{match.source} {match.trigger_family} {match.matched!r}: {match.reason}"
        for match in matches
    )
    return (
        f"{TASK_WORDING_REVIEW_ANNOTATION_PREFIX} self-correction required "
        f"before implementation; matches: {details}"
    )


def _resolve_add_project(actor: str, project: str | None, system_project: bool) -> str:
    if project is None:
        _require_steer_lifetime(actor, action="creating a private task")
        return config.private_project(actor)
    if system_project:
        return config.validate_project(project)
    return config.validate_manual_creation_project(project)


def _require_steer_lifetime(actor: str, *, action: str) -> None:
    from spice.tasks import lanes

    route = lanes.team_route_for_actor(actor)
    lifetime = lanes.task_continuation_contract(route).lifetime
    if lifetime != "Steer":
        raise SpiceError(
            f"{action} requires Steer lifetime (got {lifetime or 'unrouted'}); "
            "pass --project outside Steer"
        )


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
    scheduled: str | None,
    until: str | None,
    due: str | None,
    extra: list[str] | None,
    creation_surface: str | None,
    origin: str = "",
) -> list[str]:
    mapped_priority = config.map_priority(priority)
    hidden_project = config.is_hidden_project(resolved_project)
    args = [
        "add",
        f"incepted:{incepted}",
        f"project:{resolved_project}",
        *ops.flow_args(phases),
    ]
    if hidden_project:
        args.append(f"{config.PROJECT_HIDDEN_UDA}:1")
    if mapped_priority:
        args.append(f"priority:{mapped_priority}")
    if due:
        args.append(f"due:{due}")
    elif mapped_priority and mapped_priority in config.SLA_DUE_SECONDS:
        args.append(f"due:{tw.future_iso(config.SLA_DUE_SECONDS[mapped_priority])}")
    if wait:
        args.append(f"wait:{wait}")
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
    if origin:
        args.append(f"origin:{origin}")
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
    if hidden_project:
        args.append(f"+{config.HIDDEN_TASK_TAG}")
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
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
    origin: str | None = None,
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
    resolved_project = _resolve_add_project(actor, project, system_project)
    resolved_origin = _resolved_task_origin(origin, actor)
    if claim:
        ops._require_single_active_slot(actor, action="task add --claim")
        # Match a normal claim's baseline check before creating the task row.
        # If this fails, task add --claim must not leave unclaimed work behind.
        gitsync.prepare_for_claim()
    routed_flow = _creation_flow_policy(
        flow=flow,
        acceptance=acceptance,
        resolved_project=resolved_project,
        creation_surface=creation_surface,
        system_project=system_project,
    )
    phases = config.resolve_flow(routed_flow, resolved_project)
    wording_matches = detect_suspect_wording(
        title=title,
        description=body,
        acceptance=acceptance,
        project=resolved_project,
        flow=phases,
    )
    phases = _suspect_wording_flow_policy(
        phases,
        wording_matches,
        system_project=system_project,
    )
    incepted = identity.mint_incepted(existing)
    if existing is not None:
        existing.add(incepted)
    extra_args = [
        *(extra or []),
        *_suspect_wording_extra_args(
            wording_matches,
            system_project=system_project,
        ),
    ]
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
        scheduled=scheduled,
        until=until,
        due=due,
        extra=extra_args,
        creation_surface=creation_surface,
        origin=resolved_origin,
    )
    tw.run(args)
    created = tw.export([f"incepted.is:{incepted}"]) if wording_matches or claim else []
    if wording_matches and not system_project and created:
        ops.annotate(
            identity.uuid_of(created[0]),
            _suspect_wording_annotation(wording_matches),
        )
    route_feedback = ops._subscribe_created_project(resolved_project, actor)
    if claim:
        if created:
            ops.do_claim(identity.uuid_of(created[0]), actor, guard_unclaimed=False)
    key = identity.key_for(resolved_project, title)
    result = TaskAddResult(
        handle=f"{key}-{incepted}",
        project=resolved_project,
        route_feedback=route_feedback,
        wording_matches=wording_matches,
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
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
    origin: str | None = None,
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
        scheduled=scheduled,
        until=until,
        due=due,
        origin=origin,
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
    scheduled: str | None = None,
    until: str | None = None,
    due: str | None = None,
    origin: str | None = None,
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
        scheduled=scheduled,
        until=until,
        due=due,
        origin=origin,
        creation_surface=creation_surface,
    )


BatchFields = dict[str, list[str]]
REPEATABLE_BATCH_FIELDS = frozenset({"acceptance"})


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


def _parse_batch_fields(raw: str, index: int) -> tuple[BatchFields, list[str]]:
    fields: BatchFields = {}
    errors: list[str] = []
    for part in raw.split("|"):
        if "=" not in part:
            errors.append(
                f"line {index}: field without '=': {part.strip()!r} "
                "(use key=value segments; repeat acceptance=... for "
                "multiple acceptance criteria)"
            )
            continue
        key, value = part.split("=", 1)
        fields.setdefault(key.strip(), []).append(value.strip())
    return fields, errors


def _batch_field_errors(fields: BatchFields, index: int) -> list[str]:
    errors: list[str] = []
    for req in ("title", "project"):
        if not _batch_field(fields, req):
            errors.append(f"line {index}: missing required field {req!r}")
    for key, values in fields.items():
        if len(values) > 1 and key not in REPEATABLE_BATCH_FIELDS:
            errors.append(
                f"line {index}: duplicate field {key!r}; only acceptance "
                "may be repeated"
            )
    if _batch_field(fields, "title"):
        try:
            _task_title(_batch_field(fields, "title"), context=f"line {index}: ")
        except SpiceError as exc:
            errors.append(str(exc))
    if _batch_field(fields, "project"):
        try:
            config.validate_manual_creation_project(_batch_field(fields, "project"))
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    if "priority" in fields:
        try:
            config.map_priority(_batch_field(fields, "priority"))
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    if "deferred" in fields and not _batch_bool_field(_batch_field(fields, "deferred")):
        errors.append(f"line {index}: deferred must be true/false")
    flow = _batch_csv(_batch_field(fields, "flow"))
    if flow and _batch_field(fields, "project"):
        try:
            config.resolve_flow(list(flow), _batch_field(fields, "project"))
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    for dep in _batch_csv(_batch_field(fields, "after")):
        try:
            identity.resolve(dep)
        except SpiceError:
            errors.append(f"line {index}: unknown after handle {dep!r}")
    if _batch_field(fields, "origin"):
        try:
            validated_task_origin(_batch_field(fields, "origin"))
        except SpiceError as exc:
            errors.append(f"line {index}: {exc}")
    return errors


def _batch_request_from_fields(fields: BatchFields) -> TaskAddBatchRequest:
    return TaskAddBatchRequest(
        title=_batch_field(fields, "title"),
        description=_batch_field(fields, "description") or None,
        project=_batch_field(fields, "project"),
        priority=_batch_field(fields, "priority") or config.DEFAULT_PRIORITY,
        flow=_batch_csv(_batch_field(fields, "flow")),
        tags=_batch_csv(_batch_field(fields, "tags")),
        after=_batch_csv(_batch_field(fields, "after")),
        acceptance=tuple(item for item in fields.get("acceptance", ()) if item),
        due=_batch_field(fields, "due") or None,
        deferred=_batch_bool(_batch_field(fields, "deferred")),
        origin=_batch_field(fields, "origin") or None,
    )


def _batch_field(fields: BatchFields, key: str) -> str:
    values = fields.get(key) or []
    return values[0] if values else ""


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
    default_origin: str | None = None,
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
            origin=request.origin or default_origin,
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
