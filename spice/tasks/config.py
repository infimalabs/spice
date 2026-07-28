"""Backend selection, paths, durable vocabulary, and taskrc generation.

A *backend* is one shared Taskwarrior database. Its root holds the generated
``taskrc`` and the single ``data/`` directory every agent in the backend
shares. The default root is the git common dir's ``.spice/`` state namespace.
Every worktree of a repository sees one board; there are no per-worktree
replicas and no sync server.
"""

from __future__ import annotations

import os
import re
import time
from contextlib import contextmanager
from pathlib import Path

from spice import defaults
from spice.errors import SpiceError
from spice.process.git import run_git_command
from spice.locking import bounded_exclusive_lock
from spice.paths import atomic_write_text, shared_state_root

TASK_BACKEND_ENV = "SPICE_TASK_BACKEND"  # env-policy: allow
PROJECT_SEGMENT_PATTERN = "[0-9a-z_]+"
PROJECT_SEGMENT_RULE_LABEL = "lowercase letters, digits, and underscores"
PROJECT_DELIMITER = "."
SEGMENT_RE = re.compile(rf"^{PROJECT_SEGMENT_PATTERN}$")
DEFAULT_PROJECT_MIN_DEPTH = defaults.integer("tasks", "project_min_depth")
DEFAULT_PROJECT_MAX_DEPTH = defaults.integer("tasks", "project_max_depth")
PHASE_MODELS_KEY = "phase_models"

# Durable vocabulary. `task` and `serve` ship with the harness; `agent` is
# reserved for automatic private task creation. Hidden system stems such as
# `.oops` are addressable but excluded from normal boards. A repo adds its own
# public stems, hidden stems, and per-stem default flows through the effective
# layered ``tasks`` table. These are operator-authored values, never invented
# by an agent.
BASE_APPROVED_STEMS = defaults.strings("tasks", "base_stems")
INTERNAL_STEMS = defaults.strings("tasks", "internal_stems")
MAXIM_PROPOSAL_HIDDEN_STEM = defaults.string("tasks", "maxim_proposal_hidden_stem")
BASE_HIDDEN_STEMS = defaults.strings("tasks", "hidden_stems")
HIDDEN_PROJECT_PREFIX = "."
APPROVED_PHASES = defaults.strings("tasks", "approved_phases")
PHASE_SLOT_COUNT = defaults.integer("tasks", "phase_slot_count")
TASK_EVENT_FILENAME = "events"
TASK_EVENT_LOCK_FILENAME = ".events.lock"
DEFAULT_FLOW = defaults.strings("tasks", "default_flow")
PRIVATE_DEFAULT_FLOW = defaults.strings("tasks", "private_default_flow")
# The hidden .oops triage project defaults to a lone plan phase: an oops item is
# a deferred speed bump claimed in place when triage is directed, so it starts
# in plan and decomposes into dependent public tasks. Oops identity rides the .oops
# project stem (.oops and its .oops.* descendants), never a tag or an
# APPROVED_PHASES entry.
OOPS_DEFAULT_FLOW = defaults.strings("tasks", "oops_default_flow")
TASK_CREATION_SURFACE_UDA = "creation_surface"
TASK_CREATION_SURFACE_CLI = "cli"
TASK_WORDING_REVIEW_UDA = "wording_review"
TASK_READY_AT_UDA = "ready_at"
# Task-document identity is system-owned: authoring surfaces never write or
# edit these fields; apply writes them atomically when it creates a row.
TASKDOC_ID_UDA = "taskdoc_id"
TASKDOC_PARENT_UDA = "taskdoc_parent"
TASKDOC_SYSTEM_UDAS = frozenset({TASKDOC_ID_UDA, TASKDOC_PARENT_UDA})

SENTINEL_ACTOR = "00000000-0000-0000-0000-000000000000"
DEFERRED_WAIT = defaults.string("tasks", "deferred_wait")
OOPS_WAIT_SECONDS = defaults.integer("tasks", "oops_wait_seconds")
OOPS_PROJECT = f".{defaults.string('tasks', 'oops_hidden_stem')}"
MAXIM_PROPOSAL_PROJECT = f".{MAXIM_PROPOSAL_HIDDEN_STEM}"

