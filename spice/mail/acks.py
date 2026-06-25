"""Ultra-fast extraction of ACK'd inbox keys from assistant messages.

An ACK in the harness idiom looks like:

    ACK 20260513T184251491561Z: <what changed or was captured>

The detector treats text as an ACK iff it carries:

1. The exact ALL-CAPS word `ACK` as a standalone token, AND
2. One or more inbox-key-shaped substrings matching `[0-9]{8}T[0-9A-Za-z]{6,}`.

Both signatures must appear in order: consume `ACK`, consume the key list that
follows it, then treat the remaining text up to the next valid `ACK` as that
acknowledgment's body. Callers that deduplicate yield a repeated key once.

For callers that want the prose an ACK acknowledged, not just its keys,
`extract_ack_segments_from_text` splits a message at each valid ACK marker and
pairs every ACK's keys with the cleaned content attributed to it.

Hot path notes:

- Pre-filters JSONL lines with a substring check before JSON-parsing, so the
  bulk of a transcript (huge tool-call payloads) is skipped without ever
  touching `json.loads`.
- Per-message pre-filters with `"ACK" in text` before scanning for ACK tokens.
- Per-line timestamp normalization happens only when a window is requested
  and the candidate already cleared the ACK pre-filter.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Iterator, Mapping

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
    inbox_item_key_aliases,
    inbox_payload_items,
    notify_inbox_changed,
)
from spice.sessions.util import first_text, normalize_timestamp

ACK_TOKEN = "ACK"
NACK_TOKEN = "NACK"
TASK_DIRECTIVE_TOKEN = "TASK"

# Transcript lines start with `{"timestamp":"<iso>",...` — the timestamp can
# be sliced out without JSON parsing for cheap window pre-filtering.
_TS_PREFIX = '{"timestamp":"'
_TS_PREFIX_LEN = len(_TS_PREFIX)

# A valid ACK header runs from `ACK` through its consecutive key-like tokens.
# Plain `ACK <key> prose` is body-bearing: the header ends at the key and the
# following prose is body. A narrow separator immediately after the key list
# (`:`, comma, dash, or sentence punctuation) is skipped when present. The same
# separator characters may appear immediately after `ACK` before the first key.
_ACK_HEADER_FILLER_WORDS = frozenset({"inbox", "key", "keys"})
_ACK_HEADER_WRAPPER_CHARS = " \t\r\n`\"'[],()*_"
_ACK_KEY_CLOSER_CHARS = "`\"'])*_"
_ACK_BODY_SPACE_CHARS = " \t\r\n"
_ACK_HEADER_SEPARATOR_CHARS = ":—–.-,;!?"
_TASK_DIRECTIVE_SEPARATOR_CHARS = " \t:-"
_ACK_CONTEXT_BREAK_CHARS = "\r\n.!?;"
_ACK_CONTEXT_WORD_EXTRA_CHARS = frozenset({"'", "-"})
_ACK_CONTEXT_WINDOW = 6
_ACK_NEGATION_WORDS = frozenset(
    {
        "can't",
        "cannot",
        "cant",
        "not",
        "refuse",
        "refused",
        "refuses",
        "refusing",
        "will-not",
        "won't",
        "wont",
    }
)
_ACK_NEGATION_PHRASES = (
    ("instead", "of"),
    ("instead-of",),
    ("refuse", "to"),
    ("refused", "to"),
    ("refuses", "to"),
    ("refusing", "to"),
)
_ACK_HYPOTHETICAL_WORDS = frozenset(
    {"could", "hypothetically", "if", "should", "whether", "would"}
)
_ACK_TURNING_WORDS = frozenset({"but", "hence", "so", "therefore", "thus"})
_ACK_NARRATION_WORDS = frozenset(
    {
        "example",
        "form",
        "literal",
        "mention",
        "mentioned",
        "mentions",
        "narrated",
        "phrase",
        "say",
        "saying",
        "string",
        "token",
        "write",
        "writing",
    }
)
# Key grammar: 8 date digits, a "T", then 6+ alphanumerics.
_KEY_DATE_DIGITS = 8
_KEY_TIME_SEPARATOR_INDEX = 8
_KEY_MIN_LENGTH = 9
_KEY_SUFFIX_MIN_LENGTH = 6


def extract_ack_keys_from_text(text: str) -> Iterator[str]:
    """Yield inbox keys from ACK headers in `text`."""
    if ACK_TOKEN not in text:
        return
    for ack_pos in _iter_ack_tokens(text):
        parsed = _parse_ack_header(text, ack_pos)
        if parsed is None:
            continue
        _header_end, keys = parsed
        yield from keys


@dataclass(frozen=True)
class AckSegment:
    """One keyed response: the keys it names and the content attributed to it.

    `keys` are the inbox keys read from the ACK/NACK header. `content` is the
    cleaned message body that runs from this marker to the next valid marker.
    """

    keys: tuple[str, ...]
    content: str


def split_ack_message(
    text: str, *, drop_task_directives: bool = True
) -> tuple[str, list[AckSegment]]:
    """Split `text` into its leading prose and its ordered ACK segments.

    The first element is the cleaned preamble — everything before the first ACK
    marker (often empty). The second is the list of :class:`AckSegment`, one per
    marker, each pairing the keys in its header with the cleaned content that
    runs from that marker to the next (or end of text). A marker is the
    uppercase `ACK` token opening a recognizable header.
    """
    bounds = _ack_marker_bounds(text)
    return _split_keyed_message(
        text,
        bounds,
        all_bounds=_keyed_marker_bounds(text),
        drop_task_directives=drop_task_directives,
    )


def split_nack_message(
    text: str, *, drop_task_directives: bool = True
) -> tuple[str, list[AckSegment]]:
    """Split `text` into its leading prose and ordered reason-bearing NACKs."""
    bounds = _nack_marker_bounds(text)
    return _split_keyed_message(
        text,
        bounds,
        all_bounds=_keyed_marker_bounds(text),
        drop_task_directives=drop_task_directives,
    )


def _split_keyed_message(
    text: str,
    bounds: list[tuple[int, int, tuple[str, ...]]],
    *,
    all_bounds: list[tuple[int, int, tuple[str, ...]]] | None = None,
    drop_task_directives: bool,
) -> tuple[str, list[AckSegment]]:
    if not bounds:
        return _clean_segment_content(
            text, drop_task_directives=drop_task_directives
        ), []
    preamble = _clean_segment_content(
        text[: bounds[0][0]], drop_task_directives=drop_task_directives
    )
    segments: list[AckSegment] = []
    split_bounds = all_bounds if all_bounds is not None else bounds
    for _index, (marker_pos, header_end, keys) in enumerate(bounds):
        body_end = _next_keyed_marker_pos(split_bounds, marker_pos, default=len(text))
        segments.append(
            AckSegment(
                keys=keys,
                content=_clean_segment_content(
                    text[header_end:body_end],
                    drop_task_directives=drop_task_directives,
                ),
            )
        )
    return preamble, segments


def _next_keyed_marker_pos(
    bounds: list[tuple[int, int, tuple[str, ...]]], marker_pos: int, *, default: int
) -> int:
    for next_pos, _header_end, _keys in bounds:
        if next_pos > marker_pos:
            return next_pos
    return default


def extract_ack_segments_from_text(text: str) -> list[AckSegment]:
    """Return just the ACK segments of `text` (see :func:`split_ack_message`)."""
    return split_ack_message(text)[1]


def extract_nack_segments_from_text(text: str) -> list[AckSegment]:
    """Return just the NACK segments of `text` (see :func:`split_nack_message`)."""
    return split_nack_message(text)[1]


def extract_task_batch_lines_from_text(text: str) -> list[str]:
    """Return inline TASK batch payloads carried by an assistant message."""
    return _task_batch_lines(text)


def ack_content_by_key(segments: Iterable[AckSegment]) -> dict[str, str]:
    """Roll segments up into a key -> cleaned-content map (latest ACK wins)."""
    mapping: dict[str, str] = {}
    for segment in segments:
        for key in segment.keys:
            mapping[key] = segment.content
    return mapping


def archive_ackd_inbox_items(
    repo_root: str | Path | None,
    ack_keys: Iterable[str],
    *,
    ack_text: str = "",
    ack_content_by_key: Mapping[str, str] | None = None,
) -> list[str]:
    """Retire pending inbox items whose key appears in assistant ACK text.

    The consumed steering text and durable attachment references are recorded
    in `spiceacks.sqlite3`; the pending inbox file is only the input transport
    and is discarded after the database write succeeds.
    """
    if repo_root is None:
        return []
    root = Path(repo_root)
    acked_aliases: set[str] = set()
    for key in ack_keys:
        if key:
            acked_aliases |= inbox_item_key_aliases(key)
    if not acked_aliases:
        return []
    pending = collect_inbox_items(str(root))
    to_retire = [
        item for item in pending if inbox_item_key_aliases(item.name) & acked_aliases
    ]
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
                ack_text=ack_text,
                ack_content=_ack_content_for_item(item.name, ack_content_by_key),
                disposition=ACK_DISPOSITION_ACKED,
            )
            for item in to_retire
        ],
    )
    discard_inbox_items(inbox_payload_items(to_retire))
    notify_inbox_changed(root)
    return [inbox_item_key(item.name) for item in to_retire]


def archive_nackd_inbox_items(
    repo_root: str | Path | None,
    nack_keys: Iterable[str],
    *,
    nack_text: str = "",
    nack_content_by_key: Mapping[str, str] | None = None,
) -> list[str]:
    """Refuse pending inbox items whose key appears in reason-bearing NACK text."""
    if repo_root is None:
        return []
    root = Path(repo_root)
    nacked_aliases: set[str] = set()
    for key in nack_keys:
        if key:
            nacked_aliases |= inbox_item_key_aliases(key)
    if not nacked_aliases:
        return []
    pending = collect_inbox_items(str(root))
    to_refuse = [
        item for item in pending if inbox_item_key_aliases(item.name) & nacked_aliases
    ]
    if not to_refuse:
        return []
    record_acked_inbox_items(
        repo_root,
        [
            AckStateWrite(
                key=inbox_item_key(item.name),
                inbox_name=item.name,
                text=item.text,
                attachments=_ack_state_attachments(item),
                ack_text=nack_text,
                ack_content=_ack_content_for_item(item.name, nack_content_by_key),
                disposition=ACK_DISPOSITION_REFUSED,
            )
            for item in to_refuse
        ],
    )
    discard_inbox_items(inbox_payload_items(to_refuse))
    notify_inbox_changed(root)
    return [inbox_item_key(item.name) for item in to_refuse]


@dataclass(frozen=True)
class AckArchivalSummary:
    """Disposition of the ACK keys named by one assistant message.

    `archived` are the inbox keys whose pending item this message retired.
    `already_acked` are keys that matched durable ACK state but had no pending
    item left to retire. `unmatched` are keys the message ACK'd that retired
    nothing and have no prior ACK record.
    """

    archived: list[str]
    already_acked: list[str]
    unmatched: list[str]


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
    already_acked_aliases = _consumed_state_aliases(
        repo_root, disposition=ACK_DISPOSITION_ACKED
    )
    archived = archive_ackd_inbox_items(
        repo_root,
        requested,
        ack_text=message_text,
        ack_content_by_key=ack_content_by_key(segments),
    )
    archived_aliases: set[str] = set()
    for key in archived:
        archived_aliases |= inbox_item_key_aliases(key)
    already_acked = [
        key
        for key in requested
        if not (inbox_item_key_aliases(key) & archived_aliases)
        and (inbox_item_key_aliases(key) & already_acked_aliases)
    ]
    already_acked_request_aliases: set[str] = set()
    for key in already_acked:
        already_acked_request_aliases |= inbox_item_key_aliases(key)
    unmatched = [
        key
        for key in requested
        if not (inbox_item_key_aliases(key) & archived_aliases)
        and not (inbox_item_key_aliases(key) & already_acked_request_aliases)
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
            if not segment.content.strip()
            for key in segment.keys
        )
    )
    reasoned_segments = [segment for segment in segments if segment.content.strip()]
    requested = list(
        dict.fromkeys(key for segment in reasoned_segments for key in segment.keys)
    )
    already_refused_aliases = _consumed_state_aliases(
        repo_root, disposition=ACK_DISPOSITION_REFUSED
    )
    already_acked_aliases = _consumed_state_aliases(
        repo_root, disposition=ACK_DISPOSITION_ACKED
    )
    refused = archive_nackd_inbox_items(
        repo_root,
        requested,
        nack_text=message_text,
        nack_content_by_key=ack_content_by_key(reasoned_segments),
    )
    refused_aliases: set[str] = set()
    for key in refused:
        refused_aliases |= inbox_item_key_aliases(key)
    already_refused = [
        key
        for key in requested
        if not (inbox_item_key_aliases(key) & refused_aliases)
        and (inbox_item_key_aliases(key) & already_refused_aliases)
    ]
    already_refused_request_aliases: set[str] = set()
    for key in already_refused:
        already_refused_request_aliases |= inbox_item_key_aliases(key)
    already_acked = [
        key
        for key in requested
        if not (inbox_item_key_aliases(key) & refused_aliases)
        and not (inbox_item_key_aliases(key) & already_refused_request_aliases)
        and (inbox_item_key_aliases(key) & already_acked_aliases)
    ]
    already_acked_request_aliases: set[str] = set()
    for key in already_acked:
        already_acked_request_aliases |= inbox_item_key_aliases(key)
    unmatched = [
        key
        for key in requested
        if not (inbox_item_key_aliases(key) & refused_aliases)
        and not (inbox_item_key_aliases(key) & already_refused_request_aliases)
        and not (inbox_item_key_aliases(key) & already_acked_request_aliases)
    ]
    return NackArchivalSummary(
        refused=refused,
        already_refused=already_refused,
        already_acked=already_acked,
        unmatched=unmatched,
        reasonless=reasonless,
    )


def _consumed_state_aliases(
    repo_root: str | Path | None, *, disposition: str | None = None
) -> set[str]:
    if repo_root is None:
        return set()
    aliases: set[str] = set()
    for record in ack_state_records(repo_root):
        if disposition is not None and record.disposition != disposition:
            continue
        aliases |= inbox_item_key_aliases(record.key)
        aliases |= inbox_item_key_aliases(record.inbox_name)
    return aliases


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


def _ack_content_for_item(
    inbox_name: str, content_by_key: Mapping[str, str] | None
) -> str:
    if not content_by_key:
        return ""
    for alias in inbox_item_key_aliases(inbox_name):
        if alias in content_by_key:
            return content_by_key[alias]
    return ""


def _ack_marker_bounds(text: str) -> list[tuple[int, int, tuple[str, ...]]]:
    """Return `(ack_pos, header_end, keys)` for each valid ACK marker in order."""
    bounds: list[tuple[int, int, tuple[str, ...]]] = []
    for ack_pos in _iter_ack_tokens(text):
        parsed = _parse_ack_header(text, ack_pos)
        if parsed is not None:
            header_end, keys = parsed
            bounds.append((ack_pos, header_end, keys))
    return bounds


def _nack_marker_bounds(text: str) -> list[tuple[int, int, tuple[str, ...]]]:
    """Return `(nack_pos, header_end, keys)` for each valid NACK marker."""
    bounds: list[tuple[int, int, tuple[str, ...]]] = []
    for nack_pos in _iter_nack_tokens(text):
        parsed = _parse_nack_header(text, nack_pos)
        if parsed is not None:
            header_end, keys = parsed
            bounds.append((nack_pos, header_end, keys))
    return bounds


def _keyed_marker_bounds(text: str) -> list[tuple[int, int, tuple[str, ...]]]:
    """Return every valid ACK/NACK marker bound in source order."""
    return sorted((*_ack_marker_bounds(text), *_nack_marker_bounds(text)))


def _iter_ack_tokens(text: str) -> Iterator[int]:
    yield from _iter_header_tokens(text, ACK_TOKEN)


def _iter_nack_tokens(text: str) -> Iterator[int]:
    yield from _iter_header_tokens(text, NACK_TOKEN)


def _iter_header_tokens(text: str, token: str) -> Iterator[int]:
    start = 0
    while True:
        index = text.find(token, start)
        if index == -1:
            return
        start = index + len(token)
        if not _is_standalone_word(text, index, start):
            continue
        if token == ACK_TOKEN and _has_guarded_ack_context(text, index):
            continue
        yield index


def _is_standalone_word(text: str, start: int, end: int) -> bool:
    before = text[start - 1] if start > 0 else ""
    after = text[end] if end < len(text) else ""
    return not _is_word_char(before) and not _is_word_char(after)


def _is_word_char(char: str) -> bool:
    return bool(char) and char.isalnum()


def _has_guarded_ack_context(text: str, ack_pos: int) -> bool:
    """True when surrounding prose is talking about an ACK, not making one."""
    if _ack_token_is_quoted(text, ack_pos):
        return True
    words = _ack_prefix_words(text, ack_pos)
    if not words:
        return False
    context = _words_after_last_turn(words)
    recent = context[-_ACK_CONTEXT_WINDOW:]
    return (
        bool(_ACK_NEGATION_WORDS & set(recent))
        or _contains_phrase(recent, _ACK_NEGATION_PHRASES)
        or bool(_ACK_HYPOTHETICAL_WORDS & set(recent))
        or bool(_ACK_NARRATION_WORDS & set(recent))
    )


def _ack_token_is_quoted(text: str, ack_pos: int) -> bool:
    cursor = ack_pos - 1
    while cursor >= 0 and text[cursor] in " \t":
        cursor -= 1
    if cursor >= 0 and text[cursor] in "`\"'":
        return True
    line_start = text.rfind("\n", 0, ack_pos) + 1
    return text[line_start:ack_pos].count("`") % 2 == 1


def _ack_prefix_words(text: str, ack_pos: int) -> tuple[str, ...]:
    start = ack_pos
    while start > 0 and text[start - 1] not in _ACK_CONTEXT_BREAK_CHARS:
        start -= 1
    words: list[str] = []
    cursor = start
    while cursor < ack_pos:
        char = text[cursor]
        if char.isalnum():
            word_start = cursor
            cursor += 1
            while cursor < ack_pos and (
                text[cursor].isalnum() or text[cursor] in _ACK_CONTEXT_WORD_EXTRA_CHARS
            ):
                cursor += 1
            words.append(text[word_start:cursor].lower())
            continue
        cursor += 1
    return tuple(words)


def _words_after_last_turn(words: tuple[str, ...]) -> tuple[str, ...]:
    for index in range(len(words) - 1, -1, -1):
        if words[index] in _ACK_TURNING_WORDS:
            return words[index + 1 :]
    return words


def _contains_phrase(
    words: tuple[str, ...], phrases: tuple[tuple[str, ...], ...]
) -> bool:
    for phrase in phrases:
        size = len(phrase)
        if size > len(words):
            continue
        for index in range(0, len(words) - size + 1):
            if words[index : index + size] == phrase:
                return True
    return False


def _parse_ack_header(text: str, ack_pos: int) -> tuple[int, tuple[str, ...]] | None:
    """Validate an ACK header at `ack_pos`; return `(header_end, keys)` or None."""
    return _parse_keyed_header(text, ack_pos, ACK_TOKEN)


def _parse_nack_header(text: str, nack_pos: int) -> tuple[int, tuple[str, ...]] | None:
    """Validate a NACK header at `nack_pos`; return `(header_end, keys)` or None."""
    return _parse_keyed_header(text, nack_pos, NACK_TOKEN)


def _parse_keyed_header(
    text: str, token_pos: int, token: str
) -> tuple[int, tuple[str, ...]] | None:
    limit = len(text)
    cursor = token_pos + len(token)
    first_key = _next_header_key(text, cursor, limit, allow_filler_words=True)
    if first_key is None:
        return None
    header_key_matches = []
    header_end = first_key[1]
    key_match: tuple[int, int, str] | None = first_key
    while key_match is not None:
        header_key_matches.append(key_match)
        header_end = key_match[1]
        while header_end < limit and text[header_end] in _ACK_KEY_CLOSER_CHARS:
            header_end += 1
        key_match = _next_header_key(text, header_end, limit, allow_filler_words=False)

    while header_end < limit and text[header_end] in _ACK_KEY_CLOSER_CHARS:
        header_end += 1
    header_end, consumed_separator = _consume_ack_header_separator(
        text, header_end, limit
    )
    if consumed_separator or header_end >= limit:
        return header_end, tuple(match[2] for match in header_key_matches)
    if text[header_end] not in _ACK_BODY_SPACE_CHARS:
        return None
    return header_end, tuple(match[2] for match in header_key_matches)


def is_message_less_keyed_header(header: str) -> bool:
    """True when a body-less ACK header carries only keys and filler words."""
    if not _header_keys(header):
        return False
    return _header_remainder_is_filler(header)


def _next_header_key(
    text: str, cursor: int, limit: int, *, allow_filler_words: bool
) -> tuple[int, int, str] | None:
    while cursor < limit:
        key_end = _ack_key_end(text, cursor, limit)
        if key_end is not None:
            return cursor, key_end, text[cursor:key_end]
        char = text[cursor]
        if char in _ACK_HEADER_WRAPPER_CHARS + _ACK_HEADER_SEPARATOR_CHARS:
            cursor += 1
            continue
        if allow_filler_words and char.isalpha():
            word_end = cursor + 1
            while word_end < limit and text[word_end].isalpha():
                word_end += 1
            if text[cursor:word_end].lower() in _ACK_HEADER_FILLER_WORDS:
                cursor = word_end
                continue
        return None
    return None


def _consume_ack_header_separator(
    text: str, header_end: int, line_end: int
) -> tuple[int, bool]:
    """Skip an immediate ACK separator after the key list, when present."""
    index = header_end
    while index < line_end and text[index] in _ACK_KEY_CLOSER_CHARS + " \t":
        index += 1
    if index < line_end and text[index] in _ACK_HEADER_SEPARATOR_CHARS:
        body_start = index + 1
        while body_start < line_end and text[body_start] in _ACK_BODY_SPACE_CHARS:
            body_start += 1
        return body_start, True
    return header_end, False


def _header_remainder_is_filler(header: str) -> bool:
    index = 0
    while index < len(header):
        key_end = _ack_key_end(header, index, len(header))
        if key_end is not None:
            index = key_end
            continue
        char = header[index]
        if char in _ACK_HEADER_WRAPPER_CHARS + _ACK_HEADER_SEPARATOR_CHARS:
            index += 1
            continue
        if char.isalpha():
            word_end = index + 1
            while word_end < len(header) and header[word_end].isalpha():
                word_end += 1
            if header[index:word_end].lower() not in _ACK_HEADER_FILLER_WORDS:
                return False
            index = word_end
            continue
        return False
    return True


def _header_keys(header: str) -> tuple[str, ...]:
    keys: list[str] = []
    cursor = 0
    limit = len(header)
    while cursor < limit:
        key_end = _ack_key_end(header, cursor, limit)
        if key_end is not None:
            keys.append(header[cursor:key_end])
            cursor = key_end
            continue
        cursor += 1
    return tuple(keys)


def _ack_key_end(text: str, start: int, limit: int) -> int | None:
    if start > 0 and _is_word_char(text[start - 1]):
        return None
    if start + _KEY_MIN_LENGTH > limit:
        return None
    for index in range(start, start + _KEY_DATE_DIGITS):
        if not text[index].isdigit():
            return None
    if text[start + _KEY_TIME_SEPARATOR_INDEX] != "T":
        return None
    suffix_start = start + _KEY_MIN_LENGTH
    suffix_end = suffix_start
    while suffix_end < limit and text[suffix_end].isalnum():
        suffix_end += 1
    if suffix_end - suffix_start < _KEY_SUFFIX_MIN_LENGTH:
        return None
    if suffix_end < limit and _is_word_char(text[suffix_end]):
        return None
    return suffix_end


def _clean_segment_content(body: str, *, drop_task_directives: bool = False) -> str:
    lines = [
        line
        for line in body.splitlines()
        if not _is_app_directive_line(line)
        and (not drop_task_directives or not _is_task_directive_line(line))
    ]
    return "\n".join(lines).strip()


def _is_app_directive_line(line: str) -> bool:
    # App-control directives (e.g. `::git-commit{...}`) are host-app records,
    # not acknowledgment prose; they are dropped from cleaned content.
    stripped = line.strip()
    if not stripped.startswith("::") or not stripped.endswith("}"):
        return False
    open_brace = stripped.find("{")
    if open_brace <= 2:
        return False
    name = stripped[2:open_brace]
    return all(char.islower() or char.isdigit() or char == "-" for char in name)


def _task_batch_lines(text: str) -> list[str]:
    lines: list[str] = []
    for line in text.splitlines():
        payload = _task_batch_line_from_directive(line)
        if payload is not None:
            lines.append(payload)
    return lines


def _is_task_directive_line(line: str) -> bool:
    return _task_batch_line_from_directive(line) is not None


def _task_batch_line_from_directive(line: str) -> str | None:
    stripped = line.strip()
    token_end = len(TASK_DIRECTIVE_TOKEN)
    if not stripped.startswith(TASK_DIRECTIVE_TOKEN):
        return None
    if len(stripped) > token_end and stripped[token_end] not in (
        _TASK_DIRECTIVE_SEPARATOR_CHARS
    ):
        return None
    return stripped


def iter_assistant_ack_keys(
    files: Iterable[Path],
    *,
    start_ts: str | None = None,
    end_ts: str | None = None,
    turn_ids: Iterable[str] | None = None,
    repo_root: str | Path | None = None,
) -> Iterator[str]:
    """Walk JSONL transcripts and emit ACK'd keys in source order.

    `start_ts`/`end_ts` are compared against the per-event timestamp after
    normalization. `turn_ids` filters to assistant messages produced inside
    the listed turns. Both are optional; omitting them is fastest.
    """
    if _ack_state_is_authoritative(repo_root, start_ts, end_ts, turn_ids):
        yield from iter_ack_state_keys(repo_root)
        return
    for text in iter_assistant_message_texts(
        files, start_ts=start_ts, end_ts=end_ts, turn_ids=turn_ids
    ):
        yield from extract_ack_keys_from_text(text)


def iter_assistant_ack_segments(
    files: Iterable[Path],
    *,
    start_ts: str | None = None,
    end_ts: str | None = None,
    turn_ids: Iterable[str] | None = None,
    repo_root: str | Path | None = None,
) -> Iterator[AckSegment]:
    """Walk JSONL transcripts and emit ACK segments in source order."""
    if _ack_state_is_authoritative(repo_root, start_ts, end_ts, turn_ids):
        yield from iter_ack_state_segments(repo_root)
        return
    for text in iter_assistant_message_texts(
        files, start_ts=start_ts, end_ts=end_ts, turn_ids=turn_ids
    ):
        yield from extract_ack_segments_from_text(text)


def iter_ack_state_keys(repo_root: str | Path | None) -> Iterator[str]:
    """Yield ACK'd inbox keys from durable ACK state in archive order."""
    if repo_root is None:
        return
    for record in _ack_state_records_in_archive_order(repo_root):
        if record.disposition != ACK_DISPOSITION_ACKED:
            continue
        if record.key:
            yield record.key


