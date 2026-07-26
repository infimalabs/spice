"""Lane status, inventory, and metrics payload builders."""

from __future__ import annotations

from collections.abc import Mapping
from datetime import UTC, datetime
from typing import Any

from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.serve import messages as message_reader
from spice.serve.payload.chrome import (
    LaneChromeObservation,
    LaneChromeOrder,
    assemble_lane_chrome,
)
from spice.serve.payload.identity import (
    _agent_name_for_target,
    _binding_status,
    team_actor_for_target,
)
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.taskboard import OpenTaskBoardProjection, open_task_board_projection
from spice.serve.team.history import ObservationAttributionMode
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import identity as task_identity
from spice.transcript.timestamps import parse_timestamp

LANE_METRIC_SPARKLINE_BUCKETS = 12


LANE_METRIC_SPARKLINE_BUCKET_SECONDS = 60


REVIEW_PRESSURE_LIMIT = 3


def status_line_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    items: list[message_reader.AssistantMessage],
    error: str | None,
    pending_count: int | None = None,
    pending_identity: dict[str, Any] | None = None,
    task_board: OpenTaskBoardProjection | None = None,
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
        active_claims=task_board,
    )


def _claimed_task_payload(
    thread_id: str, *, claims: OpenTaskBoardProjection | None = None
) -> dict[str, str]:
    if not thread_id:
        return {}
    projection = claims or open_task_board_projection()
    row = projection.active_claim(thread_id)
    if row is None:
        return {}
    handle = task_identity.render_handle(row)
    phase = str(row.get("phase") or "").strip()
    title = " ".join(str(row.get("description") or "").split())
    return {"handle": handle, "phase": phase, "title": title}


def _status_line_payload_from_status(
    *,
    status: Any,
    thread_id: str,
    binding_error: str,
    items: list[message_reader.AssistantMessage],
    error: str | None,
    pending_identity: dict[str, Any],
    active_claims: OpenTaskBoardProjection | None = None,
) -> dict[str, Any]:
    thread_id = thread_id or ""
    visible = [item for item in items if not item.kind.startswith("presence:")]
    latest = visible[0] if visible else None
    latest_activity = items[0] if items else None
    binding_status = _binding_status(thread_id, binding_error)
    latest_status = latest_activity or latest
    latest_activity_kind = latest_status.kind if latest_status else ""
    return {
        "bindingStatus": binding_status,
        "bound": bool(thread_id),
        "bindingError": binding_error,
        "rolloutStatus": "error" if binding_error or error else "ok",
        "activityStatus": message_reader.activity_status(items),
        "lastAssistantAt": latest_status.timestamp if latest_status else "",
        "latestActivityKind": latest_activity_kind,
        "latestMessagePreview": latest.preview if latest else "",
        "latestActivityPreview": (latest_activity.preview if latest_activity else ""),
        "preview": latest_status.preview if latest_status else "",
        **pending_identity,
        "agentProcessStatus": status.process_status,
        "agentVisualStatus": _agent_visual_status(
            status.process_status, latest_activity_kind
        ),
        "claimedTask": _claimed_task_payload(thread_id, claims=active_claims),
        "error": binding_error or error or "",
    }


def _agent_visual_status(process_status: str, latest_activity_kind: str) -> str:
    if process_status == "running" and latest_activity_kind == "final":
        return "idle"
    return process_status


