"""Archival of ACK'd and NACK'd inbox steering with per-message summaries.

Retiring a pending inbox item records the consumed steering text and durable
attachment references in `spiceacks.sqlite3`; the pending inbox file is only
the input transport and is discarded after the database write succeeds. ACK
and NACK archival are one operation with opposite dispositions.

The summary helpers mirror inline-task creation feedback: every key a message
named is accounted for — retired, already consumed, or unmatched — so the
supervisor can tell the agent exactly which acknowledgments landed.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Mapping

from spice.errors import SpiceError
from spice.mail.ackgrammar import (
    ack_content_by_key,
    extract_ack_segments_from_text,
    extract_nack_segments_from_text,
    has_noop_ack_marker,
)
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    AckStateWrite,
    ack_state_records,
    record_acked_inbox_items,
)
from spice.mail.inbox import (
    collect_inbox_items,
    discard_inbox_items,
    inbox_item_key,
    inbox_payload_items,
    notify_inbox_changed,
    parse_inbox_payload,
)

_MISSING_GIT_WORKTREE_ERROR = "not inside a git worktree"


def archive_ackd_inbox_items(
    repo_root: str | Path | None,
    ack_keys: Iterable[str],
    *,
    ack_text: str = "",
    ack_content_by_key: Mapping[str, str] | None = None,
) -> list[str]:
    """Retire pending inbox items whose key appears in assistant ACK text."""
    return _archive_keyed_inbox_items(
        repo_root,
        ack_keys,
        message_text=ack_text,
        content_by_key=ack_content_by_key,
        disposition=ACK_DISPOSITION_ACKED,
    )


def archive_nackd_inbox_items(
    repo_root: str | Path | None,
    nack_keys: Iterable[str],
    *,
    nack_text: str = "",
    nack_content_by_key: Mapping[str, str] | None = None,
) -> list[str]:
    """Refuse pending inbox items whose key appears in reason-bearing NACK text."""
    return _archive_keyed_inbox_items(
        repo_root,
        nack_keys,
        message_text=nack_text,
        content_by_key=nack_content_by_key,
        disposition=ACK_DISPOSITION_REFUSED,
    )


def _archive_keyed_inbox_items(
    repo_root: str | Path | None,
    keys: Iterable[str],
    *,
    message_text: str,
    content_by_key: Mapping[str, str] | None,
    disposition: str,
) -> list[str]:
    if repo_root is None:
        return []
    root = Path(repo_root)
    wanted = {inbox_item_key(key) for key in keys if key}
    if not wanted:
        return []
    pending = collect_inbox_items(str(root))
    to_retire = [item for item in pending if inbox_item_key(item.name) in wanted]
    if not to_retire:
        return []
    record_acked_inbox_items(
        repo_root,
        [
            AckStateWrite(
                key=inbox_item_key(item.name),
                inbox_name=item.name,
                text=item.text,
                attachments=_ack_state_attachments(item),
                lineage=_ack_state_lineage(item),
                ack_text=message_text,
                ack_content=_ack_content_for_item(item.name, content_by_key),
                disposition=disposition,
            )
            for item in to_retire
        ],
    )
    discard_inbox_items(inbox_payload_items(to_retire))
    notify_inbox_changed(root)
    return [inbox_item_key(item.name) for item in to_retire]


@dataclass(frozen=True)
class AckArchivalSummary:
    """Disposition of the ACK keys named by one assistant message.

    `archived` are the inbox keys whose pending item this message retired.
    `already_acked` are keys that matched durable ACK state but had no pending
    item left to retire. `unmatched` are keys the message ACK'd that retired
    nothing and have no prior ACK record. `noop` means the message used an ACK
    marker but named no inbox key, so there was nothing to retire.
    """

    archived: list[str]
    already_acked: list[str]
    unmatched: list[str]
    noop: bool = False


@dataclass(frozen=True)
class NackArchivalSummary:
    """Disposition of the NACK keys named by one assistant message."""

    refused: list[str]
    already_refused: list[str]
    already_acked: list[str]
    unmatched: list[str]
    reasonless: list[str]


def summarize_ack_archival(
    repo_root: str | Path | None, message_text: str
) -> AckArchivalSummary:
    """Archive inbox items ACK'd by one assistant message, reporting disposition.

    Mirrors inline-task creation feedback: every key the message ACK'd is
    accounted for, split into the items actually retired, the keys already
    consumed by an earlier ACK, and the keys that matched no known item, so the
    supervisor can tell the agent exactly which acknowledgments landed.
    """
    segments = extract_ack_segments_from_text(message_text)
    requested = list(dict.fromkeys(key for segment in segments for key in segment.keys))
    if not requested:
        return AckArchivalSummary(
            archived=[],
            already_acked=[],
            unmatched=[],
            noop=has_noop_ack_marker(message_text),
        )
    try:
        already_acked_keys = _consumed_state_keys(
            repo_root, disposition=ACK_DISPOSITION_ACKED
        )
        archived = archive_ackd_inbox_items(
            repo_root,
            requested,
            ack_text=message_text,
            ack_content_by_key=ack_content_by_key(segments),
        )
    except SpiceError as exc:
        if _is_missing_git_worktree_error(exc):
            return _empty_ack_archival_summary()
        raise
    archived_keys = {inbox_item_key(key) for key in archived}
    already_acked = [
        key
        for key in requested
        if inbox_item_key(key) not in archived_keys
        and inbox_item_key(key) in already_acked_keys
    ]
    already_acked_request_keys = {inbox_item_key(key) for key in already_acked}
    unmatched = [
        key
        for key in requested
        if inbox_item_key(key) not in archived_keys
        and inbox_item_key(key) not in already_acked_request_keys
    ]
    return AckArchivalSummary(
        archived=archived,
        already_acked=already_acked,
        unmatched=unmatched,
    )


def summarize_nack_archival(
    repo_root: str | Path | None, message_text: str
) -> NackArchivalSummary:
    """Archive inbox items NACK'd by one assistant message as refused."""
    segments = extract_nack_segments_from_text(message_text)
    reasonless = list(
        dict.fromkeys(
            key
            for segment in segments
            if not nack_response_is_honored(segment.content)
            for key in segment.keys
        )
    )
    reasoned_segments = [
        segment for segment in segments if nack_response_is_honored(segment.content)
    ]
    requested = list(
        dict.fromkeys(key for segment in reasoned_segments for key in segment.keys)
    )
    if not requested:
        return NackArchivalSummary(
            refused=[],
            already_refused=[],
            already_acked=[],
            unmatched=[],
            reasonless=reasonless,
        )
    try:
        already_refused_keys = _consumed_state_keys(
            repo_root, disposition=ACK_DISPOSITION_REFUSED
        )
        already_acked_keys = _consumed_state_keys(
            repo_root, disposition=ACK_DISPOSITION_ACKED
        )
        refused = archive_nackd_inbox_items(
            repo_root,
            requested,
            nack_text=message_text,
            nack_content_by_key=ack_content_by_key(reasoned_segments),
        )
    except SpiceError as exc:
        if _is_missing_git_worktree_error(exc):
            return _empty_nack_archival_summary()
        raise
    refused_keys = {inbox_item_key(key) for key in refused}
    already_refused = [
        key
        for key in requested
        if inbox_item_key(key) not in refused_keys
        and inbox_item_key(key) in already_refused_keys
    ]
    already_refused_request_keys = {inbox_item_key(key) for key in already_refused}
    already_acked = [
        key
        for key in requested
        if inbox_item_key(key) not in refused_keys
        and inbox_item_key(key) not in already_refused_request_keys
        and inbox_item_key(key) in already_acked_keys
    ]
    already_acked_request_keys = {inbox_item_key(key) for key in already_acked}
    unmatched = [
        key
        for key in requested
        if inbox_item_key(key) not in refused_keys
        and inbox_item_key(key) not in already_refused_request_keys
        and inbox_item_key(key) not in already_acked_request_keys
    ]
    return NackArchivalSummary(
        refused=refused,
        already_refused=already_refused,
        already_acked=already_acked,
        unmatched=unmatched,
        reasonless=reasonless,
    )


