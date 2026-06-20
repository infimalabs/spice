"""Lane and topology payloads: what the UI knows, assembled server-side."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

from spice.agent.identity import canonical_thread_id
from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.agent.renewal import strip_renewal_handoff_request_suffix
from spice.config import configured_say_voice
from spice.errors import SpiceError
from spice.mail.inbox import (
    collect_acked_inbox_items,
    collect_inbox_items,
    inbox_item_key_aliases,
    inbox_request_body,
    inbox_request_priority,
)
from spice.serve.attachments import inbox_attachment_payloads
from spice.serve import messages as message_reader
from spice.serve.agentapi import (
    ensure_agent_for_pending_inbox,
)
from spice.serve.markdown import render_message_html
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.teams import ServeTeamStore, renewal_intent_payload
from spice.serve.worktrees import WorktreeTarget
from spice.tasks import config as task_config
from spice.tasks import identity as task_identity
from spice.tasks import tw

ACK_CONTEXT_ARCHIVE_LIMIT = 50

LANE_METRIC_SPARKLINE_BUCKETS = 12
LANE_METRIC_SPARKLINE_BUCKET_SECONDS = 60
TASK_ACTOR_FIELDS = ("claim_by", "claim_thread", "review_author", "review_by")
TASK_CARD_SOURCE_KIND = "cli_task_created"


def agent_ensure_thread_id(agent_ensure: dict[str, Any] | None) -> str:
    if not isinstance(agent_ensure, dict):
        return ""
    return canonical_thread_id(agent_ensure.get("threadId") or "")


def record_started_renewal_from_ensure(
    store: ServeTeamStore,
    *,
    predecessor_agent_id: str,
    agent_ensure: dict[str, Any] | None,
) -> str:
    successor_agent_id = agent_ensure_thread_id(agent_ensure)
    if not predecessor_agent_id or not successor_agent_id:
        return successor_agent_id
    if successor_agent_id == predecessor_agent_id:
        return successor_agent_id
    if not store.agent_renewal_active(predecessor_agent_id):
        return successor_agent_id
    try:
        store.record_started_renewal(
            predecessor_agent_id=predecessor_agent_id,
            successor_agent_id=successor_agent_id,
            ancestor_thread_id=predecessor_agent_id,
        )
    except SpiceError:
        pass
    return successor_agent_id


def task_filter_inventory() -> dict[str, Any]:
    """Open-task counts per assignable project, plus system header signals."""
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
        if project == task_config.OOPS_PROJECT and not row.get("start"):
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
        "taskFilterEntries": [
            entry.to_payload() for entry in team.config.task_filter_entries
        ],
        "lifetime": team.config.lifetime,
        "renewalIntent": renewal_intent_for_actor(store, actor),
    }


def team_actor_for_target(
    store: ServeTeamStore, target: WorktreeTarget, thread_id: str | None
) -> str:
    """Return the single durable team actor for a target.

    A target id is only a placeholder before the worktree has a thread. Once a
    real thread exists, any placeholder membership is rewritten to that thread
    before callers read team facts.
    """
    actor = canonical_thread_id(thread_id or "")
    if not actor:
        return target.id
    if target.id and target.id != actor:
        placeholder_team = store.current_team_for_agent(target.id)
        if placeholder_team is not None:
            store.assign_agent(placeholder_team, actor, aliases=[target.id])
    return actor


def team_facts_for_target(
    store: ServeTeamStore, target: WorktreeTarget, thread_id: str | None
) -> dict[str, Any]:
    return team_facts_for_actor(store, team_actor_for_target(store, target, thread_id))


def renewal_intent_for_target(
    store: ServeTeamStore, target: WorktreeTarget, thread_id: str | None
) -> dict[str, Any]:
    actor = team_actor_for_target(store, target, thread_id)
    return renewal_intent_for_actor(store, actor)


def target_identity_payload(
    target: WorktreeTarget,
    thread_id: str,
    *,
    binding_status: str = "",
    binding_error: str = "",
    agent_name: str | None = None,
) -> dict[str, Any]:
    status = binding_status or ("bound" if thread_id else "unbound")
    payload = {
        "targetId": _required_identity_string(target.id, "target id"),
        "worktreeName": _required_identity_string(target.name, "worktree name"),
        "branch": _required_identity_string(
            target.branch or target.name,
            "target branch",
        ),
        "agent": _agent_identity_payload(
            _agent_name_for_target(target) if agent_name is None else agent_name
        ),
        "thread": _thread_identity_payload(
            thread_id,
            binding_status=status,
            binding_error=binding_error,
        ),
    }
    return payload


def team_identity_payload(team_facts: dict[str, Any]) -> dict[str, Any]:
    team_id = str(team_facts.get("teamId") or "").strip()
    if not team_id:
        return {"state": "none"}
    return {
        "state": "member",
        "teamId": _required_identity_string(team_id, "team id"),
        "teamRevision": _nonnegative_payload_int(
            team_facts.get("teamRevision"),
            "team revision",
        ),
        "configRevision": _nonnegative_payload_int(
            team_facts.get("configRevision"),
            "config revision",
        ),
    }


def _agent_identity_payload(agent_name: str) -> dict[str, str]:
    name = str(agent_name or "").strip()
    if not name:
        return {"state": "unconfigured"}
    return {
        "state": "configured",
        "name": _required_identity_string(name, "agent name"),
    }


def _thread_identity_payload(
    thread_id: str,
    *,
    binding_status: str,
    binding_error: str = "",
) -> dict[str, str]:
    state = str(binding_status or "").strip()
    if state == "unbound":
        return {"state": "unbound"}
    if state == "mismatch":
        payload = {"state": "mismatch"}
        thread = str(thread_id or "").strip()
        if thread:
            payload["threadId"] = _required_identity_string(thread, "thread id")
        error = str(binding_error or "").strip()
        if error:
            payload["error"] = error
        return payload
    if state != "bound":
        raise SpiceError(f"invalid thread identity state: {state or '-'}")
    payload = {
        "state": state,
        "threadId": _required_identity_string(thread_id, "thread id"),
    }
    error = str(binding_error or "").strip()
    if error:
        payload["error"] = error
    return payload


def _required_identity_string(value: Any, label: str) -> str:
    text = str(value or "").strip()
    if not text:
        raise SpiceError(f"{label} must be non-empty in identity payload")
    return text


def _nonnegative_payload_int(value: Any, label: str) -> int:
    if value is None or value == "":
        raise SpiceError(f"{label} is required in identity payload")
    number = int(value)
    if number < 0:
        raise SpiceError(f"{label} must be non-negative in identity payload")
    return number


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
    items, error = message_reader.assistant_messages_for_thread_id(
        thread_id,
        limit=1,
        worktree_id=target.id,
        repo_root=target.repo_root,
    )
    return _merge_task_card_messages(thread_id, items, limit=1), error


def _merge_task_card_messages(
    thread_id: str,
    items: list[message_reader.AssistantMessage],
    *,
    limit: int,
    after: str | None = None,
    before: str | None = None,
) -> list[message_reader.AssistantMessage]:
    card_after = after
    if card_after is None and before is None and items:
        visible_items = [
            item for item in items if not item.kind.startswith("presence:")
        ]
        oldest = _oldest_message(visible_items or items)
        if oldest is not None:
            card_after = oldest.key
    cards = _task_card_messages_for_thread(thread_id, after=card_after, before=before)
    if not cards:
        return items
    bounded = max(1, min(limit, message_reader.MAX_MESSAGE_LIMIT))
    merged = {item.key: item for item in (*items, *cards)}
    values = _filter_non_offset_boundary(
        list(merged.values()),
        after=after,
        before=before,
    )
    presence = [item for item in values if item.kind.startswith("presence:")]
    latest_presence = _newest_message(presence)
    visible = [item for item in values if not item.kind.startswith("presence:")]
    kept = _newest_messages(visible, limit=bounded)
    if latest_presence is not None:
        kept.append(latest_presence)
    return _newest_messages(kept, limit=len(kept))


def _task_card_messages_for_thread(
    thread_id: str,
    *,
    after: str | None,
    before: str | None,
) -> list[message_reader.AssistantMessage]:
    actor = tw.canonical_actor(thread_id)
    if not actor:
        return []
    try:
        rows = tw.export(
            [
                "status.any:",
                f"{task_config.TASK_CREATION_SURFACE_UDA}.is:"
                f"{task_config.TASK_CREATION_SURFACE_CLI}",
                f"origin_thread.is:{actor}",
            ]
        )
    except SpiceError:
        return []
    cards = [
        card for row in rows if (card := _task_card_message_from_row(row)) is not None
    ]
    return [
        card
        for card in cards
        if _message_inside_time_boundary(card, after=after, before=before)
    ]


def _task_card_message_from_row(
    row: dict[str, Any],
) -> message_reader.AssistantMessage | None:
    timestamp = _task_row_timestamp(row)
    if not timestamp:
        return None
    handle = task_identity.render_handle(row)
    fields: list[tuple[str, str]] = []
    title = str(row.get("description") or "").strip()
    project = str(row.get("project") or "").strip()
    acceptance = str(row.get("acceptance") or "").strip()
    if title:
        fields.append(("title", title))
    if project:
        fields.append(("project", project))
    if acceptance:
        fields.append(("acceptance", acceptance))
    if handle:
        fields.append(("handle", handle))
    if not fields:
        return None
    return message_reader.task_card_message(
        key=f"{timestamp}#task-card:{str(row.get('uuid') or handle)}",
        index=_task_card_index(row),
        timestamp=timestamp,
        fields=fields,
        source_kind=TASK_CARD_SOURCE_KIND,
    )


def _task_card_index(row: dict[str, Any]) -> int:
    raw_id = row.get("id")
    try:
        task_id = int(raw_id)
    except (TypeError, ValueError):
        task_id = 0
    return 9_000_000_000_000_000_000 + max(0, task_id)


def _task_row_timestamp(row: dict[str, Any]) -> str:
    parsed = _parse_task_timestamp(str(row.get("incepted") or "")) or (
        _parse_task_timestamp(str(row.get("entry") or ""))
    )
    if parsed is None:
        return ""
    return parsed.isoformat(timespec="microseconds").replace("+00:00", "Z")


def _parse_task_timestamp(raw: str) -> datetime | None:
    value = raw.strip()
    if not value:
        return None
    parsed = message_reader.parse_timestamp(value)
    if parsed is not None:
        return parsed
    for fmt in ("%Y%m%dT%H%M%S%fZ", "%Y%m%dT%H%M%SZ"):
        try:
            return datetime.strptime(value, fmt).replace(tzinfo=UTC)
        except ValueError:
            continue
    return None


def _filter_non_offset_boundary(
    items: list[message_reader.AssistantMessage],
    *,
    after: str | None,
    before: str | None,
) -> list[message_reader.AssistantMessage]:
    after_boundary = None if _key_has_transcript_offset(after) else after
    before_boundary = None if _key_has_transcript_offset(before) else before
    if not after_boundary and not before_boundary:
        return items
    return [
        item
        for item in items
        if _message_inside_time_boundary(
            item, after=after_boundary, before=before_boundary
        )
    ]


def _message_inside_time_boundary(
    item: message_reader.AssistantMessage,
    *,
    after: str | None,
    before: str | None,
) -> bool:
    timestamp = message_reader.parse_timestamp(item.timestamp)
    if timestamp is None:
        return True
    after_timestamp = _timestamp_from_message_key(after)
    if after_timestamp is not None and timestamp <= after_timestamp:
        return False
    before_timestamp = _timestamp_from_message_key(before)
    if before_timestamp is not None and timestamp >= before_timestamp:
        return False
    return True


def _timestamp_from_message_key(key: str | None) -> datetime | None:
    if not key:
        return None
    timestamp, _sep, _suffix = key.partition("#")
    return message_reader.parse_timestamp(timestamp)


def _key_has_transcript_offset(key: str | None) -> bool:
    if not key or "#" not in key:
        return False
    raw = key.rsplit("#", 1)[-1]
    try:
        return int(raw) >= 0
    except ValueError:
        return False


def _newest_message(
    items: list[message_reader.AssistantMessage],
) -> message_reader.AssistantMessage | None:
    newest = _newest_messages(items, limit=1)
    return newest[0] if newest else None


def _oldest_message(
    items: list[message_reader.AssistantMessage],
) -> message_reader.AssistantMessage | None:
    return min(items, key=_message_sort_key) if items else None


def _newest_messages(
    items: list[message_reader.AssistantMessage], *, limit: int
) -> list[message_reader.AssistantMessage]:
    return sorted(items, key=_message_sort_key, reverse=True)[:limit]


def _message_sort_key(item: message_reader.AssistantMessage) -> tuple[float, int, str]:
    timestamp = message_reader.parse_timestamp(item.timestamp)
    epoch = timestamp.timestamp() if timestamp is not None else 0.0
    return (epoch, item.index, item.key)


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


def work_trees_payload(state: Any) -> dict[str, Any]:
    targets = state.worktree_targets()
    inventory = task_filter_inventory()
    work_trees = []
    for target in targets:
        thread_id = resolve_thread_id_for_target(state, target) or ""
        pending_identity = pending_inbox_identity_payload(target.repo_root)
        pending = int(pending_identity["pendingInboxCount"])
        predecessor_actor = team_actor_for_target(state.team_store, target, thread_id)
        renew_intent = bool(
            thread_id
            and predecessor_actor
            and state.team_store.agent_renewal_active(predecessor_actor)
        )
        agent_ensure = ensure_agent_for_pending_inbox(
            target,
            pending,
            attempt_cache=state.pending_agent_ensure_attempts,
            force_new=renew_intent,
        )
        ensured_thread_id = record_started_renewal_from_ensure(
            state.team_store,
            predecessor_agent_id=predecessor_actor,
            agent_ensure=agent_ensure,
        )
        if ensured_thread_id:
            thread_id = ensured_thread_id
        pending_identity = pending_inbox_identity_payload(target.repo_root)
        pending = int(pending_identity["pendingInboxCount"])
        status = agent_status(target.repo_root)
        binding_error = agent_binding_error(target.repo_root, status)
        binding_status = _binding_status(thread_id, binding_error)
        team_facts = team_facts_for_target(state.team_store, target, thread_id)
        team_identity = team_identity_payload(team_facts)
        agent_name = _agent_name_for_target(target)
        renewal_intent = renewal_intent_for_target(state.team_store, target, thread_id)
        items, error = target_activity_items(target, thread_id)
        status_line = _status_line_payload_from_status(
            status=status,
            thread_id=thread_id,
            binding_error=binding_error,
            items=items,
            error=error,
            pending_identity=pending_identity,
        )
        work_trees.append(
            {
                "id": target.id,
                "repoRoot": str(target.repo_root),
                "displayName": target.display_name,
                "branch": target.branch or target.name,
                "targetIdentity": target_identity_payload(
                    target,
                    thread_id,
                    binding_status=binding_status,
                    binding_error=binding_error,
                    agent_name=agent_name,
                ),
                "taskFilters": team_facts.get("taskFilters", []),
                "laneFilterVersion": "",
                "teamIdentity": team_identity,
                "lifetime": team_facts.get("lifetime", ""),
                "renewalIntent": renewal_intent,
                "taskFilterInventory": inventory,
                "laneInfo": _lane_info_payload(target, thread_id),
                "pendingCount": pending,
                "pendingLabel": str(pending),
                **pending_identity,
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
    explicit_thread_id = canonical_thread_id(expected_thread_id or "")
    thread_id = explicit_thread_id or resolve_thread_id_for_target(state, target) or ""
    pending_identity = pending_inbox_identity_payload(target.repo_root)
    pending = int(pending_identity["pendingInboxCount"])
    predecessor_actor = team_actor_for_target(state.team_store, target, thread_id)
    renew_intent = bool(
        thread_id
        and predecessor_actor
        and state.team_store.agent_renewal_active(predecessor_actor)
    )
    agent_ensure = ensure_agent_for_pending_inbox(
        target,
        pending,
        attempt_cache=state.pending_agent_ensure_attempts,
        fast_mode=fast_mode,
        force_new=renew_intent,
    )
    ensured_thread_id = record_started_renewal_from_ensure(
        state.team_store,
        predecessor_agent_id=predecessor_actor,
        agent_ensure=agent_ensure,
    )
    if ensured_thread_id:
        thread_id = ensured_thread_id
    pending_identity = pending_inbox_identity_payload(target.repo_root)
    pending = int(pending_identity["pendingInboxCount"])
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
        items = _merge_task_card_messages(
            thread_id,
            items,
            limit=limit,
            after=after,
            before=before,
        )
    team_facts = team_facts_for_target(state.team_store, target, thread_id)
    team_identity = team_identity_payload(team_facts)
    renewal_intent = renewal_intent_for_target(state.team_store, target, thread_id)
    status = agent_status(target.repo_root)
    binding_error = agent_binding_error(target.repo_root, status)
    binding_status = _binding_status(thread_id, binding_error)
    return {
        "messages": [item.to_payload() for item in items],
        "targetWorktreeName": target.name,
        "targetBranch": target.branch or target.name,
        "targetIdentity": target_identity_payload(
            target,
            thread_id,
            binding_status=binding_status,
            binding_error=binding_error,
        ),
        "taskFilters": team_facts.get("taskFilters", []),
        "laneFilterVersion": "",
        "teamIdentity": team_identity,
        "lifetime": team_facts.get("lifetime", ""),
        "renewalIntent": renewal_intent,
        "taskFilterInventory": task_filter_inventory(),
        "laneMetrics": lane_metrics_payload(
            state, target, thread_id=thread_id, items=items, status=status
        ),
        "laneInfo": _lane_info_payload(target, thread_id),
        "agentProcessStatus": status.process_status,
        "error": error or "",
        **pending_identity,
        "agentEnsure": agent_ensure or {},
        "statusLine": status_line_payload(
            state,
            target,
            items=items,
            error=error,
            pending_count=pending,
            pending_identity=pending_identity,
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
    archived = collect_acked_inbox_items(
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
            body = strip_renewal_handoff_request_suffix(inbox_request_body(item.text))
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