# Spice extends Taskwarrior's native priority UDA to C/H/M/L (or unset).
DEFAULT_PRIORITY = defaults.string("tasks", "default_priority")
PRIORITY_MAP = {
    str(key): str(value) for key, value in defaults.table("tasks", "priority").items()
}
PRIORITY_URGENCY = {
    str(key): float(value)
    for key, value in defaults.table("tasks", "priority_urgency").items()
}
TASKWARRIOR_URGENCY = {
    str(key): value
    for key, value in defaults.table("tasks", "taskwarrior_urgency").items()
}
ALLOCATOR_BAND_WIDTH = defaults.number("tasks", "allocator_band_width")
ALLOCATOR_ANTI_SELF_REVIEW = defaults.number("tasks", "allocator_anti_self_review")
SEVERITY_PRIORITY = {
    str(key): str(value)
    for key, value in defaults.table("tasks", "severity_priority").items()
}
SEVERITIES = defaults.strings("tasks", "severities")
SEVERITY_SHORTHANDS = {
    str(key): str(value)
    for key, value in defaults.table("tasks", "severity_shorthands").items()
}
SLA_DUE_SECONDS = {
    str(key): int(value)
    for key, value in defaults.table("tasks", "sla_due_seconds").items()
}

CLAIM_TTL_SECONDS = defaults.integer("tasks", "claim_ttl_seconds")
CLAIM_CONTEXT_SECONDS = defaults.integer("tasks", "claim_context_seconds")

_DURATION_RE = re.compile(r"^(\d+)([smhdw])$")
_DURATION_UNIT = {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}

# Named reports so a maintainer can explain the allocator with raw
# Taskwarrior. name -> (description, filter, sort).
REPORTS = {
    str(name): (
        str(raw["description"]),
        str(raw["filter"]),
        str(raw["sort"]),
    )
    for name, raw in defaults.table("tasks", "reports").items()
}
ANALYTICS_COMMANDS = defaults.strings("tasks", "analytics", "commands")
_REPORT_COLUMNS = "id,project,phase,priority,urgency,claim_by,description"
_REPORT_LABELS = "ID,Project,Phase,Pri,Urg,Claim,Description"


def private_project(actor: str) -> str:
    alnum_actor = re.sub(r"[^0-9a-z]", "", actor.lower())
    return f"agent.{alnum_actor}.task"


def approved_stems() -> tuple[str, ...]:
    return _approved_stems(_tasks_config_table())


def _approved_stems(table: dict[str, object]) -> tuple[str, ...]:
    extras = _configured_extra_stems(table)
    merged: list[str] = list(BASE_APPROVED_STEMS)
    for stem in extras:
        if stem not in merged:
            merged.append(stem)
    return tuple(merged)


def assignable_stems() -> tuple[str, ...]:
    return _assignable_stems(approved_stems())


def _assignable_stems(stems: tuple[str, ...]) -> tuple[str, ...]:
    return tuple(stem for stem in stems if stem not in INTERNAL_STEMS)


def _configured_extra_stems(table: dict[str, object]) -> tuple[str, ...]:
    from spice.config.layers import config_string_list

    return tuple(
        stem
        for stem in config_string_list(table.get("stems"))
        if SEGMENT_RE.match(stem)
    )


def _configured_hidden_stems(
    table: dict[str, object], approved: tuple[str, ...]
) -> tuple[str, ...]:
    from spice.config.layers import config_string_list

    configured: list[str] = []
    approved_set = set(approved)
    for stem in config_string_list(table.get("hidden_stems")):
        if stem.startswith(HIDDEN_PROJECT_PREFIX):
            raise SpiceError(
                "[tool.spice.tasks].hidden_stems values omit the leading '.'; "
                f"use {stem[len(HIDDEN_PROJECT_PREFIX) :]!r} instead of {stem!r}"
            )
        if not SEGMENT_RE.match(stem):
            raise SpiceError(
                "[tool.spice.tasks].hidden_stems values must match "
                f"{PROJECT_SEGMENT_RULE_LABEL}; got {stem!r}"
            )
        if stem in approved_set:
            raise SpiceError(
                f"hidden project stem {stem!r} conflicts with an approved public "
                "project stem"
            )
        if stem not in configured:
            configured.append(stem)
    return tuple(configured)