def nack_response_is_honored(content: str) -> bool:
    """Whether a NACK segment carries the reason required to retire its keys."""
    return bool(content.strip())


def _empty_ack_archival_summary() -> AckArchivalSummary:
    return AckArchivalSummary(archived=[], already_acked=[], unmatched=[])


def _empty_nack_archival_summary() -> NackArchivalSummary:
    return NackArchivalSummary(
        refused=[],
        already_refused=[],
        already_acked=[],
        unmatched=[],
        reasonless=[],
    )


def _is_missing_git_worktree_error(exc: SpiceError) -> bool:
    return str(exc) == _MISSING_GIT_WORKTREE_ERROR


def _consumed_state_keys(
    repo_root: str | Path | None, *, disposition: str | None = None
) -> set[str]:
    if repo_root is None:
        return set()
    keys: set[str] = set()
    for record in ack_state_records(repo_root):
        if disposition is not None and record.disposition != disposition:
            continue
        keys.add(inbox_item_key(record.key))
        keys.add(inbox_item_key(record.inbox_name))
    return keys


def _ack_state_attachments(item: Any) -> tuple[dict[str, Any], ...]:
    return tuple(
        {
            "path": str(attachment.path),
            "name": attachment.name,
            "content_type": attachment.content_type,
            "size": attachment.size,
        }
        for attachment in item.attachments
    )


def _ack_state_lineage(item: Any) -> dict[str, Any]:
    from spice.agent.identity import ambient_thread_id

    payload = parse_inbox_payload(item.text)
    attempts = [
        {
            "attempt": attempt.attempt,
            "at": attempt.at,
            "messages_elapsed": attempt.messages_elapsed,
        }
        for attempt in payload.resend_attempts
    ]
    lineage: dict[str, Any] = {}
    thread_id = ambient_thread_id()
    if thread_id:
        lineage["thread_id"] = thread_id
    if payload.resend_count or attempts:
        lineage["resend_count"] = payload.resend_count
        lineage["resend_attempts"] = attempts
    return lineage


def _ack_content_for_item(
    inbox_name: str, content_by_key: Mapping[str, str] | None
) -> str:
    if not content_by_key:
        return ""
    return content_by_key.get(inbox_item_key(inbox_name), "")
