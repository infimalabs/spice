"""Session analysis primitives: message filtering and working phases."""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from spice.sessions import records
from spice.sessions.records import TurnRecord
from spice.sessions.util import (
    first_text,
    format_float,
    normalize_timestamp,
    parse_iso_ts,
)

QUESTION_WORDS = ("what", "why", "how", "can", "is", "are", "should", "could", "would")
DIRECTIVE_CUES = (
    "need to",
    "i want",
    "please",
    "let's",
    "let us",
    "help me",
    "show me",
    "give me",
    "implement",
    "fix",
    "remove",
    "change",
    "add",
    "build",
    "make sure",
)
CONSTRAINT_CUES = ("do not", "don't", "must", "only", "without", "never", "important")
TURN_ACTIVITY_ORDER = (
    "debugging",
    "implementation",
    "research",
    "execution",
    "discussion",
)
PHASE_GAP_MINUTES = 45
SECONDS_PER_MINUTE = 60
PHASE_GAP_SECONDS = PHASE_GAP_MINUTES * SECONDS_PER_MINUTE
TOP_PATH_LIMIT = 2
COMMAND_ACTIVITY_TOOL = "exec_command"
CANONICAL_TURN_ID_HEX_CHARS = 32


@dataclass(frozen=True, slots=True)
class MessageRecord:
    source_file: str
    ts: str
    turn_id: str | None
    side: str
    phase: str
    text: str
    primary_flavor: str
    flavor_tags: list[str]
    matched_cues: list[str]


@dataclass(frozen=True, slots=True)
class PhaseRecord:
    index: int
    family: str
    primary_archetype: str
    start_ts: str
    end_ts: str
    turns: list[TurnRecord]


def collect_messages(files: list[Path]) -> list[MessageRecord]:
    rows: list[MessageRecord] = []
    for path in files:
        rows.extend(_collect_messages_from_file(path))
    rows.sort(key=lambda row: (row.ts, row.source_file))
    return rows


def _collect_messages_from_file(path: Path) -> list[MessageRecord]:
    rows: list[MessageRecord] = []
    current_turn_id: str | None = None
    for obj in records.iter_events(path):
        ts = normalize_timestamp(obj.get("timestamp"))
        if not ts:
            continue
        payload = obj.get("payload") or {}
        top_type = obj.get("type")
        if top_type == "event_msg" and payload.get("type") == "task_started":
            current_turn_id = (
                payload.get("turn_id")
                if isinstance(payload.get("turn_id"), str)
                else None
            )
            continue
        if top_type == "event_msg" and payload.get("type") == "task_complete":
            current_turn_id = None
            continue
        if top_type == "event_msg" and payload.get("type") == "user_message":
            record = _user_event_message(path, ts, current_turn_id, payload)
        elif _is_response_message(top_type, payload):
            record = _response_message(path, ts, current_turn_id, payload)
        else:
            record = None
        if record is not None:
            rows.append(record)
    return rows


def _user_event_message(
    path: Path, ts: str, turn_id: str | None, payload: dict[str, Any]
) -> MessageRecord | None:
    message = payload.get("message")
    if not isinstance(message, str) or not message.strip():
        return None
    return _message_record(
        path,
        ts,
        turn_id,
        side="user",
        phase="prompt",
        text=message,
    )


def _is_response_message(top_type: Any, payload: dict[str, Any]) -> bool:
    return top_type == "response_item" and payload.get("type") == "message"


def _response_message(
    path: Path, ts: str, turn_id: str | None, payload: dict[str, Any]
) -> MessageRecord | None:
    text = first_text(payload.get("content"))
    if not text:
        return None
    role = payload.get("role")
    if role == "user":
        return _message_record(
            path,
            ts,
            turn_id,
            side="user",
            phase="prompt",
            text=text,
        )
    if role == "assistant":
        return _message_record(
            path,
            ts,
            turn_id,
            side="assistant",
            phase=str(payload.get("phase") or "commentary"),
            text=text,
        )
    return None


def _message_record(
    path: Path,
    ts: str,
    turn_id: str | None,
    *,
    side: str,
    phase: str,
    text: str,
) -> MessageRecord:
    primary, tags, cues = classify_message(text, side=side, phase=phase)
    return MessageRecord(
        source_file=str(path),
        ts=ts,
        turn_id=turn_id,
        side=side,
        phase=phase,
        text=text,
        primary_flavor=primary,
        flavor_tags=tags,
        matched_cues=cues,
    )


def classify_message(
    text: str, *, side: str, phase: str
) -> tuple[str, list[str], list[str]]:
    lower = text.lower()
    stripped = lower.lstrip()
    if side == "user":
        tags, cues, priority = _classify_user_message(lower, stripped)
    else:
        tags, cues, priority = _classify_assistant_message(lower, stripped, phase)
    tags = _dedupe(tags)
    primary = next((tag for tag in priority if tag in tags), tags[0])
    return primary, tags, _dedupe(cues)