def iter_ack_state_segments(repo_root: str | Path | None) -> Iterator[AckSegment]:
    """Yield ACK segments from durable ACK state when ACK content was recorded."""
    if repo_root is None:
        return
    for record in _ack_state_records_in_archive_order(repo_root):
        if record.disposition != ACK_DISPOSITION_ACKED:
            continue
        if record.key:
            yield AckSegment(keys=(record.key,), content=record.ack_content)


def _ack_state_is_authoritative(
    repo_root: str | Path | None,
    start_ts: str | None,
    end_ts: str | None,
    turn_ids: Iterable[str] | None,
) -> bool:
    return repo_root is not None and not (start_ts or end_ts or turn_ids)


def _ack_state_records_in_archive_order(repo_root: str | Path):
    return sorted(
        ack_state_records(repo_root),
        key=lambda record: (record.archived_at, record.key),
    )


def iter_assistant_message_texts(
    files: Iterable[Path],
    *,
    start_ts: str | None = None,
    end_ts: str | None = None,
    turn_ids: Iterable[str] | None = None,
) -> Iterator[str]:
    """Yield the text of each in-window assistant message across `files`.

    Shared spine for the key and segment extractors: it owns the cheap JSONL
    pre-filtering, time-window short-circuit, and turn tracking, leaving the
    callers to interpret the text however they need.
    """
    turn_filter: set[str] | None = set(turn_ids) if turn_ids else None
    for path in files:
        yield from _iter_path_message_texts(
            path,
            turn_filter=turn_filter,
            start_ts=start_ts,
            end_ts=end_ts,
        )