def per_stem_flows() -> dict[str, tuple[str, ...]]:
    table = _tasks_config_table()
    return _configured_per_stem_flows(table, _approved_stems(table))


def _configured_per_stem_flows(
    table: dict[str, object], approved: tuple[str, ...]
) -> dict[str, tuple[str, ...]]:
    from spice.config.layers import config_string_list

    raw_flows = table.get("flows")
    if not isinstance(raw_flows, dict):
        return {}
    approved_set = set(approved)
    flows: dict[str, tuple[str, ...]] = {}
    for raw_stem, raw_flow in raw_flows.items():
        stem = str(raw_stem or "").strip()
        if not SEGMENT_RE.match(stem):
            raise SpiceError(
                f"flow stem {stem!r} must match {PROJECT_SEGMENT_RULE_LABEL}"
            )
        if stem not in approved_set:
            raise SpiceError(
                f"flow stem {stem!r} is not approved (approved: {', '.join(approved)})"
            )
        flows[stem] = tuple(_validate_flow_phases(config_string_list(raw_flow)))
    return flows


def _tasks_config_table(repo_root: Path | None = None) -> dict[str, object]:
    from spice.paths import repo_root_from_cwd
    from spice.config.layers import effective_table

    root = repo_root or repo_root_from_cwd()
    if root is None:
        return {}
    return effective_table(root, "tasks")


def phase_launch_overrides(repo_root: Path, driver: str, phase: str) -> dict[str, str]:
    """Tracked per-driver, per-phase launch override.

    Read from ``[tool.spice.tasks.phase_models.<driver>.<phase>]``; {} when
    the driver or phase has no entry, so an unmapped phase falls back to the
    driver's ordinary launch config.
    """
    if not driver or not phase:
        return {}
    table = _tasks_config_table(repo_root).get(PHASE_MODELS_KEY)
    if not isinstance(table, dict):
        return {}
    driver_table = table.get(driver)
    if not isinstance(driver_table, dict):
        return {}
    phase_table = driver_table.get(phase)
    if not isinstance(phase_table, dict):
        return {}
    return {
        key: str(phase_table[key]).strip()
        for key in ("model", "effort")
        if phase_table.get(key)
    }


def project_depth_bounds() -> tuple[int, int]:
    from spice.config.layers import contextualize_config_error
    from spice.paths import repo_root_from_cwd

    root = repo_root_from_cwd()
    try:
        table = _tasks_config_table(root)
        return _project_depth_bounds(table)
    except SpiceError as exc:
        if root is None:
            raise
        raise contextualize_config_error(root, exc, "tasks") from exc


def _project_depth_bounds(table: dict[str, object]) -> tuple[int, int]:
    min_depth = _configured_project_depth(
        table, "project_min_depth", DEFAULT_PROJECT_MIN_DEPTH
    )
    max_depth = _configured_project_depth(
        table, "project_max_depth", DEFAULT_PROJECT_MAX_DEPTH
    )
    if max_depth < min_depth:
        raise SpiceError(
            "[tool.spice.tasks].project_max_depth must be greater than or equal "
            "to project_min_depth"
        )
    return min_depth, max_depth


def _configured_project_depth(table: dict[str, object], key: str, default: int) -> int:
    raw = table.get(key)
    if raw is None:
        return default
    if isinstance(raw, bool) or not isinstance(raw, int) or raw < 1:
        raise SpiceError(f"[tool.spice.tasks].{key} must be a positive integer")
    return raw


def map_priority(raw: str) -> str:
    value = (raw or "").strip()
    if value.upper() in PRIORITY_URGENCY:
        return value.upper()
    mapped = PRIORITY_MAP.get(value.lower())
    if mapped is None:
        raise SpiceError(
            f"invalid priority {raw!r} (use critical/high/medium/low/none or C/H/M/L)"
        )
    return mapped


def map_severity(raw: str) -> str:
    value = (raw or "medium").strip()
    if value.lower() in SEVERITY_SHORTHANDS:
        return SEVERITY_SHORTHANDS[value.lower()]
    if value.lower() in SEVERITIES:
        return value.lower()
    raise SpiceError(
        f"invalid severity {raw!r} (use critical/high/medium/low or C/H/M/L)"
    )


