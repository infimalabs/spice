"""Lane and topology payloads: what the UI knows, assembled server-side."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

from spice.agent.identity import canonical_thread_id
from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.config import configured_say_voice
from spice.mail.inbox import (
    collect_archived_inbox_items,
    collect_inbox_items,
    inbox_item_key_aliases,
    inbox_request_body,
    inbox_request_priority,
    pending_inbox_count,
)
from spice.serve.attachments import inbox_attachment_payloads
from spice.serve import messages as message_reader
from spice.serve.agentapi import (
    ensure_agent_for_pending_inbox,
    pending_inbox_count_after_agent_ensure,
)
from spice.serve.markdown import render_message_html
from spice.serve.teams import ServeTeamStore, renewal_intent_payload
from spice.serve.worktrees import WorktreeTarget
from spice.tasks import config as task_config

ACK_CONTEXT_ARCHIVE_LIMIT = 50

LANE_METRIC_SPARKLINE_BUCKETS = 12
LANE_METRIC_SPARKLINE_BUCKET_SECONDS = 60
TASK_ACTOR_FIELDS = ("claim_by", "claim_thread", "review_author", "review_by")


def task_filter_inventory() -> dict[str, Any]:
    """Open-task counts per assignable project, plus the validation catalog."""
    catalog = task_config.task_project_validation_catalog()
    filters: list[dict[str, Any]] = []
    stems: dict[str, dict[str, Any]] = {}
    from spice.errors import SpiceError
    from spice.tasks import tw

    try:
        rows = tw.export(["status:pending"])
    except SpiceError:
        # No Taskwarrior (or no backend yet): the lane UI still works; the
        # filter inventory is simply empty.
        rows = []
    counts: dict[str, int] = {}
    for row in rows:
        project = str(row.get("project") or "")
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
    return {
        "filters": filters,
        "primaryStems": list(stems.values()),
        "openTaskCount": sum(item["openTaskCount"] for item in filters),
        "catalog": {
            "approvedStems": catalog["approvedStems"],
            "approvedPhases": catalog["approvedPhases"],
            "defaultFlow": catalog["defaultFlow"],
            "perStemFlows": catalog["perStemFlows"],
            "filterDelimiter": catalog["projectDelimiter"],
            "segmentPattern": catalog["segmentPattern"],
            "segmentRuleLabel": catalog["segmentRuleLabel"],
            "filterExamples": catalog["projectExamples"],
        },
    }


def resolve_thread_id_for_target(state: Any, target: WorktreeTarget) -> str | None:
    thread_id = canonical_thread_id(agent_status(target.repo_root).thread_id)
    with state.cache_lock:
        if thread_id:
            state.cached_thread_ids[target.id] = thread_id
            return thread_id
        return state.cached_thread_ids.get(target.id)


def team_facts_for_actor(store: ServeTeamStore, actor: str) -> dict[str, Any]:
    if not actor:
        return {}
    team_id = store.current_team_for_agent(actor)
    if team_id is None:
        return {}
    team = store.team_state(team_id)
    return {
        "teamId": team.team_id,
        "teamRevision": team.revision,
        "configRevision": team.config_revision,
        "taskFilters": list(team.config.task_filters),
        "lifetime": team.config.lifetime,
        "renewalIntent": renewal_intent_for_actor(store, actor),
    }


def renewal_intent_for_actor(store: ServeTeamStore, actor: str) -> dict[str, Any]:
    if not actor:
        return renewal_intent_payload(None)
    return renewal_intent_payload(
        store.renewal_state_for_agent(actor),
        agent_id=actor,
    )


def target_activity_items(
    target: WorktreeTarget, thread_id: str
) -> tuple[list[message_reader.AssistantMessage], str | None]:
    if not thread_id:
        return [], None
    return message_reader.assistant_messages_for_thread_id(
        thread_id,
        limit=1,
        worktree_id=target.id,
        repo_root=target.repo_root,
    )


def status_line_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    items: list[message_reader.AssistantMessage],
    error: str | None,
    pending_count: int | None = None,
) -> dict[str, Any]:
    status = agent_status(target.repo_root)
    binding_error = agent_binding_error(target.repo_root, status)
    pending = (
        pending_count
        if pending_count is not None
        else pending_inbox_count(target.repo_root)
    )
    return _status_line_payload_from_status(
        status=status,
        thread_id=status.thread_id,
        binding_error=binding_error,
        items=items,
        error=error,
        pending=pending,
    )


def _status_line_payload_from_status(
    *,
    status: Any,
    thread_id: str,
    binding_error: str,
    items: list[message_reader.AssistantMessage],
    error: str | None,
    pending: int,
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
        "pendingInboxCount": pending,
        "pendingInboxLabel": str(pending),
        "agentProcessStatus": status.process_status,
        "agentVisualStatus": status.process_status,
        "error": binding_error or error or "",
    }


def work_trees_payload(state: Any) -> dict[str, Any]:
    targets = state.worktree_targets()
    inventory = task_filter_inventory()
    work_trees = []
    for target in targets:
        pending = pending_inbox_count(target.repo_root)
        agent_ensure = ensure_agent_for_pending_inbox(
            target,
            pending,
            attempt_cache=state.pending_agent_ensure_attempts,
        )
        pending = pending_inbox_count_after_agent_ensure(pending, agent_ensure)
        thread_id = resolve_thread_id_for_target(state, target) or ""
        status = agent_status(target.repo_root)
        binding_error = agent_binding_error(target.repo_root, status)
        binding_status = _binding_status(thread_id, binding_error)
        team_facts = team_facts_for_actor(state.team_store, thread_id)
        renewal_intent = renewal_intent_for_actor(state.team_store, thread_id)
        items, error = target_activity_items(target, thread_id)
        status_line = _status_line_payload_from_status(
            status=status,
            thread_id=thread_id,
            binding_error=binding_error,
            items=items,
            error=error,
            pending=pending,
        )
        work_trees.append(
            {
                "id": target.id,
                "repoRoot": str(target.repo_root),
                "displayName": target.display_name,
                "branch": target.branch or target.name,
                "agentName": _agent_name_for_target(target),
                "threadId": thread_id,
                "taskFilters": team_facts.get("taskFilters", []),
                "laneFilterVersion": "",
                "teamId": team_facts.get("teamId", ""),
                "teamRevision": team_facts.get("teamRevision", 0),
                "configRevision": team_facts.get("configRevision", 0),
                "lifetime": team_facts.get("lifetime", ""),
                "renewalIntent": renewal_intent,
                "taskFilterInventory": inventory,
                "laneInfo": _lane_info_payload(target, thread_id),
                "bindingStatus": binding_status,
                "bindingError": binding_error,
                "pendingCount": pending,
                "pendingLabel": str(pending),
                "privateTaskCount": 0,
                "agentProcessStatus": status.process_status,
                "agentVisualStatus": status_line["agentVisualStatus"],
                "agentEnsure": agent_ensure or {},
                "lastAssistantAt": status_line["lastAssistantAt"],
                "statusLine": status_line,
            }
        )
    return {
        "workTrees": work_trees,
        "defaultTargetId": targets[0].id if targets else "",
        "taskFilterInventory": inventory,
    }


def _agent_name_for_target(target: WorktreeTarget) -> str:
    """The agent's voice name; empty when no voice is configured."""
    voice = configured_say_voice(target.repo_root)
    if not voice:
        return ""
    return voice.split("(", 1)[0].strip()