def _classify_user_message(
    lower: str, stripped: str
) -> tuple[list[str], list[str], tuple[str, ...]]:
    tags: list[str] = []
    cues: list[str] = []
    question_cues = _question_cues(lower, stripped)
    if question_cues:
        tags.append("question_like")
        cues.extend(question_cues)
    _add_pattern_tag(tags, cues, "directive_like", _pattern_cues(lower, DIRECTIVE_CUES))
    _add_pattern_tag(
        tags, cues, "constraint_like", _pattern_cues(lower, CONSTRAINT_CUES)
    )
    if not tags:
        tags.append("plain")
    return tags, cues, ("constraint_like", "directive_like", "question_like", "plain")


def _classify_assistant_message(
    lower: str, stripped: str, phase: str
) -> tuple[list[str], list[str], tuple[str, ...]]:
    tags = ["final_answer" if phase == "final_answer" else "commentary"]
    cues = _question_cues(lower, stripped)
    if cues:
        tags.append("question_like")
    return tags, cues, ("final_answer", "commentary", "question_like")


def _question_cues(lower: str, stripped: str) -> list[str]:
    cues = ["?"] if "?" in lower else []
    cues.extend(word for word in QUESTION_WORDS if stripped.startswith(word))
    return cues


def _pattern_cues(lower: str, patterns: Iterable[str]) -> list[str]:
    return [pattern for pattern in patterns if pattern in lower]


def _add_pattern_tag(
    tags: list[str], cues: list[str], tag: str, matched: list[str]
) -> None:
    if matched:
        tags.append(tag)
        cues.extend(matched)


def filter_messages(
    rows: list[MessageRecord],
    *,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    turn_ids: list[str] | None = None,
    sides: list[str] | None = None,
    phase_kinds: list[str] | None = None,
    flavors: list[str] | None = None,
) -> list[MessageRecord]:
    needle = (contains or "").lower()
    turn_filter = set(_clean_values(turn_ids))
    side_filter = set(_clean_values(sides))
    phase_filter = set(_clean_values(phase_kinds))
    flavor_filter = {flavor.lower() for flavor in _clean_values(flavors)}
    return [
        row
        for row in rows
        if _message_matches(
            row,
            start=start,
            end=end,
            needle=needle,
            turn_filter=turn_filter,
            side_filter=side_filter,
            phase_filter=phase_filter,
            flavor_filter=flavor_filter,
        )
    ]


def _message_matches(
    row: MessageRecord,
    *,
    start: str | None,
    end: str | None,
    needle: str,
    turn_filter: set[str],
    side_filter: set[str],
    phase_filter: set[str],
    flavor_filter: set[str],
) -> bool:
    if start and row.ts < start:
        return False
    if end and row.ts > end:
        return False
    if turn_filter and (row.turn_id or "") not in turn_filter:
        return False
    if side_filter and row.side not in side_filter:
        return False
    if phase_filter and row.phase not in phase_filter:
        return False
    if flavor_filter and not any(
        tag.lower() in flavor_filter for tag in row.flavor_tags
    ):
        return False
    return not needle or needle in row.text.lower()


def segment_phases(turns: list[TurnRecord]) -> list[PhaseRecord]:
    if not turns:
        return []
    families = [phase_family_for_turn(turn) for turn in turns]
    bounds = [0]
    for index in range(1, len(turns)):
        previous = turns[index - 1]
        current = turns[index]
        if _gap_between_turns(previous, current) >= PHASE_GAP_SECONDS:
            bounds.append(index)
            continue
        if families[index] != families[index - 1] and _family_change_is_stable(
            families, index
        ):
            bounds.append(index)
    bounds.append(len(turns))
    phases = [
        _build_phase_record(phase_index, turns[start:end])
        for phase_index, (start, end) in enumerate(zip(bounds, bounds[1:]), start=1)
        if start < end
    ]
    return _merge_small_phases(phases)


def _build_phase_record(index: int, turns: list[TurnRecord]) -> PhaseRecord:
    family_counts: Counter[str] = Counter()
    for turn in turns:
        family_counts[phase_family_for_turn(turn)] += max(
            1, _turn_intensity_score(turn)
        )
    primary_counts = Counter(classify_turn(turn)[0] for turn in turns)
    return PhaseRecord(
        index=index,
        family=family_counts.most_common(1)[0][0],
        primary_archetype=primary_counts.most_common(1)[0][0],
        start_ts=turns[0].start_ts,
        end_ts=_turn_activity_ts(turns[-1]),
        turns=turns,
    )


def _merge_small_phases(phases: list[PhaseRecord]) -> list[PhaseRecord]:
    if not phases:
        return []
    groups: list[list[TurnRecord]] = []
    for phase in phases:
        if not groups:
            groups.append(list(phase.turns))
            continue
        current_family = phase.family
        previous_family = phase_family_for_turn(groups[-1][-1])
        if current_family == previous_family or len(phase.turns) == 1:
            groups[-1].extend(phase.turns)
        else:
            groups.append(list(phase.turns))
    return [
        _build_phase_record(index, group) for index, group in enumerate(groups, start=1)
    ]


