"""Turn-structured records parsed out of an agent transcript.

The transcript is an append-only JSONL of timestamped events. This module
folds it into the shapes every forensic view shares:

* :class:`TurnRecord` — one operator ask: the user messages that opened it
  (each classified with a :class:`MessageShape`), the assistant commentary and
  final answers inside it, and activity counts (commands, patches, errors,
  web searches, compactions, touched files).
* :class:`CompactionRecord` — a context compaction with the prose around it.
* :class:`TokenUsage` — cumulative token accounting from token_count events.
* :class:`CommitRecord` — commit declarations harvested from assistant prose.
"""

from __future__ import annotations

import json
import re
import textwrap
from collections.abc import Callable, Iterator
from collections import Counter
from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path
from typing import Any

from spice.agent.driver import driver_for_transcript
from spice.errors import SpiceError
from spice.sessions.jsonl import iter_jsonl_lines, iter_jsonl_lines_reverse
from spice.sessions.util import first_text, int_or_zero, normalize_timestamp

COMMIT_SHA_RE = re.compile(r"\b[0-9a-f]{7,40}\b")
COMMIT_LINE_RE = re.compile(
    r"(?:^|\n)[^\n]*\b(?:commit(?:ted)?|sha)\b[^\n]*", re.IGNORECASE
)
UNPARSEABLE_COMPACTION_INTENT = "[unparseable compaction-summary intent]"


class MessageShape(StrEnum):
    """Deterministic class of one user-role transcript message."""

    HUMAN = "human"
    SKILL_MANTRA = "skill-mantra"
    COMPACTION_SUMMARY = "compaction-summary"
    TASK_NOTIFICATION = "task-notification"
    ENVIRONMENT_SCAFFOLD = "environment-scaffold"


# Each known non-human shape maps to exactly one class by its opening.
_SHAPE_PREFIXES: tuple[tuple[str, MessageShape], ...] = (
    # The spice bootstrap prompt: the full preamble, or the bare skill link
    # the wrapper sends on its own line.
    ("The linked skill below carries the full authority", MessageShape.SKILL_MANTRA),
    ("[$spice](", MessageShape.SKILL_MANTRA),
    # Claude's post-compaction rehydration summary.
    (
        "This session is being continued from a previous conversation",
        MessageShape.COMPACTION_SUMMARY,
    ),
    # Claude's background-task completion notice.
    ("<task-notification>", MessageShape.TASK_NOTIFICATION),
    # Codex session-scaffolding envelopes.
    ("<user_instructions>", MessageShape.ENVIRONMENT_SCAFFOLD),
    ("<environment_context>", MessageShape.ENVIRONMENT_SCAFFOLD),
    ("<ENVIRONMENT", MessageShape.ENVIRONMENT_SCAFFOLD),
    # Harness-injected retry nudges: no operator typed these.
    ("Your tool call was malformed", MessageShape.ENVIRONMENT_SCAFFOLD),
    (
        "[Your previous response had no visible output",
        MessageShape.ENVIRONMENT_SCAFFOLD,
    ),
)

# Future harness injections may introduce new tag names, but must occupy the
# complete message behind one matching outer envelope.
_TAG_ENVELOPE_RE = re.compile(
    r"<(?P<tag>[A-Za-z][\w-]*)(?:\s[^<>]*)?>.*</(?P=tag)>", re.DOTALL
)
_COMPACTION_INTENT_SECTION_RE = re.compile(
    r"(?im)^\s*\d+[.)]\s+\*{0,2}Primary Request and Intent\*{0,2}\s*:\s*(.*)$"
)
_COMPACTION_NEXT_SECTION_RE = re.compile(r"(?m)^\s*\d+[.)]\s+\S.*:\s*$")


