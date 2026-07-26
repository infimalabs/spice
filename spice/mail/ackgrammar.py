"""Grammar for keyed steering responses in assistant messages.

An ACK in the harness idiom looks like:

    ACK 1kF4sdFJ: <what changed or was captured>

The detector treats text as an ACK iff it carries:

1. The exact ALL-CAPS word `ACK` as a standalone token, AND
2. One or more non-hyphen-prefixed inbox-key-shaped substrings: an
   8-character base52 moment stamp (the `spice.tasks.identity` alphabet),
   optionally carrying a `-N` collision suffix from inbox filename
   publishing.

Both signatures must appear in order: consume `ACK`, consume the key list that
follows it, then treat the remaining text up to the next valid `ACK` as that
acknowledgment's body. Callers that deduplicate yield a repeated key once.

For callers that want the prose an ACK acknowledged, not just its keys,
`extract_ack_segments_from_text` splits a message at each valid ACK marker and
pairs every ACK's keys with the cleaned content attributed to it. NACK is the
same grammar with opposite sign; `split_keyed_response` walks both polarities
in source order.

Hot path note: every extractor pre-filters with a plain substring check
(`ACK in text`) before scanning for tokens, so ACK-free text costs one scan.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any, Iterable, Iterator, Sequence

from spice.mail.ackstate import ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED
from spice import textcontext
from spice.tasks import identity

ACK_TOKEN = "ACK"
NACK_TOKEN = "NACK"
TASK_DIRECTIVE_TOKEN = "TASK"

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
# Markdown emphasis delimiters. Claude routinely wraps the header in bold
# (`**ACK <key>:** ...`); a uniform run of these immediately before the token
# is the wrapper's opening delimiter and its close is consumed with it.
_EMPHASIS_CHARS = "*_"
_TASK_DIRECTIVE_SEPARATOR_CHARS = " \t:-"
_CONTROL_LINE_LIST_MARKERS = ("- ", "* ", "+ ")
_SOURCE_CONTEXT_LINE_RE = re.compile(r"^\s*(?:[./\w-]+/)*[\w.-]+:\d+(?::\d+)?:")
_ACK_HYPOTHETICAL_WORDS = frozenset(
    {"could", "hypothetically", "if", "should", "whether", "would"}
)
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
# Key grammar: an 8-character base52 moment stamp, optionally carrying a
# `-N` collision suffix from inbox filename publishing.
_KEY_STAMP_CHARS = frozenset(identity.ALPHABET)
_KEY_STAMP_WIDTH = identity.STAMP_WIDTH


def extract_ack_keys_from_text(text: str) -> Iterator[str]:
    """Yield inbox keys from ACK headers in `text`."""
    if ACK_TOKEN not in text:
        return
    for ack_pos in _iter_header_tokens(text, ACK_TOKEN):
        parsed = _parse_keyed_header(text, ack_pos, ACK_TOKEN)
        if parsed is None:
            continue
        _header_end, keys, _body_wrapper = parsed
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
    bounds = _marker_bounds(text, ACK_TOKEN)
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
    bounds = _marker_bounds(text, NACK_TOKEN)
    return _split_keyed_message(
        text,
        bounds,
        all_bounds=_keyed_marker_bounds(text),
        drop_task_directives=drop_task_directives,
    )


@dataclass(frozen=True)
class KeyedResponse:
    """One steering response: its keys, cleaned body, and polarity.

    ACK and NACK are the same shape with opposite sign — an acknowledgment vs a
    reasoned refusal. `disposition` is `ACK_DISPOSITION_ACKED` for an ACK header
    and `ACK_DISPOSITION_REFUSED` for a NACK header; everything else mirrors
    :class:`AckSegment`.
    """

    keys: tuple[str, ...]
    content: str
    disposition: str


def split_keyed_response(
    text: str, *, drop_task_directives: bool = True
) -> tuple[str, list[KeyedResponse]]:
    """Split `text` into leading prose and ordered ACK/NACK responses.

    Unlike :func:`split_ack_message` / :func:`split_nack_message`, which each see
    only one polarity (so a NACK-led message hides its refusal in the ACK
    preamble), this walks both marker kinds in source order and tags each segment
    with its disposition. Bodies stop at the next keyed marker of either kind.
    """
    tagged = sorted(
        [(*b, ACK_DISPOSITION_ACKED) for b in _marker_bounds(text, ACK_TOKEN)]
        + [(*b, ACK_DISPOSITION_REFUSED) for b in _marker_bounds(text, NACK_TOKEN)]
    )
    if not tagged:
        return (
            _clean_segment_content(text, drop_task_directives=drop_task_directives),
            [],
        )
    preamble = _clean_segment_content(
        text[: tagged[0][0]], drop_task_directives=drop_task_directives
    )
    responses: list[KeyedResponse] = []
    for marker_pos, header_end, keys, body_wrapper, disposition in tagged:
        body_end = _next_keyed_marker_pos(tagged, marker_pos, default=len(text))
        responses.append(
            KeyedResponse(
                keys=keys,
                content=_clean_segment_content(
                    text[header_end:body_end],
                    drop_task_directives=drop_task_directives,
                    wrapper=body_wrapper,
                ),
                disposition=disposition,
            )
        )
    return preamble, responses


def _split_keyed_message(
    text: str,
    bounds: list[tuple[int, int, tuple[str, ...], str]],
    *,
    all_bounds: list[tuple[int, int, tuple[str, ...], str]] | None = None,
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
    for marker_pos, header_end, keys, body_wrapper in bounds:
        body_end = _next_keyed_marker_pos(split_bounds, marker_pos, default=len(text))
        segments.append(
            AckSegment(
                keys=keys,
                content=_clean_segment_content(
                    text[header_end:body_end],
                    drop_task_directives=drop_task_directives,
                    wrapper=body_wrapper,
                ),
            )
        )
    return preamble, segments


def _next_keyed_marker_pos(
    bounds: Sequence[tuple[Any, ...]], marker_pos: int, *, default: int
) -> int:
    for next_pos, *_rest in bounds:
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


def keyed_response_reason(content: str) -> str:
    """The reason text a keyed response carries, with control lines removed.

    A segment's content reaches this from two directions: archival hands over
    text this module already cleaned, while a display hands over the body it
    kept whole so task directives could still become cards. Cleaning is
    idempotent, so running it here lets both arrive at the same reason and
    keeps the decision from depending on which door the text came through.
    """
    return _clean_segment_content(content, drop_task_directives=True)


def ack_content_by_key(segments: Iterable[AckSegment]) -> dict[str, str]:
    """Roll segments up into a key -> cleaned-content map (latest ACK wins)."""
    mapping: dict[str, str] = {}
    for segment in segments:
        for key in segment.keys:
            mapping[key] = segment.content
    return mapping


def has_noop_ack_marker(text: str) -> bool:
    """True when the message says ACK but names no valid inbox key."""
    if ACK_TOKEN not in text:
        return False
    for ack_pos in _iter_header_tokens(text, ACK_TOKEN):
        if _parse_keyed_header(text, ack_pos, ACK_TOKEN) is None and (
            _looks_noop_ack_marker(text, ack_pos)
        ):
            return True
    return False


def _marker_bounds(
    text: str, token: str
) -> list[tuple[int, int, tuple[str, ...], str]]:
    """Return `(marker_start, header_end, keys, body_wrapper)` per keyed marker.

    `marker_start` backs the position up over any markdown-emphasis wrapper the
    header opened with, so the leading `**` is excluded from the preamble.
    `body_wrapper` is the same delimiter when its close still trails the body
    (`**ACK k: body**`), so the segment cleaner can strip the dangling run.
    """
    bounds: list[tuple[int, int, tuple[str, ...], str]] = []
    for token_pos in _iter_header_tokens(text, token):
        parsed = _parse_keyed_header(text, token_pos, token)
        if parsed is not None:
            header_end, keys, body_wrapper = parsed
            marker_start, _wrapper = _emphasis_run_before(text, token_pos)
            bounds.append((marker_start, header_end, keys, body_wrapper))
    return bounds


def _keyed_marker_bounds(text: str) -> list[tuple[int, int, tuple[str, ...], str]]:
    """Return every valid ACK/NACK marker bound in source order."""
    return sorted((*_marker_bounds(text, ACK_TOKEN), *_marker_bounds(text, NACK_TOKEN)))


def _iter_header_tokens(text: str, token: str) -> Iterator[int]:
    suppressed_ranges = _suppressed_control_line_ranges(text)
    start = 0
    while True:
        index = text.find(token, start)
        if index == -1:
            return
        start = index + len(token)
        if not _is_standalone_word(text, index, start):
            continue
        if _position_in_ranges(index, suppressed_ranges):
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
    words = textcontext.clause_prefix_words(text, ack_pos)
    if not words:
        return False
    context = textcontext.words_after_last_turn(words)
    recent = context[-textcontext.CONTEXT_WINDOW :]
    return (
        textcontext.has_explicit_negation_before(text, ack_pos)
        or bool(_ACK_HYPOTHETICAL_WORDS & set(recent))
        or bool(_ACK_NARRATION_WORDS & set(recent))
    )


def _looks_noop_ack_marker(text: str, ack_pos: int) -> bool:
    """True when a keyless ACK token is shaped like a directive."""
    line_start = text.rfind("\n", 0, ack_pos) + 1
    if text[line_start:ack_pos].strip():
        return False
    cursor = ack_pos + len(ACK_TOKEN)
    while cursor < len(text) and text[cursor] in " \t":
        cursor += 1
    if (
        cursor < len(text)
        and text[cursor] == "-"
        and _ack_key_shape_end(text, cursor + 1, len(text)) is not None
    ):
        return False
    return cursor >= len(text) or text[cursor] in _ACK_HEADER_SEPARATOR_CHARS + "\r\n"


def _ack_token_is_quoted(text: str, ack_pos: int) -> bool:
    cursor = ack_pos - 1
    while cursor >= 0 and text[cursor] in " \t":
        cursor -= 1
    if cursor >= 0 and text[cursor] in "`\"'":
        return True
    line_start = text.rfind("\n", 0, ack_pos) + 1
    return text[line_start:ack_pos].count("`") % 2 == 1


def _parse_keyed_header(
    text: str, token_pos: int, token: str
) -> tuple[int, tuple[str, ...], str] | None:
    """Validate a keyed header; return `(header_end, keys, body_wrapper)` or None.

    When the header opened with a markdown-emphasis wrapper (`**ACK k:** ...`)
    whose close sits right after the separator, that close is consumed into
    `header_end` and `body_wrapper` is empty. When the wrapper instead closes at
    the end of the body (`**ACK k: body**`), `body_wrapper` names the delimiter
    so the segment cleaner strips the dangling run.
    """
    limit = len(text)
    _marker_start, lead_wrapper = _emphasis_run_before(text, token_pos)
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
    body_wrapper = lead_wrapper
    if lead_wrapper:
        after_wrapper = _consume_body_wrapper_close(
            text, header_end, limit, lead_wrapper
        )
        if after_wrapper != header_end:
            header_end = after_wrapper
            consumed_separator = True
            body_wrapper = ""
    keys = tuple(match[2] for match in header_key_matches)
    if consumed_separator or header_end >= limit:
        return header_end, keys, body_wrapper
    if text[header_end] not in _ACK_BODY_SPACE_CHARS:
        return None
    return header_end, keys, body_wrapper


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


def _emphasis_run_before(text: str, pos: int) -> tuple[int, str]:
    """Return `(start, delimiter)` for a markdown-emphasis wrapper before `pos`.

    A wrapper is a uniform run of `*`/`_` immediately preceding `pos` that opens
    at a non-word boundary (so `**ACK` opens a wrapper but `x**ACK` does not, its
    `**` closing prior bold). Returns `(pos, "")` when no clean wrapper is found.
    """
    start = pos
    while start > 0 and text[start - 1] in _EMPHASIS_CHARS:
        start -= 1
    if start == pos:
        return pos, ""
    run = text[start:pos]
    if run != run[0] * len(run):
        return pos, ""
    if start > 0 and _is_word_char(text[start - 1]):
        return pos, ""
    return start, run


def _consume_body_wrapper_close(text: str, index: int, limit: int, wrapper: str) -> int:
    """Skip a wrapper-closing emphasis run (and trailing space) at `index`."""
    if text[index : index + len(wrapper)] != wrapper:
        return index
    index += len(wrapper)
    while index < limit and text[index] in _ACK_BODY_SPACE_CHARS:
        index += 1
    return index


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


def _ack_key_end(text: str, start: int, limit: int) -> int | None:
    if start > 0 and (_is_word_char(text[start - 1]) or text[start - 1] == "-"):
        return None
    return _ack_key_shape_end(text, start, limit)


def _ack_key_shape_end(text: str, start: int, limit: int) -> int | None:
    end = start + _KEY_STAMP_WIDTH
    if end > limit:
        return None
    for index in range(start, end):
        if text[index] not in _KEY_STAMP_CHARS:
            return None
    if end + 1 < limit and text[end] == "-" and text[end + 1].isdigit():
        end += 1
        while end < limit and text[end].isdigit():
            end += 1
    if end < limit and _is_word_char(text[end]):
        return None
    return end


def _clean_segment_content(
    body: str, *, drop_task_directives: bool = False, wrapper: str = ""
) -> str:
    lines = [
        line
        for line in body.splitlines()
        if not _is_app_directive_line(line)
        and (not drop_task_directives or not _is_task_directive_line(line))
    ]
    cleaned = "\n".join(lines).strip()
    if wrapper and cleaned.endswith(wrapper):
        cleaned = cleaned[: -len(wrapper)].rstrip()
    return cleaned


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
    for line, suppressed in _iter_control_lines(text):
        if suppressed:
            continue
        payload = _task_batch_line_from_directive(line)
        if payload is not None:
            lines.append(payload)
    return lines


def _is_task_directive_line(line: str) -> bool:
    return _task_batch_line_from_directive(line) is not None


def _task_batch_line_from_directive(line: str) -> str | None:
    stripped = _undecorated_control_line(line)
    token_end = len(TASK_DIRECTIVE_TOKEN)
    if not stripped.startswith(TASK_DIRECTIVE_TOKEN):
        return None
    rest = stripped[token_end:]
    # `**TASK** payload` closes its wrapper before the separator; the run is the
    # decoration, not the directive, so it is consumed rather than refused.
    wrapper_end = _emphasis_run_size(rest)
    if wrapper_end:
        rest = rest[wrapper_end:]
    if rest and rest[0] not in _TASK_DIRECTIVE_SEPARATOR_CHARS:
        return None
    return TASK_DIRECTIVE_TOKEN + rest


def _undecorated_control_line(line: str) -> str:
    """Return `line` without the markdown decoration a directive may wear.

    A control line is recognized by its token, not by how the agent chose to
    render it, and the ACK grammar already reads through emphasis wrappers and
    list markers to find its own token. This is that same permission stated once
    for a whole line, so a bold or bulleted TASK header is the directive it
    plainly is instead of silently becoming prose. Whether a line is eligible at
    all remains the suppression rule's decision, which is unchanged.
    """
    stripped = line.strip()
    marker = _list_marker_size(stripped)
    if marker:
        stripped = stripped[marker:].lstrip()
    wrapper_size = _emphasis_run_size(stripped)
    if not wrapper_size:
        return stripped
    delimiter = stripped[:wrapper_size]
    stripped = stripped[wrapper_size:]
    # Only the run that closes this wrapper comes off, so a payload ending in
    # its own emphasis character keeps it.
    if stripped.endswith(delimiter):
        stripped = stripped[:-wrapper_size].rstrip()
    return stripped


def _list_marker_size(text: str) -> int:
    """Length of a leading markdown list marker, including its trailing space."""
    for marker in _CONTROL_LINE_LIST_MARKERS:
        if text.startswith(marker):
            return len(marker)
    return 0


def _emphasis_run_size(text: str) -> int:
    """Length of the uniform leading run of markdown-emphasis characters."""
    if not text or text[0] not in _EMPHASIS_CHARS:
        return 0
    char = text[0]
    size = 0
    while size < len(text) and text[size] == char:
        size += 1
    return size


def _suppressed_control_line_ranges(text: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    for _line, suppressed, start, end in _iter_control_line_ranges(text):
        if suppressed:
            ranges.append((start, end))
    return ranges


def _position_in_ranges(position: int, ranges: list[tuple[int, int]]) -> bool:
    return any(start <= position < end for start, end in ranges)


def _iter_control_lines(text: str) -> Iterator[tuple[str, bool]]:
    for line, suppressed, _start, _end in _iter_control_line_ranges(text):
        yield line, suppressed


def _iter_control_line_ranges(text: str) -> Iterator[tuple[str, bool, int, int]]:
    in_fence = False
    fence_char = ""
    fence_size = 0
    offset = 0
    for raw_line in text.splitlines(keepends=True):
        line = raw_line.rstrip("\r\n")
        line_end = offset + len(raw_line)
        fence = _markdown_fence(line)
        suppressed = (
            in_fence
            or _is_markdown_quote_line(line)
            or _is_indented_code_line(line)
            or _is_rendered_source_context_line(line)
        )
        if fence is not None:
            fence_candidate_char, fence_candidate_size = fence
            suppressed = True
            if not in_fence:
                in_fence = True
                fence_char = fence_candidate_char
                fence_size = fence_candidate_size
            elif (
                fence_candidate_char == fence_char
                and fence_candidate_size >= fence_size
            ):
                in_fence = False
                fence_char = ""
                fence_size = 0
        yield line, suppressed, offset, line_end
        offset = line_end
    if text and (not text.endswith(("\n", "\r"))):
        return


def _markdown_fence(line: str) -> tuple[str, int] | None:
    indent = len(line) - len(line.lstrip(" "))
    if indent > 3:
        return None
    stripped = line[indent:]
    if stripped.startswith("```"):
        return "`", _opening_run_size(stripped, "`")
    if stripped.startswith("~~~"):
        return "~", _opening_run_size(stripped, "~")
    return None


def _opening_run_size(text: str, char: str) -> int:
    size = 0
    while size < len(text) and text[size] == char:
        size += 1
    return size


def _is_markdown_quote_line(line: str) -> bool:
    indent = len(line) - len(line.lstrip(" "))
    return indent <= 3 and line[indent:].startswith(">")


def _is_indented_code_line(line: str) -> bool:
    return line.startswith("\t") or line.startswith("    ")


def _is_rendered_source_context_line(line: str) -> bool:
    return _SOURCE_CONTEXT_LINE_RE.match(line) is not None