def phase_payload(phase: PhaseRecord, example_count: int) -> dict[str, Any]:
    top_paths = Counter(_turn_path_signature(turn) for turn in phase.turns).most_common(
        TOP_PATH_LIMIT
    )
    examples = sorted(phase.turns, key=_turn_intensity_score, reverse=True)[
        : max(0, example_count)
    ]
    return {
        "index": phase.index,
        "family": phase.family,
        "primary_archetype": phase.primary_archetype,
        "start_ts": phase.start_ts,
        "end_ts": phase.end_ts,
        "turns": len(phase.turns),
        "completed": sum(1 for turn in phase.turns if turn.completed),
        "commands": sum(_command_activity_count(turn) for turn in phase.turns),
        "patches": sum(turn.patch_count for turn in phase.turns),
        "compactions": sum(turn.compaction_count for turn in phase.turns),
        "errors": sum(turn.error_count for turn in phase.turns),
        "duration_seconds": format_float(_phase_duration_seconds(phase)),
        "top_paths": [path for path, _ in top_paths],
        "examples": [
            {
                "start_ts": turn.start_ts,
                "turn_id": short_turn_id(turn.turn_id),
                "archetype": classify_turn(turn)[0],
                "path": _turn_path_signature(turn),
                "user": turn.user_messages[0] if turn.user_messages else None,
                "final": turn.final_answers[-1] if turn.final_answers else None,
            }
            for turn in examples
        ],
    }


def classify_turn(turn: TurnRecord) -> tuple[str, list[str]]:
    tags: list[str] = []
    if turn.error_count > 0:
        tags.append("debugging")
    if turn.patch_count > 0:
        tags.append("implementation")
    if turn.web_search_count > 0:
        tags.append("research")
    if _command_activity_count(turn) > 0 or turn.tool_calls:
        tags.append("execution")
    if not tags:
        tags.append("discussion")
    tags = _dedupe(tags)
    primary = next((tag for tag in TURN_ACTIVITY_ORDER if tag in tags), tags[0])
    return primary, tags


def short_turn_id(turn_id: str | None) -> str:
    if not turn_id:
        return "-"
    compact = turn_id.replace("-", "")
    if len(compact) >= CANONICAL_TURN_ID_HEX_CHARS and all(
        char in "0123456789abcdefABCDEF" for char in compact
    ):
        return compact[:8]
    return turn_id


def _phase_duration_seconds(phase: PhaseRecord) -> float | None:
    start = parse_iso_ts(phase.start_ts)
    end = parse_iso_ts(phase.end_ts)
    if start is None or end is None:
        return None
    return (end - start).total_seconds()


def _gap_between_turns(previous: TurnRecord, current: TurnRecord) -> float:
    previous_end = previous.end_ts or previous.last_activity_ts or previous.start_ts
    start = parse_iso_ts(previous_end)
    end = parse_iso_ts(current.start_ts)
    if start is None or end is None:
        return 0.0
    return (end - start).total_seconds()


def _family_change_is_stable(families: list[str], index: int) -> bool:
    previous_family = families[index - 1]
    current_family = families[index]
    backward = 1
    pointer = index - 2
    while pointer >= 0 and families[pointer] == previous_family:
        backward += 1
        pointer -= 1
    forward = 1
    pointer = index + 1
    while pointer < len(families) and families[pointer] == current_family:
        forward += 1
        pointer += 1
    return (
        (backward >= 2 and forward >= 2)
        or (backward >= 4 and forward >= 1)
        or (backward >= 1 and forward >= 3)
    )


def phase_family_for_turn(turn: TurnRecord) -> str:
    return classify_turn(turn)[0]


def _turn_activity_ts(turn: TurnRecord) -> str:
    return turn.end_ts or turn.last_activity_ts or turn.start_ts


def _turn_intensity_score(turn: TurnRecord) -> int:
    return (
        len(turn.user_messages) * 3
        + len(turn.assistant_commentary)
        + _command_activity_count(turn)
        + turn.patch_count * 5
        + turn.compaction_count * 8
        + turn.web_search_count * 3
    )


def _command_activity_count(turn: TurnRecord) -> int:
    return max(turn.command_count, turn.tool_calls.get(COMMAND_ACTIVITY_TOOL, 0))


def _turn_path_signature(turn: TurnRecord) -> str:
    stages = ["U" if turn.user_messages else "NOU"]
    if turn.assistant_commentary:
        stages.append("C")
    if _command_activity_count(turn) > 0 or turn.tool_calls:
        stages.append("CMD")
    if turn.web_search_count > 0:
        stages.append("WEB")
    if turn.patch_count > 0:
        stages.append("PATCH")
    if turn.compaction_count > 0:
        stages.append("CMP")
    if turn.final_answers:
        stages.append("FINAL")
    stages.append("CLOSED" if turn.completed else "OPEN")
    if turn.error_count > 0:
        stages.append("ERR")
    return ">".join(stages)


def _dedupe(values: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    deduped: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        deduped.append(value)
    return deduped


def _clean_values(values: Iterable[str] | None) -> list[str]:
    return [str(value).strip() for value in values or [] if str(value).strip()]