def classify_user_message(text: str) -> MessageShape:
    """Classify a user-role message by its opening shape.

    One deterministic path: a known scaffold prefix maps to its specific class,
    an otherwise complete tag envelope is treated as forward-compatible
    environment scaffolding, and everything else is a human message.
    """
    stripped = text.strip()
    for prefix, shape in _SHAPE_PREFIXES:
        if stripped.startswith(prefix):
            return shape
    if _TAG_ENVELOPE_RE.fullmatch(stripped):
        return MessageShape.ENVIRONMENT_SCAFFOLD
    return MessageShape.HUMAN


def parse_compaction_summary_intent(text: str) -> str:
    match = _COMPACTION_INTENT_SECTION_RE.search(text)
    if match is None:
        return UNPARSEABLE_COMPACTION_INTENT
    section_start = match.end()
    next_match = _COMPACTION_NEXT_SECTION_RE.search(text, section_start)
    section_end = next_match.start() if next_match else len(text)
    inline = match.group(1).strip()
    body = textwrap.dedent(text[section_start:section_end]).strip()
    intent = "\n\n".join(piece for piece in [inline, body] if piece).strip()
    return intent or UNPARSEABLE_COMPACTION_INTENT


@dataclass(slots=True)
class UserMessage:
    text: str
    shape: MessageShape


@dataclass(slots=True)
class TurnRecord:
    source_file: str
    start_ts: str
    turn_id: str | None = None
    end_ts: str | None = None
    last_activity_ts: str | None = None
    completed: bool = False
    user_messages: list[UserMessage] = field(default_factory=list)
    assistant_commentary: list[str] = field(default_factory=list)
    final_answers: list[str] = field(default_factory=list)
    ordered_messages: list[tuple[str, str]] = field(default_factory=list)
    command_count: int = 0
    patch_count: int = 0
    web_search_count: int = 0
    error_count: int = 0
    compaction_count: int = 0
    tool_calls: Counter[str] = field(default_factory=Counter)
    touched_files: Counter[str] = field(default_factory=Counter)


@dataclass(slots=True)
class CompactionRecord:
    source_file: str
    ts: str
    last_assistant_before_text: str | None = None
    summary_after_text: str | None = None
    intent_text: str | None = None
    first_user_after_text: str | None = None


@dataclass(slots=True)
class TokenUsage:
    label: str
    input_tokens: int = 0
    cached_input_tokens: int = 0
    output_tokens: int = 0
    reasoning_output_tokens: int = 0
    total_tokens: int = 0
    snapshot_count: int = 0
    first_snapshot_ts: str | None = None
    last_snapshot_ts: str | None = None


@dataclass(slots=True)
class CommitRecord:
    start_ts: str
    turn_id: str | None
    source_file: str
    sha: str
    line: str
    user: str | None


def iter_events(
    path: Path,
    *,
    start: str | None = None,
    context_lines_before_start: int = 0,
) -> Iterator[dict[str, Any]]:
    driver = driver_for_transcript(path)
    for line in _iter_jsonl_lines_from_start(
        path, start, context_lines_before_start=context_lines_before_start
    ):
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(obj, dict):
            continue
        event = driver.normalize_transcript_line(obj)
        if event is not None:
            yield event


def _iter_jsonl_lines_from_start(
    path: Path, start: str | None, *, context_lines_before_start: int = 0
) -> Iterator[str]:
    if not start:
        yield from iter_jsonl_lines(path)
        return
    lines: list[str] = []
    context_count = 0
    for line in iter_jsonl_lines_reverse(path):
        ts = _line_timestamp(line)
        if ts and ts < start:
            if context_count >= context_lines_before_start:
                break
            lines.append(line)
            context_count += 1
            continue
        lines.append(line)
    yield from reversed(lines)


def _line_timestamp(line: str) -> str | None:
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return None
    if not isinstance(obj, dict):
        return None
    return normalize_timestamp(obj.get("timestamp"))


def collect_turns(files: list[Path], *, start: str | None = None) -> list[TurnRecord]:
    turns: list[TurnRecord] = []
    for path in files:
        turns.extend(_collect_turns_for_file(path, start=start))
    turns.sort(key=lambda turn: (turn.start_ts, turn.source_file))
    return turns