def parse_duration(text: str) -> int:
    match = _DURATION_RE.match((text or "").strip())
    if not match:
        raise SpiceError(
            f"invalid duration: {text!r} (use forms like 30s, 5m, 2h, 1d, 1w)"
        )
    return int(match.group(1)) * _DURATION_UNIT[match.group(2)]


_STRING = "string"
_CLAIM = [
    "claim_by",
    "claim_at",
    "claim_until",
    "claim_thread",
    "claim_worktree",
    "claim_branch",
    "claim_head",
    "claim_lease_seconds",
    "claim_context_start",
    "claim_context_end",
    "claim_context_link",
    "claim_context_turn",
]
_REVIEW = ["review_author", "review_by", "review_at", "review_finding", "review_note"]
_TASK_DOCUMENT = [TASKDOC_ID_UDA, TASKDOC_PARENT_UDA]
_EVIDENCE = [
    "acceptance",
    "task_description",
    "validation",
    "judgment",
    "delete_reason",
    "origin",
    TASK_CREATION_SURFACE_UDA,
    TASK_WORDING_REVIEW_UDA,
    TASK_READY_AT_UDA,
    "origin_thread",
    "origin_worktree",
    "origin_branch",
    "done_head",
    "done_merge_head",
    "done_ref",
    "done_local_commits",
    "done_upstream",
    "done_upstream_head",
]

_backend_override: str | None = None
TASK_BOOTSTRAP_LOCK_TIMEOUT_SECONDS = 30.0


def set_backend(selector: str | None) -> None:
    global _backend_override
    _backend_override = (selector or "").strip() or None


def backend_override() -> str | None:
    """Return the effective explicit or environment-selected backend."""
    return _selector() or None


def _selector() -> str:
    if _backend_override is not None:
        return _backend_override
    return os.environ.get(TASK_BACKEND_ENV, "").strip()  # env-policy: allow