def _binding_status(thread_id: str, binding_error: str) -> str:
    if binding_error:
        return "mismatch"
    return "bound" if thread_id else "unbound"


def _lane_info_payload(target: WorktreeTarget, thread_id: str) -> dict[str, Any]:
    agent_name = _agent_name_for_target(target)
    rows = [
        {"key": "agent", "value": agent_name or "-", "span": False},
        {"key": "target", "value": target.id, "span": False},
        {"key": "worktree", "value": target.name or "-", "span": False},
        {"key": "path", "value": str(target.repo_root), "span": True},
        {"key": "branch", "value": target.branch or "-", "span": False},
        {"key": "thread", "value": thread_id or "-", "span": True},
    ]
    return {"summaryRows": rows, "members": []}


def lane_metrics_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str,
    items: list[message_reader.AssistantMessage],
    status: Any,
) -> dict[str, Any]:
    """Lane counters from durable per-agent metrics plus live process uptime."""
    summary = state.team_store.lane_metric_summary(
        thread_id,
        bucket_count=LANE_METRIC_SPARKLINE_BUCKETS,
        bucket_seconds=LANE_METRIC_SPARKLINE_BUCKET_SECONDS,
    )
    return {
        "drained": _drained_task_count(thread_id),
        "acked": summary.acked,
        "sends": summary.sends,
        "toolCalls": summary.tool_calls,
        "uptimeSeconds": _agent_uptime_seconds(status, items),
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


def _agent_uptime_seconds(
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


def _message_sparkline(items: list[message_reader.AssistantMessage]) -> list[int]:
    values = [0] * LANE_METRIC_SPARKLINE_BUCKETS
    timestamps = _message_timestamps(items)
    if not timestamps:
        return values
    start = max(timestamps).timestamp() - (
        (LANE_METRIC_SPARKLINE_BUCKETS - 1) * LANE_METRIC_SPARKLINE_BUCKET_SECONDS
    )
    for timestamp in timestamps:
        index = int(
            (timestamp.timestamp() - start) // LANE_METRIC_SPARKLINE_BUCKET_SECONDS
        )
        values[max(0, min(index, LANE_METRIC_SPARKLINE_BUCKETS - 1))] += 1
    return values


def messages_payload_for_worktree(
    state: Any,
    target: WorktreeTarget,
    *,
    limit: int,
    after: str | None = None,
    before: str | None = None,
    expected_thread_id: str | None = None,
    fast_mode: bool = False,
) -> dict[str, Any]:
    thread_id = (
        canonical_thread_id(expected_thread_id or "")
        or resolve_thread_id_for_target(state, target)
        or ""
    )
    if not thread_id:
        items: list[message_reader.AssistantMessage] = []
        error: str | None = "No agent thread is bound to this worktree yet."
    else:
        items, error = message_reader.assistant_messages_for_thread_id(
            thread_id,
            limit=limit,
            after=after,
            before=before,
            cursor=state.rollout_cursor(thread_id) if not before else None,
            worktree_id=target.id,
            repo_root=target.repo_root,
        )
    pending = pending_inbox_count(target.repo_root)
    agent_ensure = ensure_agent_for_pending_inbox(
        target,
        pending,
        attempt_cache=state.pending_agent_ensure_attempts,
        fast_mode=fast_mode,
    )
    pending = pending_inbox_count_after_agent_ensure(pending, agent_ensure)
    team_facts = team_facts_for_actor(state.team_store, thread_id)
    renewal_intent = renewal_intent_for_actor(state.team_store, thread_id)
    status = agent_status(target.repo_root)
    return {
        "messages": [item.to_payload() for item in items],
        "targetWorktreeName": target.name,
        "targetBranch": target.branch or target.name,
        "targetAgentName": _agent_name_for_target(target),
        "targetThreadId": thread_id,
        "taskFilters": team_facts.get("taskFilters", []),
        "laneFilterVersion": "",
        "teamId": team_facts.get("teamId", ""),
        "teamRevision": team_facts.get("teamRevision", 0),
        "configRevision": team_facts.get("configRevision", 0),
        "lifetime": team_facts.get("lifetime", ""),
        "renewalIntent": renewal_intent,
        "taskFilterInventory": task_filter_inventory(),
        "laneMetrics": lane_metrics_payload(
            state, target, thread_id=thread_id, items=items, status=status
        ),
        "laneInfo": _lane_info_payload(target, thread_id),
        "agentProcessStatus": status.process_status,
        "error": error or "",
        "pendingInboxCount": pending,
        "agentEnsure": agent_ensure or {},
        "statusLine": status_line_payload(
            state, target, items=items, error=error, pending_count=pending
        ),
    }


def ack_context_payload_for_worktree(
    state: Any, target: WorktreeTarget, *, keys: list[str]
) -> dict[str, Any]:
    """Resolve sent-steering context for ACK keys the UI wants to quote.

    Pending and recently archived inbox items are the source of truth. The
    assistant's ACK reply is not operator context and must not be quoted back as
    if the operator wrote it.
    """
    wanted = [key for key in keys if key]
    by_key: dict[str, dict[str, Any]] = {}
    pending = collect_inbox_items(str(target.repo_root))
    archived = collect_archived_inbox_items(
        str(target.repo_root), limit=ACK_CONTEXT_ARCHIVE_LIMIT
    )
    for item in (*pending, *archived):
        item_aliases = inbox_item_key_aliases(item.name)
        matching_keys = [
            key
            for key in wanted
            if key not in by_key and inbox_item_key_aliases(key) & item_aliases
        ]
        if matching_keys:
            body = inbox_request_body(item.text)
            html = render_message_html(body, worktree_id=target.id)
            priority = inbox_request_priority(item.text) or ""
            attachments = inbox_attachment_payloads(
                item.attachments,
                repo_root=target.repo_root,
                worktree_id=target.id,
            )
            for key in matching_keys:
                by_key[key] = {
                    "key": key,
                    "found": True,
                    "text": body,
                    "html": html,
                    "priority": priority,
                    "attachments": attachments,
                }
    acks = [by_key.get(key, {"key": key, "found": False}) for key in wanted]
    return {"ok": True, "acks": acks}