def _collect_turns_for_file(path: Path, *, start: str | None) -> list[TurnRecord]:
    turns: list[TurnRecord] = []
    current: TurnRecord | None = None
    for obj in iter_events(path, start=start):
        ts = normalize_timestamp(obj.get("timestamp")) or ""
        payload = obj.get("payload") or {}
        record_type = obj.get("type")
        if record_type == "event_msg":
            current = _apply_turn_event(turns, current, path, ts, payload)
            continue
        if record_type == "compacted":
            if current is not None:
                current.compaction_count += 1
                current.last_activity_ts = ts
            continue
        if record_type != "response_item":
            continue
        # Codex marks turn boundaries with task_started events; Claude has none,
        # so a real user prompt (carrying Claude's per-turn `prompt_id`) opens
        # the turn instead. Events without a prompt_id append to the current one.
        prompt_id = payload.get("prompt_id")
        if (
            prompt_id
            and payload.get("type") == "message"
            and payload.get("role") == "user"
            and (current is None or current.turn_id != prompt_id)
        ):
            current = TurnRecord(source_file=str(path), start_ts=ts, turn_id=prompt_id)
            turns.append(current)
        elif current is None:
            current = TurnRecord(source_file=str(path), start_ts=ts)
            turns.append(current)
        _apply_response_item(current, ts, payload)
    return turns


def _apply_turn_event(
    turns: list[TurnRecord],
    current: TurnRecord | None,
    path: Path,
    ts: str,
    payload: dict[str, Any],
) -> TurnRecord | None:
    inner = payload.get("type")
    if inner == "task_started":
        turn = TurnRecord(
            source_file=str(path),
            start_ts=ts,
            turn_id=(
                payload.get("turn_id")
                if isinstance(payload.get("turn_id"), str)
                else None
            ),
        )
        turns.append(turn)
        return turn
    if inner == "task_complete":
        if current is not None:
            current.completed = True
            current.end_ts = ts
            last = payload.get("last_agent_message")
            if isinstance(last, str) and last.strip():
                if not current.final_answers or current.final_answers[-1] != last:
                    current.final_answers.append(last)
                    current.ordered_messages.append(("final", last))
        return None
    if inner == "error":
        if current is not None:
            current.error_count += 1
            current.last_activity_ts = ts
    return current


def _apply_response_item(current: TurnRecord, ts: str, payload: dict[str, Any]) -> None:
    inner = payload.get("type")
    current.last_activity_ts = ts
    if inner == "message":
        text = first_text(payload.get("content")) or ""
        if not text:
            return
        role = payload.get("role")
        if role == "user":
            current.user_messages.append(
                UserMessage(text=text, shape=classify_user_message(text))
            )
            current.ordered_messages.append(("user", text))
            return
        if role == "assistant":
            if payload.get("phase") == "final_answer":
                current.final_answers.append(text)
                current.ordered_messages.append(("final", text))
            else:
                current.assistant_commentary.append(text)
                current.ordered_messages.append(("assistant", text))
        return
    if inner in ("function_call", "custom_tool_call"):
        name = str(payload.get("name") or "tool")
        current.tool_calls[name] += 1
        if name in ("shell", "local_shell", "exec_command", "container.exec"):
            current.command_count += 1
        if name == "apply_patch":
            current.patch_count += 1
            for touched in _patch_paths(payload.get("arguments")):
                current.touched_files[touched] += 1
        return
    if inner == "web_search_call":
        current.web_search_count += 1


_PATCH_PATH_RE = re.compile(
    r"\*\*\* (?:Add|Update|Delete) File: (?P<path>[^\n]+)", re.MULTILINE
)


