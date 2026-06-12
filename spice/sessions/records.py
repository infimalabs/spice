"""Turn-structured records parsed out of an agent transcript.

The transcript is an append-only JSONL of timestamped events. This module
folds it into the shapes every forensic view shares:

* :class:`TurnRecord` — one operator ask: the user messages that opened it,
  the assistant commentary and final answers inside it, and activity counts
  (commands, patches, errors, web searches, compactions, touched files).
* :class:`CompactionRecord` — a context compaction with the prose around it.
* :class:`TokenUsage` — cumulative token accounting from token_count events.
* :class:`CommitRecord` — commit declarations harvested from assistant prose.
"""

from __future__ import annotations

import json
import re
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterator

from spice.sessions.util import first_text, int_or_zero, normalize_timestamp

COMMIT_SHA_RE = re.compile(r"\b[0-9a-f]{7,40}\b")
COMMIT_LINE_RE = re.compile(
    r"(?:^|\n)[^\n]*\b(?:commit(?:ted)?|sha)\b[^\n]*", re.IGNORECASE
)
SCAFFOLDING_PREFIXES = ("<user_instructions>", "<environment_context>", "<ENVIRONMENT")


@dataclass(slots=True)
class TurnRecord:
    source_file: str
    start_ts: str
    turn_id: str | None = None
    end_ts: str | None = None
    last_activity_ts: str | None = None
    completed: bool = False
    user_messages: list[str] = field(default_factory=list)
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


def iter_events(path: Path) -> Iterator[dict[str, Any]]:
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(obj, dict):
                yield obj


def is_scaffolding_text(text: str) -> bool:
    stripped = text.lstrip()
    return stripped.startswith(SCAFFOLDING_PREFIXES)


def collect_turns(files: list[Path]) -> list[TurnRecord]:
    turns: list[TurnRecord] = []
    for path in files:
        turns.extend(_collect_turns_for_file(path))
    turns.sort(key=lambda turn: (turn.start_ts, turn.source_file))
    return turns


def _collect_turns_for_file(path: Path) -> list[TurnRecord]:
    turns: list[TurnRecord] = []
    current: TurnRecord | None = None
    for obj in iter_events(path):
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
        if current is None:
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
            current.user_messages.append(text)
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


def collect_compactions(files: list[Path]) -> list[CompactionRecord]:
    records: list[CompactionRecord] = []
    for path in files:
        last_assistant: str | None = None
        pending: CompactionRecord | None = None
        for obj in iter_events(path):
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
                if not is_scaffolding_text(text):
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
                            user=turn.user_messages[0] if turn.user_messages else None,
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
    kept: list[TurnRecord] = []
    needle = (contains or "").lower()
    turn_filter = {turn_id for turn_id in turn_ids or [] if turn_id}
    tool_filter = {tool for tool in tools or [] if tool}
    for turn in turns:
        turn_end = turn.end_ts or turn.last_activity_ts or turn.start_ts
        if start and turn_end < start:
            continue
        if end and turn.start_ts > end:
            continue
        if turn_filter and (turn.turn_id or "") not in turn_filter:
            continue
        if tool_filter and not any(tool in turn.tool_calls for tool in tool_filter):
            continue
        if needle:
            haystack = "\n".join(
                [*turn.user_messages, *turn.assistant_commentary, *turn.final_answers]
            ).lower()
            if needle not in haystack:
                continue
        kept.append(turn)
    return kept