_WINDOW_STOP = "stop"
_WINDOW_SKIP = "skip"
_WINDOW_PROCESS = "process"


def _iter_path_message_texts(
    path: Path,
    *,
    turn_filter: set[str] | None,
    start_ts: str | None,
    end_ts: str | None,
) -> Iterator[str]:
    current_turn_id: str | None = None
    with path.open() as handle:
        for line in handle:
            action = _window_prescan(
                line,
                start_ts=start_ts,
                end_ts=end_ts,
                turn_filter=turn_filter,
            )
            if action is _WINDOW_STOP:
                return
            if action is _WINDOW_SKIP:
                continue
            if not _line_might_carry_ack(line, turn_filter=turn_filter):
                continue
            obj = _safe_loads(line)
            if obj is None:
                continue
            payload = obj.get("payload") or {}
            if obj.get("type") == "event_msg":
                current_turn_id = _next_turn_id(payload, current_turn_id)
                continue
            text = _emit_text_if_in_window(
                obj,
                payload,
                current_turn_id=current_turn_id,
                turn_filter=turn_filter,
                start_ts=start_ts,
                end_ts=end_ts,
            )
            if text:
                yield text


def _window_prescan(
    line: str,
    *,
    start_ts: str | None,
    end_ts: str | None,
    turn_filter: set[str] | None,
) -> str:
    """Decide what to do with a raw JSONL line before any JSON parsing.

    Returns one of the `_WINDOW_*` sentinels: STOP terminates the whole walk
    (the file is time-ordered, nothing past end_ts can match); SKIP discards
    the line; PROCESS falls through to the normal pipeline.
    """
    if not (start_ts or end_ts):
        return _WINDOW_PROCESS
    raw_ts = _raw_timestamp(line)
    if raw_ts is None:
        return _WINDOW_PROCESS
    if end_ts and raw_ts > end_ts:
        return _WINDOW_STOP
    if start_ts and raw_ts < start_ts:
        if turn_filter is None:
            return _WINDOW_SKIP
        if '"task_started"' in line or '"task_complete"' in line:
            return _WINDOW_PROCESS
        return _WINDOW_SKIP
    return _WINDOW_PROCESS