def repo_root() -> Path:
    result = run_git_command(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise SpiceError("not inside a git worktree")
    return Path(result.stdout.strip()).resolve()


def backend_root() -> Path:
    selector = _selector()
    if selector:
        expanded = Path(selector).expanduser()
        if expanded.is_absolute():
            return expanded.resolve()
        raise SpiceError(f"{TASK_BACKEND_ENV} requires an absolute path")
    return shared_state_root(repo_root())


def data_dir(root: Path | None = None) -> Path:
    return (root or backend_root()) / "data"


def taskrc_path(root: Path | None = None) -> Path:
    return (root or backend_root()) / "taskrc"


def task_event_path(root: Path | None = None) -> Path:
    return (root or backend_root()) / TASK_EVENT_FILENAME


def task_event_lock_path(root: Path | None = None) -> Path:
    return (root or backend_root()) / TASK_EVENT_LOCK_FILENAME


def bootstrap_lock_path(root: Path | None = None) -> Path:
    return (root or backend_root()) / ".bootstrap.lock"


@contextmanager
def _bootstrap_lock(root: Path | None = None):
    selected_root = root or backend_root()
    selected_root.mkdir(parents=True, exist_ok=True)
    with bounded_exclusive_lock(
        bootstrap_lock_path(selected_root),
        timeout_seconds=TASK_BOOTSTRAP_LOCK_TIMEOUT_SECONDS,
        action="bootstrap task backend",
    ):
        yield


def task_event_generation() -> str:
    """Mint the token every task-backend event is ordered by.

    Microseconds, not nanoseconds: this is the count every generation in the
    repo is minted in, so a reader that meets more than one of them meets one
    kind of token rather than one encoding per authority.
    """
    return str(time.time_ns() // 1000)


def ensure_task_event_file(root: Path | None = None) -> Path:
    path = task_event_path(root)
    if not path.exists():
        # A new store bootstraps at the instant it was created rather than at
        # zero. A store is only ever created after every store it replaces, so
        # starting from now is what keeps this revision rising across a store
        # that was deleted and remade -- a zero here reads as older than the
        # generation it replaced, and readers that keep the highest revision
        # they have seen would refuse this backend forever.
        atomic_write_text(
            path, f"{task_event_generation()} bootstrap\n", write_if_changed=True
        )
    return path


def task_event_revision(root: Path | None = None) -> str:
    """Return the task-only revision carried by the shared wake file."""
    try:
        text = ensure_task_event_file(root).read_text(encoding="utf-8")
    except OSError:
        return "0"
    token = (text.split(maxsplit=1) or ["0"])[0]
    return token if token.isdigit() else "0"


def mark_task_backend_changed(
    reason: str = "task", *, root: Path | None = None
) -> None:
    selected_root = root or backend_root()
    normalized_reason = reason.strip() or "task"
    selected_root.mkdir(parents=True, exist_ok=True)
    with bounded_exclusive_lock(
        task_event_lock_path(selected_root),
        timeout_seconds=TASK_BOOTSTRAP_LOCK_TIMEOUT_SECONDS,
        action="publish task backend event",
    ):
        event_revision = task_event_generation()
        task_revision = (
            task_event_revision(selected_root)
            if normalized_reason == "team"
            else event_revision
        )
        token = f"{task_revision} {event_revision} {os.getpid()} {normalized_reason}\n"
        atomic_write_text(
            task_event_path(selected_root),
            token,
            write_if_changed=True,
        )


def uda_schema() -> dict[str, dict[str, str]]:
    """Map of UDA name -> dotted-config fragments (type, optional values)."""
    enum = ",".join(APPROVED_PHASES)
    schema: dict[str, dict[str, str]] = {}
    schema["incepted"] = {"type": _STRING, "label": "Incepted"}
    schema["priority"] = {
        "type": _STRING,
        "label": "Priority",
        "values": ",".join((*PRIORITY_URGENCY, "")),
    }
    schema["phase"] = {"type": _STRING, "label": "Phase", "values": enum}
    schema["phase_i"] = {"type": "numeric", "label": "PhaseIndex"}
    for i in range(PHASE_SLOT_COUNT):
        schema[f"phase_{i}"] = {"type": _STRING, "label": f"Phase{i}", "values": enum}
    for name in (*_CLAIM, *_REVIEW, *_TASK_DOCUMENT, *_EVIDENCE):
        schema[name] = {"type": _STRING, "label": name}
    return schema


def materialize_task_backend(root: Path) -> Path:
    """Create one explicit backend's native Taskwarrior config and data dir."""
    selected_root = root.expanduser()
    if not selected_root.is_absolute():
        raise SpiceError(f"{TASK_BACKEND_ENV} requires an absolute path")
    selected_root = selected_root.resolve()
    selected_data_dir = data_dir(selected_root)
    selected_taskrc = taskrc_path(selected_root)
    with _bootstrap_lock(selected_root):
        selected_data_dir.mkdir(parents=True, exist_ok=True)
        lines = [
            f"data.location={selected_data_dir}",
            "confirmation=no",
            "verbose=nothing",
            "recurrence=no",
            "# spice native urgency: every dimension is deliberate; graph "
            "position is ranked by the allocator.",
        ]
        lines.extend(
            f"urgency.{name}={value}" for name, value in TASKWARRIOR_URGENCY.items()
        )
        lines.append(
            "# spice priority urgency: adjacent tiers stay wider than the "
            "allocator comparison band."
        )
        lines.extend(
            f"urgency.uda.priority.{priority}.coefficient={coefficient}"
            for priority, coefficient in PRIORITY_URGENCY.items()
        )
        for name, frag in sorted(uda_schema().items()):
            for key, value in frag.items():
                lines.append(f"uda.{name}.{key}={value}")
        lines.extend(_report_lines())
        atomic_write_text(
            selected_taskrc,
            "\n".join(lines) + "\n",
            write_if_changed=True,
        )
    return selected_taskrc


def write_taskrc() -> None:
    from spice.paths import shared_attachment_root

    materialize_task_backend(backend_root())
    shared_attachment_root(repo_root()).mkdir(parents=True, exist_ok=True)


def _report_lines() -> list[str]:
    lines: list[str] = []
    for name, (desc, filt, sort) in REPORTS.items():
        lines.append(f"report.{name}.description={desc}")
        lines.append(f"report.{name}.filter={filt}")
        lines.append(f"report.{name}.columns={_REPORT_COLUMNS}")
        lines.append(f"report.{name}.labels={_REPORT_LABELS}")
        lines.append(f"report.{name}.sort={sort}")
    return lines


def bootstrap() -> Path:
    """Ensure the backend taskrc + data dir exist; return the taskrc path."""
    write_taskrc()
    return taskrc_path()


def _project_parts(project: str) -> tuple[bool, list[str]]:
    project = (project or "").strip()
    if not project:
        raise SpiceError("project must be non-empty")
    hidden = project.startswith(HIDDEN_PROJECT_PREFIX)
    body = project[len(HIDDEN_PROJECT_PREFIX) :] if hidden else project
    if not body:
        raise SpiceError("hidden project must name a stem after '.'")
    segments = body.split(PROJECT_DELIMITER)
    for seg in segments:
        if not SEGMENT_RE.match(seg):
            raise SpiceError(
                f"project segment {seg!r} must match [0-9a-z_] (project {project!r})"
            )
    return hidden, segments


def _normalized_project(hidden: bool, segments: list[str]) -> str:
    body = PROJECT_DELIMITER.join(segments)
    return f"{HIDDEN_PROJECT_PREFIX}{body}" if hidden else body


def project_stem(project: str) -> str:
    _hidden, segments = _project_parts(project)
    return segments[0]


def hidden_stems() -> tuple[str, ...]:
    table = _tasks_config_table()
    return _hidden_stems(table, _approved_stems(table))


def _hidden_stems(
    table: dict[str, object], approved: tuple[str, ...]
) -> tuple[str, ...]:
    merged: list[str] = list(BASE_HIDDEN_STEMS)
    for stem in _configured_hidden_stems(table, approved):
        if stem not in merged:
            merged.append(stem)
    return tuple(merged)


def is_hidden_project(project: str) -> bool:
    try:
        hidden, segments = _project_parts(project)
    except SpiceError:
        return False
    return hidden and segments[0] in hidden_stems()


def is_oops_project(project: str) -> bool:
    """The hidden `.oops` triage project and its `.oops.*` descendants.

    The oops stem acts as a trailing-wildcard prefix: a row classifies as oops
    from its project alone when its stem is `oops`, whether it sits at `.oops`
    itself or at any `.oops.<kind>` descendant.
    """
    try:
        hidden, segments = _project_parts(project)
    except SpiceError:
        return False
    return hidden and segments[0] == project_stem(OOPS_PROJECT)


def validate_project(project: str) -> str:
    hidden, segments = _project_parts(project)
    if hidden:
        stems = hidden_stems()
        if segments[0] not in stems:
            raise SpiceError(
                f"hidden project stem {segments[0]!r} is not approved "
                f"(hidden: {', '.join(stems)})"
            )
        return _normalized_project(hidden, segments)
    stems = approved_stems()
    if segments[0] not in stems:
        raise SpiceError(
            f"project stem {segments[0]!r} is not approved "
            f"(approved: {', '.join(stems)})"
        )
    return _normalized_project(hidden, segments)


def validate_assignable_project(project: str) -> str:
    project = validate_project(project)
    hidden, segments = _project_parts(project)
    stem = segments[0]
    if hidden:
        raise SpiceError(
            f"hidden project stem {stem!r} is not lane-filter assignable "
            f"(assignable: {', '.join(assignable_stems())})"
        )
    if stem not in assignable_stems():
        raise SpiceError(
            f"project stem {stem!r} is internal and cannot be lane-filter assigned "
            f"(assignable: {', '.join(assignable_stems())})"
        )
    return project


def validate_manual_creation_project(project: str) -> str:
    project = validate_project(project)
    hidden, segments = _project_parts(project)
    stem = segments[0]
    if hidden:
        raise SpiceError(
            f"hidden project stem {stem!r} is reserved for system task creation; "
            f"use an assignable project such as {_project_example()}"
        )
    if stem in INTERNAL_STEMS:
        if stem != "agent":
            raise SpiceError(
                f"project stem {stem!r} is reserved for system task creation; "
                f"use an assignable project such as {_project_example()}"
            )
        raise SpiceError(
            f"project stem {stem!r} is reserved for automatic private task creation; "
            f"omit --project for private work or use an assignable project such as "
            f"{_project_example()}"
        )
    _validate_public_task_project_depth(project, segments)
    return project


def _validate_public_task_project_depth(project: str, segments: list[str]) -> None:
    min_depth, max_depth = project_depth_bounds()
    depth = len(segments)
    if depth < min_depth:
        raise SpiceError(
            f"project {project!r} has depth {depth}; public task projects require "
            f"at least {min_depth} dotted segments, such as "
            f"{_project_example(segments[0], min_depth, max_depth)}"
        )
    if depth > max_depth:
        raise SpiceError(
            f"project {project!r} has depth {depth}; public task projects allow "
            f"at most {max_depth} dotted segments, such as "
            f"{_project_example(segments[0], min_depth, max_depth)}"
        )


def _project_example(
    stem: str | None = None,
    min_depth: int | None = None,
    max_depth: int | None = None,
) -> str:
    if stem is None:
        stem = assignable_stems()[0]
    if min_depth is None or max_depth is None:
        min_depth, max_depth = project_depth_bounds()
    target_depth = max(min_depth, 2)
    if target_depth > max_depth:
        target_depth = max_depth
    suffix_count = max(0, target_depth - 1)
    suffixes: list[str] = list(("example", "unit", "work", "item")[:suffix_count])
    while len(suffixes) < suffix_count:
        suffixes.append(f"level{len(suffixes) + 1}")
    segments = [stem, *suffixes]
    return PROJECT_DELIMITER.join(segments)


def is_internal_or_hidden_project(project: str) -> bool:
    return is_hidden_project(project) or project_stem(project) in INTERNAL_STEMS


def task_project_validation_catalog() -> dict[str, object]:
    """Return the lane-filter assignable task project vocabulary for serve."""
    from spice.config.layers import contextualize_config_error
    from spice.paths import repo_root_from_cwd

    root = repo_root_from_cwd()
    try:
        return _task_project_validation_catalog(_tasks_config_table(root))
    except SpiceError as exc:
        if root is None:
            raise
        raise contextualize_config_error(root, exc, "tasks") from exc


def _task_project_validation_catalog(
    table: dict[str, object],
) -> dict[str, object]:
    approved = _approved_stems(table)
    stems = _assignable_stems(approved)
    hidden = _hidden_stems(table, approved)
    flows = _configured_per_stem_flows(table, approved)
    min_depth, max_depth = _project_depth_bounds(table)
    return {
        "approvedStems": list(stems),
        "hiddenStems": list(hidden),
        "approvedPhases": list(APPROVED_PHASES),
        "defaultFlow": list(DEFAULT_FLOW),
        "perStemFlows": {stem: list(flow) for stem, flow in sorted(flows.items())},
        "hiddenProjectPrefix": HIDDEN_PROJECT_PREFIX,
        "projectDelimiter": PROJECT_DELIMITER,
        "projectMinDepth": min_depth,
        "projectMaxDepth": max_depth,
        "segmentPattern": PROJECT_SEGMENT_PATTERN,
        "segmentRuleLabel": PROJECT_SEGMENT_RULE_LABEL,
        "projectExamples": [
            _project_example(stem, min_depth, max_depth) for stem in stems
        ],
    }


def resolve_flow(flow: list[str] | None, project: str | None) -> list[str]:
    phases: list[str]
    stem = project_stem(project) if project else ""
    if flow:
        phases = [p.strip() for p in flow if p.strip()]
    elif project and is_hidden_project(project):
        phases = (
            list(OOPS_DEFAULT_FLOW)
            if stem == project_stem(OOPS_PROJECT)
            else list(PRIVATE_DEFAULT_FLOW)
        )
    elif stem in INTERNAL_STEMS:
        phases = list(PRIVATE_DEFAULT_FLOW)
    else:
        configured_flows = per_stem_flows()
        phases = (
            list(configured_flows[stem])
            if stem in configured_flows
            else list(DEFAULT_FLOW)
        )
    return _validate_flow_phases(phases)


def _validate_flow_phases(phases: list[str]) -> list[str]:
    if not phases:
        raise SpiceError("flow has no phases")
    if len(phases) > PHASE_SLOT_COUNT:
        raise SpiceError(f"flow exceeds {PHASE_SLOT_COUNT} phases: {phases}")
    for phase in phases:
        if phase not in APPROVED_PHASES:
            raise SpiceError(
                f"phase {phase!r} is not approved "
                f"(approved: {', '.join(APPROVED_PHASES)})"
            )
    return phases
