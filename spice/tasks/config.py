"""Backend selection, paths, durable vocabulary, and taskrc generation.

A *backend* is one shared Taskwarrior database. Its root holds the generated
``taskrc`` and the single ``data/`` directory every agent in the backend
shares. The directory lives in the git common dir (named ``task``, or
``task-<name>`` for a named backend) so every worktree of a repository sees
one board; there are no per-worktree replicas and no sync server.
"""

from __future__ import annotations

import os
import re
import subprocess
from contextlib import contextmanager
from pathlib import Path

from spice.errors import SpiceError
from spice.locking import lock_fd_exclusive, unlock_fd

TASK_BACKEND_ENV = "SPICE_TASK_BACKEND"  # env-policy: allow
# All spice git-dir state lives under the `spice/` namespace (sticky study
# state shares it), so a repo can host other tooling without collisions.
SHARED_DIR = "spice/task"
BACKEND_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
PROJECT_SEGMENT_PATTERN = "[0-9a-z_]+"
PROJECT_SEGMENT_RULE_LABEL = "lowercase letters, digits, and underscores"
PROJECT_DELIMITER = "."
SEGMENT_RE = re.compile(rf"^{PROJECT_SEGMENT_PATTERN}$")

# Durable vocabulary. `task` and `serve` ship with the harness; `agent` is
# reserved for automatic private task creation. A repo adds its own stems and
# per-stem default flows through tracked `pyproject.toml` (`[tool.spice.tasks]`),
# edited by a human — never invented by an agent.
BASE_APPROVED_STEMS = ("task", "serve", "agent")
INTERNAL_STEMS = ("agent",)
APPROVED_PHASES = ("todo", "verify", "review", "oops")
PHASE_SLOT_COUNT = 7  # phase_0 .. phase_6
DEFAULT_FLOW = ("todo", "review")
PRIVATE_DEFAULT_FLOW = ("todo",)
PER_STEM_FLOWS: dict[str, tuple[str, ...]] = {}

SENTINEL_ACTOR = "00000000-0000-0000-0000-000000000000"
OOPS_WAIT = "2099-01-01T00:00:00"
OOPS_PROJECT = "task.oops"

# Native Taskwarrior priorities (H/M/L, or unset). Word aliases map to them.
DEFAULT_PRIORITY = "medium"
PRIORITY_MAP = {
    "critical": "H",
    "high": "H",
    "medium": "M",
    "low": "L",
    "none": "",
    "": "",
}
SEVERITY_PRIORITY = {"critical": "H", "high": "H", "medium": "M", "low": "L"}
SEVERITIES = ("low", "medium", "high", "critical")
SEVERITY_SHORTHANDS = {"h": "high", "m": "medium", "l": "low"}
SLA_DUE_SECONDS = {
    "H": 86400,  # high/critical: tomorrow
    "M": 604800,  # medium: one week
    "L": 2592000,  # low: thirty days
}

CLAIM_TTL_SECONDS = 3600  # a claim is stale once its deadline elapses
CLAIM_CONTEXT_SECONDS = 300  # claim rehydration window: five minutes around claim

_DURATION_RE = re.compile(r"^(\d+)([smhdw])$")
_DURATION_UNIT = {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}

# Named reports so a maintainer can explain the allocator with raw
# Taskwarrior. name -> (description, filter, sort).
REPORTS = {
    "oready": ("spice ready queue", "status:pending +READY -oops", "urgency-"),
    "oreview": ("spice review queue", "status:pending phase:review", "urgency-"),
    "oactive": ("spice active claims", "status:pending +ACTIVE", "claim_at+"),
    "oblocked": ("spice blocked", "status:pending +BLOCKED", "urgency-"),
    "owaiting": ("spice waiting/deferred", "+WAITING", "wait+"),
    "ooops": ("spice oops triage", "+oops -COMPLETED -DELETED", "urgency-"),
}
ANALYTICS_COMMANDS = ("history", "burndown.daily", "burndown.weekly")
_REPORT_COLUMNS = "id,project,phase,priority,urgency,claim_by,description"
_REPORT_LABELS = "ID,Project,Phase,Pri,Urg,Claim,Description"


