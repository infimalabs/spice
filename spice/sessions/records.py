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
from collections.abc import Callable, Iterable, Iterator
from collections import Counter
from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path
from typing import Any

from spice.agent.driver import driver_for_transcript
from spice.errors import SpiceError
from spice.transcript.events import (
    AssistantText,
    CommandExecution,
    Compaction,
    ContextUsage,
    Image,
    Reasoning,
    ToolCall,
    ToolOutput,
    TranscriptEvent,
    TurnBoundary,
    UserMessage as TranscriptUserMessage,
    WebSearch,
)
from spice.transcript.reader import TranscriptEventReader
from spice.transcript.timestamps import normalize_timestamp

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
) -> Iterator[TranscriptEvent]:
    driver = driver_for_transcript(path)
    reader = TranscriptEventReader(path, driver)
    read = (
        reader.read(
            "since",
            start_timestamp=start,
            context_lines_before_start=context_lines_before_start,
        )
        if start
        else reader.read("forward")
    )
    yield from read.events


def collect_turns(files: list[Path], *, start: str | None = None) -> list[TurnRecord]:
    turns: list[TurnRecord] = []
    for path in files:
        turns.extend(_collect_turns_for_file(path, start=start))
    turns.sort(key=lambda turn: (turn.start_ts, turn.source_file))
    return turns


def _collect_turns_for_file(path: Path, *, start: str | None) -> list[TurnRecord]:
    return collect_turns_from_events(path, iter_events(path, start=start))


def collect_turns_from_events(
    path: Path, events: Iterable[TranscriptEvent]
) -> list[TurnRecord]:
    """Fold one already-decoded typed stream into forensic turns."""
    turns: list[TurnRecord] = []
    current: TurnRecord | None = None
    for event in events:
        ts = normalize_timestamp(event.at.timestamp) or ""
        if isinstance(event, TurnBoundary):
            current = _apply_turn_boundary(turns, current, path, ts, event)
            continue
        if isinstance(event, Compaction):
            if not event.boundary:
                continue
            if current is not None:
                current.compaction_count += 1
                current.last_activity_ts = ts
            continue
        if isinstance(event, (ContextUsage, CommandExecution)):
            continue
        if (
            isinstance(event, TranscriptUserMessage)
            and event.transcript_kind == "event_msg"
        ):
            continue
        prompt_id = (
            event.prompt_id
            if isinstance(event, TranscriptUserMessage)
            and event.transcript_kind != "event_msg"
            else None
        )
        if prompt_id and (current is None or current.turn_id != prompt_id):
            current = TurnRecord(source_file=str(path), start_ts=ts, turn_id=prompt_id)
            turns.append(current)
        elif current is None and (
            isinstance(
                event,
                (
                    AssistantText,
                    Image,
                    Reasoning,
                    ToolCall,
                    ToolOutput,
                    WebSearch,
                ),
            )
            or (
                isinstance(event, TranscriptUserMessage)
                and event.transcript_kind != "event_msg"
            )
        ):
            current = TurnRecord(source_file=str(path), start_ts=ts)
            turns.append(current)
        if current is not None:
            _apply_turn_fact(current, ts, event)
    return turns


def _apply_turn_boundary(
    turns: list[TurnRecord],
    current: TurnRecord | None,
    path: Path,
    ts: str,
    event: TurnBoundary,
) -> TurnRecord | None:
    if event.kind == "started":
        turn = TurnRecord(
            source_file=str(path),
            start_ts=ts,
            turn_id=event.turn_id,
        )
        turns.append(turn)
        return turn
    if event.kind == "completed":
        if current is not None:
            current.completed = True
            current.end_ts = ts
            last = event.last_assistant_message
            if last and last.strip():
                if not current.final_answers or current.final_answers[-1] != last:
                    current.final_answers.append(last)
                    current.ordered_messages.append(("final", last))
        return None
    if event.kind == "error":
        if current is not None:
            current.error_count += 1
            current.last_activity_ts = ts
    return current


def _apply_turn_fact(current: TurnRecord, ts: str, event: TranscriptEvent) -> None:
    current.last_activity_ts = ts
    if isinstance(event, TranscriptUserMessage):
        if event.text and event.role == "user" and event.transcript_kind != "event_msg":
            current.user_messages.append(
                UserMessage(
                    text=event.text,
                    shape=classify_user_message(event.text),
                )
            )
            current.ordered_messages.append(("user", event.text))
        return
    if isinstance(event, AssistantText):
        if event.final or event.phase == "final_answer":
            current.final_answers.append(event.text)
            current.ordered_messages.append(("final", event.text))
        else:
            current.assistant_commentary.append(event.text)
            current.ordered_messages.append(("assistant", event.text))
        return
    if isinstance(event, ToolCall):
        name = event.name or "tool"
        current.tool_calls[name] += 1
        if name in ("shell", "local_shell", "exec_command", "container.exec"):
            current.command_count += 1
        if name == "apply_patch":
            current.patch_count += 1
            for touched in _patch_paths(event.arguments):
                current.touched_files[touched] += 1
        return
    if isinstance(event, WebSearch):
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
        events = iter_events(path, start=start, context_lines_before_start=20)
        records.extend(collect_compactions_from_events(path, events))
    records.sort(key=lambda record: (record.ts, record.source_file))
    return records


def collect_compactions_from_events(
    path: Path, events: Iterable[TranscriptEvent]
) -> list[CompactionRecord]:
    """Fold one already-decoded typed stream into compaction records."""
    records: list[CompactionRecord] = []
    last_assistant: str | None = None
    pending: CompactionRecord | None = None
    for event in events:
        ts = normalize_timestamp(event.at.timestamp) or ""
        if isinstance(event, Compaction) and event.boundary:
            pending = CompactionRecord(
                source_file=str(path),
                ts=ts,
                last_assistant_before_text=last_assistant,
            )
            records.append(pending)
            continue
        if isinstance(event, AssistantText) and event.text:
            last_assistant = event.text
        elif (
            isinstance(event, TranscriptUserMessage)
            and event.role == "user"
            and event.text
            and event.transcript_kind != "event_msg"
            and pending is not None
        ):
            shape = classify_user_message(event.text)
            if shape is MessageShape.COMPACTION_SUMMARY:
                if pending.summary_after_text is None:
                    pending.summary_after_text = event.text
                    pending.intent_text = parse_compaction_summary_intent(event.text)
            elif shape is MessageShape.HUMAN:
                pending.first_user_after_text = event.text
                pending = None
    return records


def collect_token_usage(files: list[Path]) -> list[TokenUsage]:
    usages: list[TokenUsage] = []
    for path in files:
        usage = TokenUsage(label=str(path))
        for event in iter_events(path):
            if not isinstance(event, ContextUsage):
                continue
            last = event.last
            ts = normalize_timestamp(event.at.timestamp)
            usage.snapshot_count += 1
            usage.input_tokens += last.input_tokens
            usage.cached_input_tokens += last.cached_input_tokens
            usage.output_tokens += last.output_tokens
            usage.reasoning_output_tokens += last.reasoning_output_tokens
            usage.total_tokens += last.total_tokens
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
