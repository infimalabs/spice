"""Ultra-fast extraction of ACK'd inbox keys from assistant messages.

An ACK in the harness idiom looks like:

    ACK 20260513T184251491561Z: <what was understood>

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
from typing import Any, Iterable, Iterator

from spice.mail.inbox import (
    collect_inbox_items,
    consume_inbox_items,
    inbox_item_key,
    inbox_item_key_aliases,
    inbox_payload_items,
)
from spice.sessions.util import first_text, normalize_timestamp

ACK_TOKEN = "ACK"

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
    """One acknowledgment: the keys it names and the content attributed to it.

    `keys` are the inbox keys read from the ACK header. `content` is the
    cleaned message body that runs from this ACK to the next valid ACK.
    """

    keys: tuple[str, ...]
    content: str


def split_ack_message(text: str) -> tuple[str, list[AckSegment]]:
    """Split `text` into its leading prose and its ordered ACK segments.

    The first element is the cleaned preamble — everything before the first ACK
    marker (often empty). The second is the list of :class:`AckSegment`, one per
    marker, each pairing the keys in its header with the cleaned content that
    runs from that marker to the next (or end of text). A marker is the
    uppercase `ACK` token opening a recognizable header.
    """
    bounds = _ack_marker_bounds(text)
    if not bounds:
        return _clean_segment_content(text), []
    preamble = _clean_segment_content(text[: bounds[0][0]])
    segments: list[AckSegment] = []
    for index, (_ack_pos, header_end, keys) in enumerate(bounds):
        body_end = bounds[index + 1][0] if index + 1 < len(bounds) else len(text)
        segments.append(
            AckSegment(
                keys=keys, content=_clean_segment_content(text[header_end:body_end])
            )
        )
    return preamble, segments


def extract_ack_segments_from_text(text: str) -> list[AckSegment]:
    """Return just the ACK segments of `text` (see :func:`split_ack_message`)."""
    return split_ack_message(text)[1]


def ack_content_by_key(segments: Iterable[AckSegment]) -> dict[str, str]:
    """Roll segments up into a key -> cleaned-content map (latest ACK wins)."""
    mapping: dict[str, str] = {}
    for segment in segments:
        for key in segment.keys:
            mapping[key] = segment.content
    return mapping


def archive_ackd_inbox_items(
    repo_root: Path | None, ack_keys: Iterable[str]
) -> list[str]:
    """Archive pending inbox items whose key appears in assistant ACK text."""
    if repo_root is None:
        return []
    acked_aliases: set[str] = set()
    for key in ack_keys:
        if key:
            acked_aliases |= inbox_item_key_aliases(key)
    if not acked_aliases:
        return []
    pending = collect_inbox_items(str(repo_root))
    to_archive = [
        item for item in pending if inbox_item_key_aliases(item.name) & acked_aliases
    ]
    if not to_archive:
        return []
    consume_inbox_items(inbox_payload_items(to_archive))
    return [inbox_item_key(item.name) for item in to_archive]


def archive_ackd_inbox_items_from_assistant_message(
    repo_root: Path | None, message_text: str
) -> list[str]:
    """Archive inbox items ACK'd by one supervisor-observed assistant message."""
    return archive_ackd_inbox_items(repo_root, extract_ack_keys_from_text(message_text))


def _ack_marker_bounds(text: str) -> list[tuple[int, int, tuple[str, ...]]]:
    """Return `(ack_pos, header_end, keys)` for each valid ACK marker in order."""
    bounds: list[tuple[int, int, tuple[str, ...]]] = []
    for ack_pos in _iter_ack_tokens(text):
        parsed = _parse_ack_header(text, ack_pos)
        if parsed is not None:
            header_end, keys = parsed
            bounds.append((ack_pos, header_end, keys))
    return bounds


def _iter_ack_tokens(text: str) -> Iterator[int]:
    start = 0
    while True:
        index = text.find(ACK_TOKEN, start)
        if index == -1:
            return
        start = index + len(ACK_TOKEN)
        if _is_standalone_word(text, index, start):
            yield index


def _is_standalone_word(text: str, start: int, end: int) -> bool:
    before = text[start - 1] if start > 0 else ""
    after = text[end] if end < len(text) else ""
    return not _is_word_char(before) and not _is_word_char(after)


def _is_word_char(char: str) -> bool:
    return bool(char) and char.isalnum()


def _parse_ack_header(text: str, ack_pos: int) -> tuple[int, tuple[str, ...]] | None:
    """Validate an ACK header at `ack_pos`; return `(header_end, keys)` or None."""
    limit = len(text)
    cursor = ack_pos + len(ACK_TOKEN)
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


def _clean_segment_content(body: str) -> str:
    lines = [line for line in body.splitlines() if not _is_app_directive_line(line)]
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


def iter_assistant_ack_keys(
    files: Iterable[Path],
    *,
    start_ts: str | None = None,
    end_ts: str | None = None,
    turn_ids: Iterable[str] | None = None,
) -> Iterator[str]:
    """Walk JSONL transcripts and emit ACK'd keys in source order.

    `start_ts`/`end_ts` are compared against the per-event timestamp after
    normalization. `turn_ids` filters to assistant messages produced inside
    the listed turns. Both are optional; omitting them is fastest.
    """
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
) -> Iterator[AckSegment]:
    """Walk JSONL transcripts and emit ACK segments in source order."""
    for text in iter_assistant_message_texts(
        files, start_ts=start_ts, end_ts=end_ts, turn_ids=turn_ids
    ):
        yield from extract_ack_segments_from_text(text)


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


def collect_unique_ack_keys(
    files: Iterable[Path],
    *,
    start_ts: str | None = None,
    end_ts: str | None = None,
    turn_ids: Iterable[str] | None = None,
) -> list[str]:
    """Return ACK'd keys in source order, each one at most once."""
    seen: set[str] = set()
    ordered: list[str] = []
    for key in iter_assistant_ack_keys(
        files, start_ts=start_ts, end_ts=end_ts, turn_ids=turn_ids
    ):
        if key in seen:
            continue
        seen.add(key)
        ordered.append(key)
    return ordered


def collect_ack_segments(
    files: Iterable[Path],
    *,
    start_ts: str | None = None,
    end_ts: str | None = None,
    turn_ids: Iterable[str] | None = None,
) -> list[AckSegment]:
    """Return every ACK segment across `files` in source order.

    Each segment pairs the keys an ACK named with the cleaned content
    attributed to it. Use :func:`ack_content_by_key` to collapse it into a
    key -> content map.
    """
    return list(
        iter_assistant_ack_segments(
            files, start_ts=start_ts, end_ts=end_ts, turn_ids=turn_ids
        )
    )


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