def approved_stems() -> tuple[str, ...]:
    extras = _configured_extra_stems()
    merged = list(BASE_APPROVED_STEMS)
    for stem in extras:
        if stem not in merged:
            merged.append(stem)
    return tuple(merged)


def assignable_stems() -> tuple[str, ...]:
    return tuple(stem for stem in approved_stems() if stem not in INTERNAL_STEMS)


def _configured_extra_stems() -> tuple[str, ...]:
    from spice.paths import repo_root_from_cwd
    from spice.repocfg import string_list, tasks_table

    root = repo_root_from_cwd()
    if root is None:
        return ()
    return tuple(
        stem
        for stem in string_list(tasks_table(root).get("stems"))
        if SEGMENT_RE.match(stem)
    )


def per_stem_flows() -> dict[str, tuple[str, ...]]:
    flows = dict(PER_STEM_FLOWS)
    flows.update(_configured_per_stem_flows())
    return flows


def _configured_per_stem_flows() -> dict[str, tuple[str, ...]]:
    from spice.paths import repo_root_from_cwd
    from spice.repocfg import string_list, tasks_table

    root = repo_root_from_cwd()
    if root is None:
        return {}
    raw_flows = tasks_table(root).get("flows")
    if not isinstance(raw_flows, dict):
        return {}
    approved = approved_stems()
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
        flows[stem] = tuple(_validate_flow_phases(string_list(raw_flow)))
    return flows


def map_priority(raw: str) -> str:
    value = (raw or "").strip()
    if value.upper() in ("H", "M", "L"):
        return value.upper()
    mapped = PRIORITY_MAP.get(value.lower())
    if mapped is None:
        raise SpiceError(
            f"invalid priority {raw!r} (use high/medium/low/none or H/M/L)"
        )
    return mapped


def map_severity(raw: str) -> str:
    value = (raw or "medium").strip()
    if value.lower() in SEVERITY_SHORTHANDS:
        return SEVERITY_SHORTHANDS[value.lower()]
    if value.lower() in SEVERITIES:
        return value.lower()
    raise SpiceError(
        f"invalid severity {raw!r} (use critical/high/medium/low or H/M/L)"
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
    "claim_context_start",
    "claim_context_end",
    "claim_context_link",
    "claim_context_turn",
]
_REVIEW = ["review_author", "review_by", "review_at", "review_finding", "review_note"]
_EVIDENCE = [
    "acceptance",
    "task_description",
    "validation",
    "judgment",
    "delete_reason",
    "pace",
    "origin_thread",
    "origin_worktree",
    "origin_branch",
    "done_head",
    "done_merge_head",
    "done_ref",
    "done_upstream",
    "done_upstream_head",
]

_backend_override: str | None = None


def set_backend(selector: str | None) -> None:
    global _backend_override
    _backend_override = (selector or "").strip() or None


def _selector() -> str:
    if _backend_override is not None:
        return _backend_override
    return os.environ.get(TASK_BACKEND_ENV, "").strip()


def repo_root() -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise SpiceError("not inside a git worktree")
    return Path(result.stdout.strip()).resolve()


def git_common_dir(root: Path) -> Path:
    result = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--git-common-dir"],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise SpiceError("not inside a git worktree")
    raw = Path(result.stdout.strip())
    return (raw if raw.is_absolute() else root / raw).resolve()


def backend_root() -> Path:
    selector = _selector()
    if selector:
        expanded = Path(selector).expanduser()
        if expanded.is_absolute():
            return expanded.resolve()
        if not BACKEND_NAME_RE.match(selector):
            raise SpiceError(f"invalid backend name: {selector!r}")
        return git_common_dir(repo_root()) / f"{SHARED_DIR}-{selector}"
    return git_common_dir(repo_root()) / SHARED_DIR


def data_dir() -> Path:
    return backend_root() / "data"


def taskrc_path() -> Path:
    return backend_root() / "taskrc"


def bootstrap_lock_path() -> Path:
    return backend_root() / ".bootstrap.lock"