def _emit_text_if_in_window(
    obj: dict[str, Any],
    payload: dict[str, Any],
    *,
    current_turn_id: str | None,
    turn_filter: set[str] | None,
    start_ts: str | None,
    end_ts: str | None,
) -> str | None:
    """Return assistant text for a parsed record only if it clears all filters."""
    if not _is_assistant_message(obj, payload):
        return None
    if turn_filter is not None and current_turn_id not in turn_filter:
        return None
    if not _ts_within_window(obj.get("timestamp"), start_ts, end_ts):
        return None
    return first_text(payload.get("content"))


def _raw_timestamp(line: str) -> str | None:
    """Slice the timestamp out of a transcript line without parsing JSON.

    Returns None for lines that don't have the expected `{"timestamp":"...",`
    prefix (e.g. the session-meta header at the top of a transcript).
    """
    if not line.startswith(_TS_PREFIX):
        return None
    end = line.find('"', _TS_PREFIX_LEN)
    if end <= _TS_PREFIX_LEN:
        return None
    return line[_TS_PREFIX_LEN:end]


def _line_might_carry_ack(line: str, *, turn_filter: set[str] | None) -> bool:
    if "ACK" in line:
        return True
    if turn_filter is None:
        return False
    return '"task_started"' in line or '"task_complete"' in line


def _safe_loads(line: str) -> dict[str, Any] | None:
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return None
    return obj if isinstance(obj, dict) else None


def _next_turn_id(payload: dict[str, Any], current: str | None) -> str | None:
    inner = payload.get("type")
    if inner == "task_started":
        next_id = payload.get("turn_id")
        return next_id if isinstance(next_id, str) else None
    if inner == "task_complete":
        return None
    return current


def _is_assistant_message(obj: dict[str, Any], payload: dict[str, Any]) -> bool:
    return (
        obj.get("type") == "response_item"
        and payload.get("type") == "message"
        and payload.get("role") == "assistant"
    )


def _ts_within_window(raw_ts: Any, start_ts: str | None, end_ts: str | None) -> bool:
    if not start_ts and not end_ts:
        return True
    if not isinstance(raw_ts, str):
        return False
    normalized = normalize_timestamp(raw_ts)
    if normalized is None:
        return False
    if start_ts and normalized < start_ts:
        return False
    if end_ts and normalized > end_ts:
        return False
    return True
