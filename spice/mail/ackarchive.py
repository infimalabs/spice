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
    AckSegment,
    ack_content_by_key,
    extract_ack_segments_from_text,
    extract_nack_segments_from_text,
    has_noop_ack_marker,
    keyed_response_reason,
    split_keyed_response,
)
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    AckStateRecord,
    AckStateWrite,
    ack_state_records,
    ack_state_records_for_keys,
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
    consumed = {
        inbox_item_key(record.key): record
        for record in ack_state_records_for_keys(repo_root, wanted)
    }
    fresh = [
        item
        for item in to_retire
        if not _pending_item_matches_consumed_record(
            item,
            consumed.get(inbox_item_key(item.name)),
            disposition=disposition,
        )
    ]
    if fresh:
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
                for item in fresh
            ],
        )
    discard_inbox_items(inbox_payload_items(to_retire))
    notify_inbox_changed(root)
    return [inbox_item_key(item.name) for item in fresh]


def _pending_item_matches_consumed_record(
    item: Any,
    record: AckStateRecord | None,
    *,
    disposition: str,
) -> bool:
    if record is None:
        return False
    return (
        record.inbox_name == item.name
        and record.text == item.text
        and record.attachments == _ack_state_attachments(item)
        and record.disposition == disposition
    )


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


@dataclass(frozen=True)
class KeyedResponseArchivalSummary:
    """Both polarities processed in the supervisor's deterministic order."""

    ack: AckArchivalSummary
    nack: NackArchivalSummary


@dataclass(frozen=True)
class ParsedKeyedResponseSegments:
    """ACK and NACK segments produced by one unified grammar pass."""

    ack: tuple[AckSegment, ...]
    nack: tuple[AckSegment, ...]


def parse_keyed_response_segments(message_text: str) -> ParsedKeyedResponseSegments:
    """Parse both response polarities once and retain their shared boundaries."""
    _preamble, responses = split_keyed_response(message_text)
    ack: list[AckSegment] = []
    nack: list[AckSegment] = []
    for response in responses:
        segment = AckSegment(keys=response.keys, content=response.content)
        if response.disposition == ACK_DISPOSITION_ACKED:
            ack.append(segment)
        elif response.disposition == ACK_DISPOSITION_REFUSED:
            nack.append(segment)
    return ParsedKeyedResponseSegments(ack=tuple(ack), nack=tuple(nack))


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
    return _summarize_ack_archival(
        repo_root,
        message_text,
        segments,
        noop=has_noop_ack_marker(message_text),
    )


def _summarize_ack_archival(
    repo_root: str | Path | None,
    message_text: str,
    segments: Iterable[AckSegment],
    *,
    noop: bool = False,
) -> AckArchivalSummary:
    segments = tuple(segments)
    requested = list(dict.fromkeys(key for segment in segments for key in segment.keys))
    if not requested:
        return AckArchivalSummary(
            archived=[],
            already_acked=[],
            unmatched=[],
            noop=noop,
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
    return _summarize_nack_archival(repo_root, message_text, segments)


def _summarize_nack_archival(
    repo_root: str | Path | None,
    message_text: str,
    segments: Iterable[AckSegment],
) -> NackArchivalSummary:
    segments = tuple(segments)
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


def summarize_keyed_response_archival(
    repo_root: str | Path | None,
    message_text: str,
    *,
    parsed: ParsedKeyedResponseSegments | None = None,
) -> KeyedResponseArchivalSummary:
    """Process one reply exactly as supervised prose: NACK, then ACK.

    The order is part of the contract. A message that somehow names one key
    under both polarities resolves through the NACK authority first, matching
    ``process_supervised_assistant_message`` rather than whichever caller
    happened to invoke a low-level archive function first.
    """
    responses = parsed or parse_keyed_response_segments(message_text)
    nack = _summarize_nack_archival(repo_root, message_text, responses.nack)
    ack = _summarize_ack_archival(repo_root, message_text, responses.ack)
    return KeyedResponseArchivalSummary(ack=ack, nack=nack)


def nack_response_is_honored(content: str) -> bool:
    """Whether a NACK segment carries the reason required to retire its keys.

    Sharing this predicate is not enough on its own: archival strips control
    lines before it asks, and a display does not, so a refusal whose only body
    was a TASK directive read as reasonless on one side and refused on the
    other. Deriving the reason here makes the answer depend on the text rather
    than on the caller.
    """
    return bool(keyed_response_reason(content))


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