def _patch_paths(raw_arguments: Any) -> list[str]:
    if not isinstance(raw_arguments, str) or not raw_arguments:
        return []
    try:
        arguments = json.loads(raw_arguments)
    except json.JSONDecodeError:
        arguments = {}
    patch_text = ""
    if isinstance(arguments, dict):
        candidate = arguments.get("input") or arguments.get("patch") or ""
        if isinstance(candidate, str):
            patch_text = candidate
    if not patch_text:
        patch_text = raw_arguments
    return [
        match.group("path").strip() for match in _PATCH_PATH_RE.finditer(patch_text)
    ]


def collect_compactions(
    files: list[Path], *, start: str | None = None
) -> list[CompactionRecord]:
    records: list[CompactionRecord] = []
    for path in files:
        last_assistant: str | None = None
        pending: CompactionRecord | None = None
        for obj in iter_events(path, start=start, context_lines_before_start=20):
            ts = normalize_timestamp(obj.get("timestamp")) or ""
            payload = obj.get("payload") or {}
            if obj.get("type") == "compacted":
                pending = CompactionRecord(
                    source_file=str(path),
                    ts=ts,
                    last_assistant_before_text=last_assistant,
                )
                records.append(pending)
                continue
            if obj.get("type") != "response_item" or payload.get("type") != "message":
                continue
            text = first_text(payload.get("content")) or ""
            if not text:
                continue
            if payload.get("role") == "assistant":
                last_assistant = text
            elif payload.get("role") == "user" and pending is not None:
                shape = classify_user_message(text)
                if shape is MessageShape.COMPACTION_SUMMARY:
                    if pending.summary_after_text is None:
                        pending.summary_after_text = text
                        pending.intent_text = parse_compaction_summary_intent(text)
                elif shape is MessageShape.HUMAN:
                    pending.first_user_after_text = text
                    pending = None
    records.sort(key=lambda record: (record.ts, record.source_file))
    return records


def collect_token_usage(files: list[Path]) -> list[TokenUsage]:
    usages: list[TokenUsage] = []
    for path in files:
        usage = TokenUsage(label=str(path))
        for obj in iter_events(path):
            payload = obj.get("payload") or {}
            if payload.get("type") != "token_count":
                continue
            info = payload.get("info") or {}
            last = info.get("last_token_usage") or {}
            if not last:
                continue
            ts = normalize_timestamp(obj.get("timestamp"))
            usage.snapshot_count += 1
            usage.input_tokens += int_or_zero(last.get("input_tokens"))
            usage.cached_input_tokens += int_or_zero(last.get("cached_input_tokens"))
            usage.output_tokens += int_or_zero(last.get("output_tokens"))
            usage.reasoning_output_tokens += int_or_zero(
                last.get("reasoning_output_tokens")
            )
            usage.total_tokens += int_or_zero(last.get("total_tokens"))
            if ts:
                usage.first_snapshot_ts = usage.first_snapshot_ts or ts
                usage.last_snapshot_ts = ts
        usages.append(usage)
    return usages


def combine_token_usage(usages: list[TokenUsage], *, label: str) -> TokenUsage:
    total = TokenUsage(label=label)
    for usage in usages:
        total.input_tokens += usage.input_tokens
        total.cached_input_tokens += usage.cached_input_tokens
        total.output_tokens += usage.output_tokens
        total.reasoning_output_tokens += usage.reasoning_output_tokens
        total.total_tokens += usage.total_tokens
        total.snapshot_count += usage.snapshot_count
        for ts in (usage.first_snapshot_ts,):
            if ts and (total.first_snapshot_ts is None or ts < total.first_snapshot_ts):
                total.first_snapshot_ts = ts
        for ts in (usage.last_snapshot_ts,):
            if ts and (total.last_snapshot_ts is None or ts > total.last_snapshot_ts):
                total.last_snapshot_ts = ts
    return total