def lane_chrome_payload(
    *,
    target_id: str,
    team_identity: Mapping[str, Any] | None = None,
    team_facts: Mapping[str, Any] | None = None,
    renewal_intent: Mapping[str, Any] | None = None,
    task_filter_inventory: Mapping[str, Any] | None = None,
    pending_identity: Mapping[str, Any] | None = None,
    last_assistant_at: str | None = None,
) -> dict[str, Any]:
    """Project the chrome facets one pass actually observed.

    Every producer that observes these facts routes through here, so inventory,
    lane pushes, and route feedback answer with one payload built one way
    instead of each hand-assembling the same fields.

    A caller passes only what it observed, and only whole facets are published:
    a route that resolved team facts but read no inbox says nothing about the
    pending inbox rather than restating a value it did not look at. Omitting a
    facet leaves whatever the client already holds, which is what keeps a
    narrow route reply from recopying a lane's whole chrome.

    Facets whose authority keeps no counter are never published, because a
    producer that cannot say which of two observations is newer must not
    publish either. That is why ``identity`` and ``lifecycle`` are absent
    today; WEB-1kGvkZqD settles the epoch source that lets those two join.
    """
    observations: list[LaneChromeObservation] = []
    team_revision = int((team_identity or {}).get("teamRevision", 0))
    if team_identity is not None:
        observations.append(
            LaneChromeObservation(
                "teamConfig",
                # Team revision is the store's global event counter for this
                # team. It advances for membership, config, and renewal
                # mutations, whereas configRevision advances for config only.
                LaneChromeOrder(revision=team_revision),
                {"teamIdentity": dict(team_identity)},
            )
        )
    if pending_identity is not None:
        observations.append(
            LaneChromeObservation(
                "pendingInbox",
                LaneChromeOrder(revision=int(pending_identity["pendingInboxVersion"])),
                {
                    "count": int(pending_identity["pendingInboxCount"]),
                    "label": str(pending_identity["pendingInboxLabel"]),
                    "keys": pending_identity["pendingInboxKeys"],
                },
            )
        )
    if team_facts is not None and task_filter_inventory is not None:
        observations.append(
            LaneChromeObservation(
                "taskBoard",
                # The joined lane board changes when either authority feeding
                # it changes: task rows/catalog advance the board epoch, while
                # this team's filters/lifetime advance the team revision.
                LaneChromeOrder(
                    epoch=str(task_filter_inventory.get("revision", "")),
                    revision=team_revision,
                ),
                {
                    "taskFilters": team_facts.get("taskFilters", []),
                    "taskFilterEntries": team_facts.get("taskFilterEntries", []),
                    "effectiveTaskFilters": team_facts.get("effectiveTaskFilters", []),
                    "taskFilterInventory": task_filter_inventory,
                    # The filters above are observed; no server pass counts a
                    # lane's private tasks yet. The board's shape requires a
                    # number, so it publishes the only one nothing contradicts.
                    "privateTaskCount": 0,
                },
            )
        )
    if team_facts is not None and renewal_intent is not None:
        observations.append(
            LaneChromeObservation(
                "renewal",
                # Lifetime and renewal intent are one team-store observation.
                # Both mutations advance the team's event revision; max keeps
                # a legacy renewal row carrying a later explicit revision
                # ordered without inventing a second browser authority.
                LaneChromeOrder(
                    revision=max(
                        team_revision,
                        int(renewal_intent.get("revision", 0)),
                    )
                ),
                {
                    "lifetime": team_facts.get("lifetime", ""),
                    "renewalIntent": dict(renewal_intent),
                },
            )
        )
    if last_assistant_at is not None:
        # The transcript's own last assistant instant is what advances here:
        # zero-padded, so it orders naturally, and it moves exactly when the
        # activity the facet describes does.
        observations.append(
            LaneChromeObservation(
                "activity",
                LaneChromeOrder(epoch=str(last_assistant_at)),
                {"lastAssistantAt": last_assistant_at},
            )
        )
    return assemble_lane_chrome(target_id, observations).payload


def _lane_info_payload(
    target: WorktreeTarget,
    serve_identity: dict[str, Any],
    *,
    agent_name: str | None = None,
    task_board: OpenTaskBoardProjection | None = None,
) -> dict[str, Any]:
    # ``agent_name`` lets the inventory caller pass the say-voice name it already
    # resolved for this target so the lane build reuses it instead of resolving
    # the same config a second time. Left unset, it resolves normally.
    if agent_name is None:
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
    review_pressure = review_pressure_payload(serve_identity, task_board=task_board)
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


def review_pressure_payload(
    serve_identity: dict[str, Any],
    *,
    task_board: OpenTaskBoardProjection | None = None,
) -> dict[str, Any]:
    """Recent non-clean task reviews for the lane actor."""
    actors = _review_pressure_actor_keys(serve_identity)
    if not actors:
        return _empty_review_pressure()
    projection = task_board or open_task_board_projection()
    reviewed_rows = [
        row
        for row in projection.completed_review_rows(actors)
        if _review_finding_is_pressure(row.get("review_finding"))
    ]
    items = [
        _review_pressure_item(row, projection)
        for row in reviewed_rows[:REVIEW_PRESSURE_LIMIT]
    ]
    return {
        "count": len(reviewed_rows),
        "openFollowupCount": sum(
            projection.open_review_followup_count(str(row.get("uuid") or ""))
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


def _review_pressure_item(
    row: Mapping[str, Any],
    task_board: OpenTaskBoardProjection,
) -> dict[str, Any]:
    finding = str(row.get("review_finding") or "").strip()
    uuid = str(row.get("uuid") or "")
    return {
        "reviewedTask": task_identity.render_handle(row),
        "finding": finding,
        "findingSeverity": _review_finding_severity(finding),
        "reviewer": str(row.get("review_by") or ""),
        "source": "task-review",
        "followupCount": task_board.open_review_followup_count(uuid),
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
    task_board: OpenTaskBoardProjection | None = None,
) -> dict[str, Any]:
    """Lane counters from durable per-agent metrics plus live process uptime."""
    actor = team_actor_for_target(state.team_store, target, thread_id)
    summary = state.team_store.lane_metric_summary(
        actor,
        bucket_count=LANE_METRIC_SPARKLINE_BUCKETS,
        bucket_seconds=LANE_METRIC_SPARKLINE_BUCKET_SECONDS,
        attribution=ObservationAttributionMode.LINEAGE_CUMULATIVE,
    )
    return {
        "drained": (task_board or open_task_board_projection()).drained_task_count(
            thread_id
        ),
        "acked": summary.acked,
        "sends": summary.sends,
        "toolCalls": summary.tool_calls,
        "uptimeSeconds": agent_uptime_seconds(status, items),
        "sparkline": list(summary.sparkline),
    }


def agent_uptime_seconds(
    status: Any, items: list[message_reader.AssistantMessage]
) -> int:
    if not status.running or not status.started_at:
        return 0
    started = parse_timestamp(status.started_at)
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
        if (parsed := parse_timestamp(item.timestamp)) is not None
    ]
