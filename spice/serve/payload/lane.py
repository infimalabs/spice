"""Lane status, inventory, and metrics payload builders."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.serve import messages as message_reader
from spice.serve.payload.identity import (
    _agent_name_for_target,
    _binding_status,
    team_actor_for_target,
)
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import identity as task_identity
from spice.tasks import config as task_config
from spice.tasks import tw

LANE_METRIC_SPARKLINE_BUCKETS = 12


LANE_METRIC_SPARKLINE_BUCKET_SECONDS = 60


TASK_ACTOR_FIELDS = ("claim_by", "claim_thread", "review_author", "review_by")
REVIEW_PRESSURE_LIMIT = 3


def task_filter_inventory() -> dict[str, Any]:
    """Open-task counts per assignable project, plus system header signals."""
    revision = task_filter_inventory_revision()
    catalog = task_config.task_project_validation_catalog()
    filters: list[dict[str, Any]] = []
    stems: dict[str, dict[str, Any]] = {}
    from spice.errors import SpiceError
    from spice.tasks import tw

    try:
        rows = tw.export(["(", "status:pending", "or", "status:waiting", ")"])
    except SpiceError:
        # No Taskwarrior (or no backend yet): the lane UI still works; the
        # filter inventory is simply empty.
        rows = []
    counts: dict[str, int] = {}
    waiting_count = 0
    oops_count = 0
    for row in rows:
        project = str(row.get("project") or "")
        raw_tags = row.get("tags") or []
        tags = {raw_tags} if isinstance(raw_tags, str) else set(raw_tags)
        is_oops = (
            str(row.get(task_config.PROJECT_HIDDEN_UDA) or "") == "1"
            or task_config.is_hidden_project(project)
            or task_config.HIDDEN_TASK_TAG in tags
            or "oops" in tags
            or project
            == task_config.OOPS_PROJECT.lstrip(task_config.HIDDEN_PROJECT_PREFIX)
        )
        if is_oops:
            oops_count += 1
            continue
        if str(row.get("status") or "pending") == "waiting":
            waiting_count += 1
            continue
        if project:
            counts[project] = counts.get(project, 0) + 1
    assignable_stems = set(task_config.assignable_stems())
    visible_stems = set(task_config.approved_stems())
    for project, count in sorted(counts.items()):
        stem = project.split(".", 1)[0]
        if stem not in visible_stems:
            continue
        entry = stems.setdefault(
            stem, {"name": stem, "openTaskCount": 0, "filters": []}
        )
        entry["openTaskCount"] += count
        if stem not in assignable_stems:
            continue
        filters.append({"name": project, "primaryStem": stem, "openTaskCount": count})
        if project not in entry["filters"]:
            entry["filters"].append(project)
    if waiting_count:
        stems["waiting"] = {
            "name": "waiting",
            "openTaskCount": waiting_count,
            "filters": [],
            "waitingTaskCount": waiting_count,
        }
    if oops_count:
        stems["oops"] = {
            "name": "oops",
            "openTaskCount": oops_count,
            "filters": [],
            "oopsTaskCount": oops_count,
        }
    return {
        "revision": revision,
        "filters": filters,
        "primaryStems": list(stems.values()),
        "openTaskCount": sum(item["openTaskCount"] for item in filters),
        "catalog": {
            "approvedStems": catalog["approvedStems"],
            "hiddenStems": catalog["hiddenStems"],
            "approvedPhases": catalog["approvedPhases"],
            "defaultFlow": catalog["defaultFlow"],
            "perStemFlows": catalog["perStemFlows"],
            "hiddenProjectPrefix": catalog["hiddenProjectPrefix"],
            "filterDelimiter": catalog["projectDelimiter"],
            "segmentPattern": catalog["segmentPattern"],
            "segmentRuleLabel": catalog["segmentRuleLabel"],
            "filterExamples": catalog["projectExamples"],
        },
    }


def task_filter_inventory_revision() -> str:
    """Return the task event token that makes task-filter inventories comparable."""
    try:
        text = task_config.ensure_task_event_file().read_text(encoding="utf-8")
    except OSError:
        return "0"
    token = (text.split(maxsplit=1) or ["0"])[0]
    return token if token.isdigit() else "0"


def status_line_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    items: list[message_reader.AssistantMessage],
    error: str | None,
    pending_count: int | None = None,
    pending_identity: dict[str, Any] | None = None,
) -> dict[str, Any]:
    status = agent_status(target.repo_root)
    binding_error = agent_binding_error(target.repo_root, status)
    pending = pending_identity or pending_inbox_identity_payload(target.repo_root)
    if pending_count is not None:
        pending = {
            **pending,
            "pendingInboxCount": pending_count,
            "pendingInboxLabel": str(pending_count),
        }
    return _status_line_payload_from_status(
        status=status,
        thread_id=status.thread_id,
        binding_error=binding_error,
        items=items,
        error=error,
        pending_identity=pending,
    )


def _status_line_payload_from_status(
    *,
    status: Any,
    thread_id: str,
    binding_error: str,
    items: list[message_reader.AssistantMessage],
    error: str | None,
    pending_identity: dict[str, Any],
) -> dict[str, Any]:
    thread_id = thread_id or ""
    visible = [item for item in items if not item.kind.startswith("presence:")]
    latest = visible[0] if visible else None
    latest_activity = items[0] if items else None
    binding_status = _binding_status(thread_id, binding_error)
    latest_status = latest_activity or latest
    return {
        "bindingStatus": binding_status,
        "bound": bool(thread_id),
        "bindingError": binding_error,
        "rolloutStatus": "error" if binding_error or error else "ok",
        "activityStatus": message_reader.activity_status(items),
        "lastAssistantAt": latest_status.timestamp if latest_status else "",
        "latestMessagePreview": latest.preview if latest else "",
        "latestActivityPreview": (latest_activity.preview if latest_activity else ""),
        "preview": latest_status.preview if latest_status else "",
        **pending_identity,
        "agentProcessStatus": status.process_status,
        "agentVisualStatus": status.process_status,
        "error": binding_error or error or "",
    }


def _lane_info_payload(
    target: WorktreeTarget, serve_identity: dict[str, Any]
) -> dict[str, Any]:
    agent_name = _agent_name_for_target(target)
    thread_id = str((serve_identity.get("thread") or {}).get("threadId") or "")
    driver = serve_identity.get("driver") or {}
    desired_driver = str(driver.get("desired") or "")
    actual_driver = str(driver.get("actual") or "")
    session_owner = str(driver.get("transcriptOwner") or "")
    launch = serve_identity.get("launch") or {}
    desired_launch = launch.get("desired") or {}
    actual_launch = launch.get("actual") or {}
    rows = [
        {"key": "agent", "value": agent_name or "-", "span": False},
        *_identity_value_rows("driver", actual_driver, desired_driver),
        *_identity_value_rows(
            "model",
            str(actual_launch.get("model") or ""),
            str(desired_launch.get("model") or ""),
        ),
        *_identity_value_rows(
            "effort",
            str(actual_launch.get("effort") or ""),
            str(desired_launch.get("effort") or ""),
        ),
        {"key": "target", "value": target.id, "span": False},
        {"key": "worktree", "value": target.name or "-", "span": False},
        {"key": "path", "value": str(target.repo_root), "span": True},
        {"key": "branch", "value": target.branch or "-", "span": False},
        {"key": "thread", "value": thread_id or "-", "span": True},
        {"key": "session", "value": session_owner or "-", "span": False},
    ]
    review_pressure = review_pressure_payload(serve_identity)
    if review_pressure["count"]:
        rows.append(
            {
                "key": "review pressure",
                "value": _review_pressure_summary(review_pressure),
                "span": True,
            }
        )
    return {"summaryRows": rows, "members": [], "reviewPressure": review_pressure}


def _identity_value_rows(
    key: str,
    actual: str,
    desired: str,
) -> list[dict[str, Any]]:
    actual = str(actual or "").strip()
    desired = str(desired or "").strip()
    if actual and desired and actual != desired:
        return [
            {"key": f"{key} actual", "value": actual, "span": False},
            {"key": f"{key} desired", "value": desired, "span": False},
        ]
    return [{"key": key, "value": desired or actual or "-", "span": False}]


def review_pressure_payload(serve_identity: dict[str, Any]) -> dict[str, Any]:
    """Recent non-clean task reviews for the lane actor."""
    actors = _review_pressure_actor_keys(serve_identity)
    if not actors:
        return _empty_review_pressure()
    from spice.errors import SpiceError

    try:
        completed = tw.export(["status:completed"])
        open_rows = tw.export(["(", "status:pending", "or", "status:waiting", ")"])
    except SpiceError:
        return _empty_review_pressure()
    followups_by_reviewed = _review_followup_counts(open_rows)
    reviewed_rows = [
        row
        for row in completed
        if str(row.get("review_author") or "") in actors
        and _review_finding_is_pressure(row.get("review_finding"))
    ]
    reviewed_rows.sort(key=_review_pressure_sort_key, reverse=True)
    items = [
        _review_pressure_item(row, followups_by_reviewed)
        for row in reviewed_rows[:REVIEW_PRESSURE_LIMIT]
    ]
    return {
        "count": len(reviewed_rows),
        "openFollowupCount": sum(
            followups_by_reviewed.get(str(row.get("uuid") or ""), 0)
            for row in reviewed_rows
        ),
        "items": items,
    }


def _empty_review_pressure() -> dict[str, Any]:
    return {"count": 0, "openFollowupCount": 0, "items": []}


def _review_pressure_actor_keys(serve_identity: dict[str, Any]) -> set[str]:
    thread = serve_identity.get("thread") or {}
    values = [
        serve_identity.get("actorId"),
        thread.get("threadId"),
    ]
    keys: set[str] = set()
    for value in values:
        text = str(value or "").strip()
        if not text:
            continue
        keys.add(text)
        if text.startswith("thread:") or text.startswith("target:"):
            keys.add(text.split(":", 1)[1])
    return {key for key in keys if key}


def _review_finding_is_pressure(value: Any) -> bool:
    finding = str(value or "").strip().casefold()
    return bool(finding and finding != "clean")


def _review_followup_counts(rows: list[dict[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for row in rows:
        for dep in _task_dependencies(row):
            counts[dep] = counts.get(dep, 0) + 1
    return counts


def _task_dependencies(row: dict[str, Any]) -> set[str]:
    value = row.get("depends") or []
    if isinstance(value, str):
        return {item.strip() for item in value.split(",") if item.strip()}
    if isinstance(value, list):
        return {str(item).strip() for item in value if str(item).strip()}
    return set()


def _review_pressure_sort_key(row: dict[str, Any]) -> str:
    return str(
        row.get("review_at")
        or row.get("end")
        or row.get("modified")
        or row.get("entry")
        or ""
    )


def _review_pressure_item(
    row: dict[str, Any],
    followups_by_reviewed: dict[str, int],
) -> dict[str, Any]:
    finding = str(row.get("review_finding") or "").strip()
    uuid = str(row.get("uuid") or "")
    return {
        "reviewedTask": task_identity.render_handle(row),
        "finding": finding,
        "findingSeverity": _review_finding_severity(finding),
        "reviewer": str(row.get("review_by") or ""),
        "source": "task-review",
        "followupCount": followups_by_reviewed.get(uuid, 0),
        "reviewedAt": str(row.get("review_at") or ""),
    }


def _review_finding_severity(finding: str) -> str:
    value = finding.strip().casefold()
    if value in {"changes", "blocked"}:
        return value
    return "attention"


def _review_pressure_summary(pressure: dict[str, Any]) -> str:
    items = pressure.get("items") or []
    if not items:
        return "-"
    first = items[0]
    reviewed = str(first.get("reviewedTask") or "task")
    severity = str(first.get("findingSeverity") or first.get("finding") or "review")
    reviewer = str(first.get("reviewer") or "").strip()
    source = str(first.get("source") or "").strip()
    origin = ""
    if reviewer and source:
        origin = f" by {reviewer} via {source}"
    elif reviewer:
        origin = f" by {reviewer}"
    elif source:
        origin = f" via {source}"
    followups = int(first.get("followupCount") or 0)
    suffix = f"; {followups} follow-up" + ("" if followups == 1 else "s")
    more = int(pressure.get("count") or 0) - 1
    if more > 0:
        suffix += f"; +{more} more"
    return f"{severity} on {reviewed}{origin}{suffix}"


def lane_metrics_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str,
    items: list[message_reader.AssistantMessage],
    status: Any,
) -> dict[str, Any]:
    """Lane counters from durable per-agent metrics plus live process uptime."""
    actor = team_actor_for_target(state.team_store, target, thread_id)
    summary = state.team_store.lane_metric_summary(
        actor,
        bucket_count=LANE_METRIC_SPARKLINE_BUCKETS,
        bucket_seconds=LANE_METRIC_SPARKLINE_BUCKET_SECONDS,
    )
    return {
        "drained": _drained_task_count(thread_id),
        "acked": summary.acked,
        "sends": summary.sends,
        "toolCalls": summary.tool_calls,
        "uptimeSeconds": agent_uptime_seconds(status, items),
        "sparkline": list(summary.sparkline),
    }


def _drained_task_count(thread_id: str) -> int:
    from spice.errors import SpiceError
    from spice.tasks import tw

    actor = tw.canonical_actor(thread_id) if thread_id else ""
    if not actor:
        return 0
    try:
        rows = tw.export(["status:completed"])
    except SpiceError:
        # No Taskwarrior (or no backend yet): the rest of the metrics pane
        # still works; nothing has been drained through the board.
        return 0
    return sum(
        1
        for row in rows
        if any(str(row.get(field) or "") == actor for field in TASK_ACTOR_FIELDS)
    )


def agent_uptime_seconds(
    status: Any, items: list[message_reader.AssistantMessage]
) -> int:
    if not status.running or not status.started_at:
        return 0
    started = message_reader.parse_timestamp(status.started_at)
    if started is None:
        return 0
    latest = _latest_message_timestamp(items) or datetime.now(UTC)
    return max(0, int((latest - started).total_seconds()))


def _latest_message_timestamp(
    items: list[message_reader.AssistantMessage],
) -> datetime | None:
    timestamps = _message_timestamps(items)
    return max(timestamps) if timestamps else None


def _message_timestamps(
    items: list[message_reader.AssistantMessage],
) -> list[datetime]:
    return [
        parsed
        for item in items
        if (parsed := message_reader.parse_timestamp(item.timestamp)) is not None
    ]