def collect_commit_records(turns: list[TurnRecord]) -> list[CommitRecord]:
    records: list[CommitRecord] = []
    seen: set[str] = set()
    for turn in turns:
        sources = []
        if turn.final_answers:
            sources.append(turn.final_answers[-1])
        sources.extend(reversed(turn.assistant_commentary))
        for text in sources:
            for line in COMMIT_LINE_RE.findall(text):
                for sha in COMMIT_SHA_RE.findall(line):
                    if sha in seen or sha.isdigit():
                        continue
                    seen.add(sha)
                    records.append(
                        CommitRecord(
                            start_ts=turn.start_ts,
                            turn_id=turn.turn_id,
                            source_file=turn.source_file,
                            sha=sha,
                            line=" ".join(line.split()),
                            user=(
                                turn.user_messages[0].text
                                if turn.user_messages
                                else None
                            ),
                        )
                    )
    return records


def filter_turns(
    turns: list[TurnRecord],
    *,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    turn_ids: list[str] | None = None,
    tools: list[str] | None = None,
) -> list[TurnRecord]:
    needle = (contains or "").lower()
    turn_filter = {turn_id for turn_id in turn_ids or [] if turn_id}
    _reject_unavailable_turn_id_filter(turn_filter, turns)
    tool_filter = {tool for tool in tools or [] if tool}
    filters = _turn_filter_specs(
        start=start,
        end=end,
        needle=needle,
        turn_filter=turn_filter,
        tool_filter=tool_filter,
    )
    return [turn for turn in turns if _turn_matches_filters(turn, filters)]


TurnFilterSpec = tuple[Callable[[TurnRecord, Any], bool], Any]


def _reject_unavailable_turn_id_filter(
    turn_filter: set[str], turns: list[TurnRecord]
) -> None:
    # Fail loudly instead of silently returning nothing when a turn-id filter is
    # asked of turns that carry no ids. Codex stamps turn ids from task events;
    # Claude transcripts have no per-turn id, so every turn id is empty and any
    # --turn-id would match nothing — which reads as "no such turn" rather than
    # "unsupported here".
    if turn_filter and turns and not any(turn.turn_id for turn in turns):
        raise SpiceError(
            "filter by --start/--end/--contains, or drop --turn-id; turn-id "
            "filtering is unavailable for this transcript: its turns carry no "
            "per-turn id (e.g. Claude sessions, whose transcripts have no "
            "Codex-style turn events)"
        )


def _turn_filter_specs(
    *,
    start: str | None,
    end: str | None,
    needle: str,
    turn_filter: set[str],
    tool_filter: set[str],
) -> list[TurnFilterSpec]:
    specs: list[TurnFilterSpec] = []
    if start:
        specs.append((_matches_start, start))
    if end:
        specs.append((_matches_end, end))
    if turn_filter:
        specs.append((_matches_turn_id, turn_filter))
    if tool_filter:
        specs.append((_matches_tool, tool_filter))
    if needle:
        specs.append((_matches_text, needle))
    return specs


def _turn_matches_filters(turn: TurnRecord, filters: list[TurnFilterSpec]) -> bool:
    return all(matcher(turn, expected) for matcher, expected in filters)


def _matches_start(turn: TurnRecord, start: str) -> bool:
    return _turn_end_ts(turn) >= start


def _matches_end(turn: TurnRecord, end: str) -> bool:
    return turn.start_ts <= end


def _matches_turn_id(turn: TurnRecord, turn_filter: set[str]) -> bool:
    return (turn.turn_id or "") in turn_filter


def _matches_tool(turn: TurnRecord, tool_filter: set[str]) -> bool:
    return any(tool in turn.tool_calls for tool in tool_filter)


def _matches_text(turn: TurnRecord, needle: str) -> bool:
    return needle in _turn_text(turn)


def _turn_end_ts(turn: TurnRecord) -> str:
    return turn.end_ts or turn.last_activity_ts or turn.start_ts


def _turn_text(turn: TurnRecord) -> str:
    return "\n".join(
        [
            *(message.text for message in turn.user_messages),
            *turn.assistant_commentary,
            *turn.final_answers,
        ]
    ).lower()