@contextmanager
def _bootstrap_lock():
    backend_root().mkdir(parents=True, exist_ok=True)
    with bootstrap_lock_path().open("a", encoding="utf-8") as handle:
        lock_fd_exclusive(handle.fileno(), blocking=True)
        try:
            yield
        finally:
            unlock_fd(handle.fileno())


def _atomic_write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(f"{path.suffix}.{os.getpid()}.tmp")
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, path)


def uda_schema() -> dict[str, dict[str, str]]:
    """Map of UDA name -> dotted-config fragments (type, optional values)."""
    enum = ",".join(APPROVED_PHASES)
    schema: dict[str, dict[str, str]] = {}
    schema["incepted"] = {"type": _STRING, "label": "Incepted"}
    schema["phase"] = {"type": _STRING, "label": "Phase", "values": enum}
    schema["phase_i"] = {"type": "numeric", "label": "PhaseIndex"}
    for i in range(PHASE_SLOT_COUNT):
        schema[f"phase_{i}"] = {"type": _STRING, "label": f"Phase{i}", "values": enum}
    for name in (*_CLAIM, *_REVIEW, *_EVIDENCE):
        schema[name] = {"type": _STRING, "label": name}
    return schema


def write_taskrc() -> None:
    with _bootstrap_lock():
        data_dir().mkdir(parents=True, exist_ok=True)
        lines = [
            f"data.location={data_dir()}",
            "confirmation=no",
            "verbose=nothing",
            "recurrence=no",
            "# spice phase-review urgency: peer review rises fleet-wide.",
            "urgency.uda.phase.review.coefficient=4.0",
        ]
        for name, frag in sorted(uda_schema().items()):
            for key, value in frag.items():
                lines.append(f"uda.{name}.{key}={value}")
        lines.extend(_report_lines())
        _atomic_write_text(taskrc_path(), "\n".join(lines) + "\n")


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


def validate_project(project: str) -> str:
    project = (project or "").strip()
    if not project:
        raise SpiceError("project must be non-empty")
    segments = project.split(".")
    for seg in segments:
        if not SEGMENT_RE.match(seg):
            raise SpiceError(
                f"project segment {seg!r} must match [0-9a-z_] (project {project!r})"
            )
    stems = approved_stems()
    if segments[0] not in stems:
        raise SpiceError(
            f"project stem {segments[0]!r} is not approved "
            f"(approved: {', '.join(stems)})"
        )
    return project


def validate_assignable_project(project: str) -> str:
    project = validate_project(project)
    stem = project.split(PROJECT_DELIMITER, 1)[0]
    if stem not in assignable_stems():
        raise SpiceError(
            f"project stem {stem!r} is internal and cannot be lane-filter assigned "
            f"(assignable: {', '.join(assignable_stems())})"
        )
    return project


def validate_manual_creation_project(project: str) -> str:
    project = validate_project(project)
    stem = project.split(PROJECT_DELIMITER, 1)[0]
    if stem in INTERNAL_STEMS:
        raise SpiceError(
            f"project stem {stem!r} is reserved for automatic private task creation; "
            f"omit --project for private work or use an assignable stem "
            f"({', '.join(assignable_stems())})"
        )
    return project


def is_internal_project_stem(stem: str) -> bool:
    return stem in INTERNAL_STEMS


def task_project_validation_catalog() -> dict[str, object]:
    """Return the lane-filter assignable task project vocabulary for serve."""
    stems = assignable_stems()
    flows = per_stem_flows()
    return {
        "approvedStems": list(stems),
        "approvedPhases": list(APPROVED_PHASES),
        "defaultFlow": list(DEFAULT_FLOW),
        "perStemFlows": {stem: list(flow) for stem, flow in sorted(flows.items())},
        "projectDelimiter": PROJECT_DELIMITER,
        "segmentPattern": PROJECT_SEGMENT_PATTERN,
        "segmentRuleLabel": PROJECT_SEGMENT_RULE_LABEL,
        "projectExamples": [f"{stem}.example" for stem in stems],
    }


def resolve_flow(flow: list[str] | None, project: str | None) -> list[str]:
    phases: list[str]
    stem = project.split(PROJECT_DELIMITER, 1)[0] if project else ""
    if flow:
        phases = [p.strip() for p in flow if p.strip()]
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
