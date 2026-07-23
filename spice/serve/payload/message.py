"""Transcript, task-card, and ACK context payload builders."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from spice.agent.identity import canonical_thread_id
from spice.agent.lifecycle import agent_binding_error, agent_status
from spice.agent.renewal import strip_renewal_handoff_request_suffix
from spice.errors import SpiceError
from spice.mail.replies import read_reply_records
from spice.mail.inbox import (
    collect_consumed_inbox_items_for_keys,
    collect_inbox_items,
    inbox_item_key,
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
from spice.serve.payload.wire import validate_emitter_payload
from spice.serve.markdown import render_message_html
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.worktree.inventory import (
    _ensure_work_tree_agent,
    _work_tree_renewal_intent,
)
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import claimstate
from spice.tasks import config as task_config
from spice.tasks import identity as task_identity
from spice.tasks import tw


TASK_CARD_SOURCE_KIND = "cli_task_created"


class TaskCardExportSnapshot:
    """Load all task-card rows once and filter them per inventory lane.

    Inventory builds render the same global task board through every bound
    lane. Loading it lazily keeps unbound inventories free of this export,
    while caching a failed export as empty lets every lane degrade uniformly
    without repeatedly spawning the failing command.
    """

    def __init__(self) -> None:
        self._rows: list[dict[str, Any]] = []
        self._loaded = False

    def rows_for_actor(self, actor: str) -> list[dict[str, Any]]:
        """Return rows whose stored origin_thread exactly matches ``actor``."""
        if not actor:
            return []
        if not self._loaded:
            self._loaded = True
            try:
                self._rows = tw.export(["status.any:"])
            except SpiceError:
                self._rows = []
        return [
            row for row in self._rows if str(row.get("origin_thread") or "") == actor
        ]


def target_activity_items(
    target: WorktreeTarget,
    thread_id: str,
    *,
    task_cards: TaskCardExportSnapshot | None = None,
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
    merged = _merge_task_card_messages(
        thread_id,
        read.items,
        limit=1,
        task_cards=task_cards,
    )
    merged = _merge_reply_card_messages(
        thread_id,
        merged,
        repo_root=target.repo_root,
        worktree_id=target.id,
        limit=1,
    )
    return (merged, read.error, read.transcript)


def _card_window_after(
    items: list[message_reader.AssistantMessage],
    after: str | None,
    before: str | None,
) -> str | None:
    if after is not None or before is not None or not items:
        return after
    visible_items = [item for item in items if not item.kind.startswith("presence:")]
    oldest = _oldest_message(visible_items or items)
    return oldest.key if oldest is not None else after


def _merge_synthetic_cards(
    items: list[message_reader.AssistantMessage],
    cards: list[message_reader.AssistantMessage],
    *,
    limit: int,
    after: str | None,
    before: str | None,
) -> list[message_reader.AssistantMessage]:
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


def _merge_task_card_messages(
    thread_id: str,
    items: list[message_reader.AssistantMessage],
    *,
    limit: int,
    after: str | None = None,
    before: str | None = None,
    task_cards: TaskCardExportSnapshot | None = None,
) -> list[message_reader.AssistantMessage]:
    cards = _task_card_messages_for_thread(
        thread_id,
        after=_card_window_after(items, after, before),
        before=before,
        task_cards=task_cards,
    )
    return _merge_synthetic_cards(items, cards, limit=limit, after=after, before=before)


def _merge_reply_card_messages(
    thread_id: str,
    items: list[message_reader.AssistantMessage],
    *,
    repo_root: Path,
    worktree_id: str | None,
    limit: int,
    after: str | None = None,
    before: str | None = None,
) -> list[message_reader.AssistantMessage]:
    cards = _reply_card_messages_for_thread(
        thread_id,
        repo_root=repo_root,
        worktree_id=worktree_id,
        after=_card_window_after(items, after, before),
        before=before,
    )
    return _merge_synthetic_cards(items, cards, limit=limit, after=after, before=before)


def _reply_card_messages_for_thread(
    thread_id: str,
    *,
    repo_root: Path,
    worktree_id: str | None,
    after: str | None,
    before: str | None,
) -> list[message_reader.AssistantMessage]:
    cards: list[message_reader.AssistantMessage] = []
    for index, record in enumerate(read_reply_records(repo_root, thread_id)):
        timestamp = str(record.get("timestamp") or "").strip()
        text = str(record.get("text") or "").strip()
        if not timestamp or not text:
            continue
        card = message_reader.reply_card_message(
            f"{timestamp}#reply-card:{index}",
            index,
            timestamp,
            text,
            worktree_id=worktree_id,
        )
        if _message_inside_time_boundary(card, after=after, before=before):
            cards.append(card)
    return cards


def _task_card_messages_for_thread(
    thread_id: str,
    *,
    after: str | None,
    before: str | None,
    task_cards: TaskCardExportSnapshot | None = None,
) -> list[message_reader.AssistantMessage]:
    actor = tw.canonical_actor(thread_id)
    if not actor:
        return []
    if task_cards is not None:
        rows = task_cards.rows_for_actor(actor)
    else:
        try:
            rows = tw.export(
                [
                    "status.any:",
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
    fields = _task_card_fields(row)
    if not fields:
        return None
    handle = task_identity.render_handle(row)
    classes, kicker = _task_card_presentation(row)
    return message_reader.task_card_message(
        key=f"{timestamp}#task-card:{str(row.get('uuid') or handle)}",
        index=_task_card_index(row),
        timestamp=timestamp,
        fields=fields,
        source_kind=_task_card_source_kind(row),
        classes=classes,
        kicker=kicker,
    )


def _task_card_fields(row: dict[str, Any]) -> list[tuple[str, str]]:
    """Ordered (label, value) rows a task card surfaces for one export row.

    Order is explicit: identity (title/description/project), then provenance and
    task-state metadata (origin/priority/status), then phase context (the current
    phase followed by the full flow pipeline), then tags, and finally acceptance
    and handle. Origin carries the row's stored provenance spelling verbatim
    (``ack:<key>`` or ``task:<handle>``); flow reconstructs the phase pipeline via
    ``claimstate.phases_of``. Each field is emitted only when it has a value.
    """
    flow = ", ".join(claimstate.phases_of(row))
    raw_tags = row.get("tags")
    tags = (
        ", ".join(tag for tag in (str(item).strip() for item in raw_tags) if tag)
        if isinstance(raw_tags, list)
        else ""
    )
    candidates = (
        ("title", str(row.get("description") or "").strip()),
        ("description", str(row.get("task_description") or "").strip()),
        ("project", str(row.get("project") or "").strip()),
        ("origin", str(row.get("origin") or "").strip()),
        ("priority", str(row.get("priority") or "").strip()),
        ("status", str(row.get("status") or "").strip()),
        ("phase", str(row.get("phase") or "").strip()),
        ("flow", flow),
        ("tags", tags),
        ("acceptance", str(row.get("acceptance") or "").strip()),
        ("handle", task_identity.render_handle(row)),
    )
    return [(label, value) for label, value in candidates if value]


def _task_card_source_kind(row: dict[str, Any]) -> str:
    if (
        str(row.get(task_config.TASK_CREATION_SURFACE_UDA) or "")
        == task_config.TASK_CREATION_SURFACE_CLI
    ):
        return TASK_CARD_SOURCE_KIND
    return "task_created"


def _task_card_presentation(row: dict[str, Any]) -> tuple[list[str], str]:
    project = str(row.get("project") or "").strip()
    is_oops = task_config.is_oops_project(project)
    is_hidden = task_config.is_hidden_project(project)
    is_private = _task_card_is_private_project(project)
    classes: list[str] = []
    if is_oops:
        classes.append("task-directive-quote--oops")
    if is_hidden:
        classes.append("task-directive-quote--hidden")
    if is_private:
        classes.append("task-directive-quote--private")
    if is_oops:
        return classes, "Oops task"
    if is_private:
        return classes, "Private task"
    if is_hidden:
        return classes, "Hidden task"
    return classes, "Task capture"


def _task_card_is_private_project(project: str) -> bool:
    segments = project.split(".")
    return len(segments) == 3 and segments[0] == "agent" and segments[2] == "task"


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
    incepted = str(row.get("incepted") or "").strip()
    if task_identity.INCEPTED_RE.match(incepted):
        parsed: datetime | None = task_identity.incepted_datetime(incepted)
    else:
        parsed = _parse_task_timestamp(str(row.get("entry") or ""))
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
    try:
        return datetime.strptime(value, "%Y%m%dT%H%M%SZ").replace(tzinfo=UTC)
    except ValueError:
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
) -> _ResolvedMessagesThread:
    explicit_thread_id = canonical_thread_id(expected_thread_id or "")
    thread_id = explicit_thread_id or resolve_thread_id_for_target(state, target) or ""
    thread_id, predecessor_actor, renew_intent, agent_ensure = _ensure_work_tree_agent(
        state, target, thread_id
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
    client_id: str | None = None,
) -> _ThreadMessages:
    if not thread_id:
        return _ThreadMessages(
            items=[],
            error="No agent thread is bound to this worktree yet.",
            transcript=None,
            removed_keys=[],
        )
    # The cursor is the per-client incremental cache; without a client id (e.g.
    # a one-shot GET) read fresh rather than mutating a shared cursor.
    cursor = (
        state.rollout_cursor(client_id, thread_id) if client_id and not before else None
    )
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
    items = _merge_reply_card_messages(
        thread_id,
        items,
        repo_root=target.repo_root,
        worktree_id=target.id,
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
    append_only: bool = False,
    client_id: str | None = None,
) -> dict[str, Any]:
    resolved = _resolve_messages_thread(
        state, target, expected_thread_id=expected_thread_id
    )
    messages = _read_thread_messages(
        state,
        target,
        resolved.thread_id,
        limit=limit,
        after=after,
        before=before,
        append_only=append_only,
        client_id=client_id,
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
    ack_contexts = _ack_contexts_for_worktree(
        target, keys=_ack_keys_for_messages(items)
    )
    payload = {
        "messages": [item.to_payload() for item in items],
        "ackContexts": ack_contexts,
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
        "taskFilterEntries": team_facts.get("taskFilterEntries", []),
        "effectiveTaskFilters": team_facts.get("effectiveTaskFilters", []),
        "laneFilterVersion": "",
        "teamIdentity": team_identity,
        "lifetime": team_facts.get("lifetime", ""),
        "renewalIntent": renewal_intent,
        "taskFilterInventory": task_filter_inventory(),
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
    return validate_emitter_payload(
        "payload.message._messages_worktree_payload", payload
    )


def lane_metrics_summary_payload(state: Any, target: WorktreeTarget) -> dict[str, Any]:
    """Build one lane's metrics on demand for the metrics pane.

    Kept out of the eager per-lane message payload: the metrics tab is never the
    first view, so a closed pane must not pay for the status:completed export
    that _drained_task_count runs. The live bus fetches this only when the
    metrics view is opened, mirroring the metrics.series request.
    """
    thread_id = resolve_thread_id_for_target(state, target) or ""
    items, _error, _transcript = target_activity_items(target, thread_id)
    status = agent_status(target.repo_root)
    return lane_metrics_payload(
        state, target, thread_id=thread_id, items=items, status=status
    )


def _ack_keys_for_messages(items: list[message_reader.AssistantMessage]) -> list[str]:
    keys: list[str] = []
    for item in items:
        for key in item.ack_keys:
            if key and key not in keys:
                keys.append(key)
    return keys


def _ack_contexts_for_worktree(
    target: WorktreeTarget, *, keys: list[str]
) -> list[dict[str, Any]]:
    """Resolve sent-steering context for ACK keys message payloads quote.

    Pending inbox items are live input. Once consumed, `spiceacks.sqlite3` is
    the source of truth for the operator's steering text and durable attachment
    references. The assistant's ACK reply is not operator context and must not
    be quoted back as if the operator wrote it.
    """
    wanted = [key for key in keys if key]
    if not wanted:
        return []
    by_key: dict[str, dict[str, Any]] = {}
    # ACK retirement commits durable state before deleting the pending file.
    # Read in the same monotonic direction: a pre-delete pending snapshot finds
    # the item, while a post-delete snapshot is followed by an exact durable
    # lookup that must see the committed record. Reversing these reads creates
    # a gap where normal hydration can observe neither source and permanently
    # cache a valid key as missing.
    pending = collect_inbox_items(str(target.repo_root))
    consumed = collect_consumed_inbox_items_for_keys(str(target.repo_root), keys=wanted)
    for item in (*consumed, *pending):
        item_key = inbox_item_key(item.name)
        matching_keys = [
            key
            for key in wanted
            if key not in by_key and inbox_item_key(key) == item_key
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
    return [by_key.get(key, {"key": key, "found": False}) for key in wanted]
