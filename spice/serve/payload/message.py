"""Transcript, task-card, and ACK context payload builders."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from spice.agent.identity import canonical_thread_id
from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.agent.renewal import strip_renewal_handoff_request_suffix
from spice.errors import SpiceError
from spice.mail.inbox import (
    collect_acked_inbox_items,
    collect_inbox_items,
    collect_refused_inbox_items,
    inbox_item_key_aliases,
    inbox_request_body,
    inbox_request_priority,
)
from spice.serve import messages as message_reader
from spice.serve.attachments import inbox_attachment_payloads
from spice.serve.payload.identity import (
    _binding_status,
    resolve_thread_id_for_target,
    serve_agent_identity_payload,
    target_identity_payload,
    team_facts_for_target,
    team_identity_payload,
)
from spice.serve.payload.lane import (
    _lane_info_payload,
    lane_metrics_payload,
    status_line_payload,
    task_filter_inventory,
)
from spice.serve.markdown import render_message_html
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.worktree.inventory import (
    _ensure_work_tree_agent,
    _work_tree_renewal_intent,
)
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import config as task_config
from spice.tasks import identity as task_identity
from spice.tasks import tw

ACK_CONTEXT_ARCHIVE_LIMIT = 50


TASK_CARD_SOURCE_KIND = "cli_task_created"


def target_activity_items(
    target: WorktreeTarget, thread_id: str
) -> tuple[
    list[message_reader.AssistantMessage],
    str | None,
    message_reader.TranscriptResolution | None,
]:
    if not thread_id:
        return [], None, None
    read = message_reader.assistant_messages_for_thread_id(
        thread_id,
        limit=1,
        worktree_id=target.id,
        repo_root=target.repo_root,
    )
    return (
        _merge_task_card_messages(thread_id, read.items, limit=1),
        read.error,
        read.transcript,
    )


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
    if raw_id is None:
        task_id = 0
    else:
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


@dataclass(frozen=True)
class _ResolvedMessagesThread:
    thread_id: str
    predecessor_actor: str
    renew_intent: bool
    agent_ensure: dict[str, Any] | None
    pending_identity: dict[str, Any]
    pending: int


@dataclass(frozen=True)
class _ThreadMessages:
    items: list[message_reader.AssistantMessage]
    error: str | None
    transcript: message_reader.TranscriptResolution | None
    removed_keys: list[str]


def _resolve_messages_thread(
    state: Any,
    target: WorktreeTarget,
    *,
    expected_thread_id: str | None,
    fast_mode: bool,
) -> _ResolvedMessagesThread:
    explicit_thread_id = canonical_thread_id(expected_thread_id or "")
    thread_id = explicit_thread_id or resolve_thread_id_for_target(state, target) or ""
    thread_id, predecessor_actor, renew_intent, agent_ensure = _ensure_work_tree_agent(
        state, target, thread_id, fast_mode=fast_mode
    )
    pending_identity = pending_inbox_identity_payload(target.repo_root)
    pending = int(pending_identity["pendingInboxCount"])
    return _ResolvedMessagesThread(
        thread_id=thread_id,
        predecessor_actor=predecessor_actor,
        renew_intent=renew_intent,
        agent_ensure=agent_ensure,
        pending_identity=pending_identity,
        pending=pending,
    )


def _read_thread_messages(
    state: Any,
    target: WorktreeTarget,
    thread_id: str,
    *,
    limit: int,
    after: str | None,
    before: str | None,
    append_only: bool,
) -> _ThreadMessages:
    if not thread_id:
        return _ThreadMessages(
            items=[],
            error="No agent thread is bound to this worktree yet.",
            transcript=None,
            removed_keys=[],
        )
    cursor = state.rollout_cursor(thread_id) if not before else None
    read = message_reader.assistant_messages_for_thread_id(
        thread_id,
        limit=limit,
        after=after,
        before=before,
        append_only=append_only,
        cursor=cursor,
        worktree_id=target.id,
        repo_root=target.repo_root,
    )
    items = _merge_task_card_messages(
        thread_id,
        read.items,
        limit=limit,
        after=after,
        before=before,
    )
    removed_keys = list(cursor.removed_keys) if cursor is not None else []
    return _ThreadMessages(
        items=items,
        error=read.error,
        transcript=read.transcript,
        removed_keys=removed_keys,
    )


def messages_payload_for_worktree(
    state: Any,
    target: WorktreeTarget,
    *,
    limit: int,
    after: str | None = None,
    before: str | None = None,
    expected_thread_id: str | None = None,
    fast_mode: bool = False,
    append_only: bool = False,
) -> dict[str, Any]:
    resolved = _resolve_messages_thread(
        state, target, expected_thread_id=expected_thread_id, fast_mode=fast_mode
    )
    messages = _read_thread_messages(
        state,
        target,
        resolved.thread_id,
        limit=limit,
        after=after,
        before=before,
        append_only=append_only,
    )
    return _messages_worktree_payload(
        state,
        target,
        thread_id=resolved.thread_id,
        predecessor_actor=resolved.predecessor_actor,
        renew_intent=resolved.renew_intent,
        agent_ensure=resolved.agent_ensure,
        pending=resolved.pending,
        pending_identity=resolved.pending_identity,
        items=messages.items,
        removed_keys=messages.removed_keys,
        error=messages.error,
        transcript=messages.transcript,
    )


def _messages_worktree_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    thread_id: str,
    predecessor_actor: str,
    renew_intent: bool,
    agent_ensure: dict[str, Any] | None,
    pending: int,
    pending_identity: dict[str, Any],
    items: list[message_reader.AssistantMessage],
    removed_keys: list[str],
    error: str | None,
    transcript: message_reader.TranscriptResolution | None,
) -> dict[str, Any]:
    team_facts = team_facts_for_target(state.team_store, target, thread_id)
    team_identity = team_identity_payload(team_facts)
    renewal_intent = _work_tree_renewal_intent(
        state, target, thread_id, predecessor_actor, renew_intent
    )
    status = agent_status(target.repo_root)
    binding_error = agent_binding_error(target.repo_root, status)
    binding_status = _binding_status(thread_id, binding_error)
    transcript_owner = transcript.owner_driver.name if transcript else ""
    serve_identity = serve_agent_identity_payload(
        target,
        thread_id,
        binding_status=binding_status,
        binding_error=binding_error,
        transcript_owner=transcript_owner,
        store=state.team_store,
    )
    payload = {
        "messages": [item.to_payload() for item in items],
        "targetWorktreeName": target.name,
        "targetBranch": target.branch or target.name,
        "targetIdentity": target_identity_payload(
            target,
            thread_id,
            binding_status=binding_status,
            binding_error=binding_error,
        ),
        "serveAgentIdentity": serve_identity,
        "taskFilters": team_facts.get("taskFilters", []),
        "laneFilterVersion": "",
        "teamIdentity": team_identity,
        "lifetime": team_facts.get("lifetime", ""),
        "renewalIntent": renewal_intent,
        "taskFilterInventory": task_filter_inventory(),
        "laneMetrics": lane_metrics_payload(
            state, target, thread_id=thread_id, items=items, status=status
        ),
        "laneInfo": _lane_info_payload(target, serve_identity),
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
    if removed_keys:
        payload["removedMessageKeys"] = list(removed_keys)
    return payload


def ack_context_payload_for_worktree(
    state: Any, target: WorktreeTarget, *, keys: list[str]
) -> dict[str, Any]:
    """Resolve sent-steering context for ACK keys the UI wants to quote.

    Pending inbox items are live input. Once consumed, `spiceacks.sqlite3` is
    the source of truth for the operator's steering text and durable attachment
    references. The assistant's ACK reply is not operator context and must not
    be quoted back as if the operator wrote it.
    """
    wanted = [key for key in keys if key]
    by_key: dict[str, dict[str, Any]] = {}
    acked = collect_acked_inbox_items(
        str(target.repo_root), limit=ACK_CONTEXT_ARCHIVE_LIMIT
    )
    refused = collect_refused_inbox_items(
        str(target.repo_root), limit=ACK_CONTEXT_ARCHIVE_LIMIT
    )
    pending = collect_inbox_items(str(target.repo_root))
    for item in (*acked, *refused, *pending):
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
                    "disposition": item.disposition,
                    "attachments": attachments,
                }
    acks = [by_key.get(key, {"key": key, "found": False}) for key in wanted]
    return {"ok": True, "acks": acks}
