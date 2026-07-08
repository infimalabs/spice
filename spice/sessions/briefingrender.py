"""Rendering support helpers for session briefing."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Protocol, TypeVar

from spice.mail.inbox import (
    INBOX_RESPONSE_ROW,
    InboxItem,
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    collect_refused_inbox_items,
    inbox_ack_state_context_rows,
    inbox_deadletter_context_rows,
    inbox_item_age_seconds,
    inbox_item_key,
    relative_time_for_path,
)
from spice.paths import repo_root_from_cwd
from spice.sessions import records


class AskCandidateLike(Protocol):
    @property
    def label(self) -> str: ...

    @property
    def timestamp(self) -> str: ...

    @property
    def text(self) -> str: ...


Candidate = TypeVar("Candidate", bound=AskCandidateLike)


def drop_human_ask_duplicates(candidates: Sequence[Candidate]) -> list[Candidate]:
    real_asks = {
        (candidate.timestamp, candidate.text)
        for candidate in candidates
        if candidate.label != "human"
    }
    return [
        candidate
        for candidate in candidates
        if candidate.label != "human"
        or (candidate.timestamp, candidate.text) not in real_asks
    ]


def active_filter_lines(
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
    turn_ids: Sequence[str] | None,
    tools: Sequence[str] | None,
) -> list[str]:
    rows: list[str] = []
    if start:
        rows.append(f"  start={start}")
    if end:
        rows.append(f"  end={end}")
    if contains:
        rows.append(f"  contains={contains}")
    if turn_ids:
        rows.append(f"  turn_ids={', '.join(turn_ids)}")
    if tools:
        rows.append(f"  tools={', '.join(tools)}")
    return rows


def filter_compactions(
    compactions: list[records.CompactionRecord],
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
) -> list[records.CompactionRecord]:
    needle = (contains or "").lower()
    kept: list[records.CompactionRecord] = []
    for record in compactions:
        if start and record.ts < start:
            continue
        if end and record.ts > end:
            continue
        if needle:
            haystack = "\n".join(
                [
                    record.last_assistant_before_text or "",
                    record.intent_text or "",
                    record.summary_after_text or "",
                    record.first_user_after_text or "",
                ]
            ).lower()
            if needle not in haystack:
                continue
        kept.append(record)
    return kept


def apply_output_budget(
    text: str,
    *,
    max_lines: int | None,
    max_bytes: int | None,
    explain: bool,
) -> str:
    lines = text.splitlines()
    original_lines = len(lines)
    original_bytes = len(text.encode("utf-8"))
    pruned = False
    line_budget = max_lines if max_lines and max_lines > 0 else None
    byte_budget = max_bytes if max_bytes and max_bytes > 0 else None
    reserve = 1
    if line_budget and len(lines) > line_budget:
        keep = max(0, line_budget - reserve)
        lines = lines[:keep]
        pruned = True

    def pruning_note(retained_lines: int, retained_bytes: int) -> str:
        return (
            "Pruning "
            f"original_lines={original_lines} original_bytes={original_bytes} "
            f"max_lines={line_budget or '-'} max_bytes={byte_budget or '-'} "
            f"retained_lines={retained_lines} retained_content_bytes={retained_bytes}"
        )

    def rendered(with_note: bool) -> str:
        out = list(lines)
        if with_note:
            text_without_note = "\n".join(out)
            retained_bytes = len(text_without_note.encode("utf-8"))
            out.append(pruning_note(len(lines) + 1, retained_bytes))
        return "\n".join(out)

    if byte_budget:
        while lines and len(rendered(explain and pruned).encode("utf-8")) > byte_budget:
            lines.pop()
            pruned = True
        if (
            not lines
            and len(rendered(explain and pruned).encode("utf-8")) > byte_budget
        ):
            return truncate_to_bytes(rendered(explain and pruned), byte_budget)
    if pruned:
        return rendered(True)
    return "\n".join(lines)


def truncate_to_bytes(text: str, max_bytes: int) -> str:
    return text.encode("utf-8")[: max(0, max_bytes)].decode("utf-8", errors="ignore")


def inbox_lines(*, max_consumed_age_seconds: int | None = None) -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    items = collect_inbox_items(str(repo_root))
    deadletters = _filter_consumed_by_age(
        collect_deadlettered_inbox_items(str(repo_root)),
        max_age_seconds=max_consumed_age_seconds,
    )
    refused = _filter_consumed_by_age(
        collect_refused_inbox_items(str(repo_root)),
        max_age_seconds=max_consumed_age_seconds,
    )
    lines = ["Inbox", f"  pending={len(items)}"]
    for item in items:
        lines.append(
            f"  key={inbox_item_key(item.name)} "
            f"age={relative_time_for_path(item.source_path)}"
        )
    if items:
        lines.append(f"  {INBOX_RESPONSE_ROW}")
    if deadletters:
        lines.append(f"  deadlettered={len(deadletters)}")
        lines.extend(f"  {line}" for line in inbox_deadletter_context_rows(deadletters))
    if refused:
        lines.append(f"  refused={len(refused)}")
        lines.extend(f"  {line}" for line in inbox_ack_state_context_rows(refused))
    return lines


def _filter_consumed_by_age(
    items: Sequence[InboxItem], *, max_age_seconds: int | None
) -> list[InboxItem]:
    if max_age_seconds is None:
        return list(items)
    return [
        item for item in items if _item_age_seconds(item) <= max(0, max_age_seconds)
    ]


def _item_age_seconds(item: InboxItem) -> float:
    try:
        return inbox_item_age_seconds(item)
    except OSError:
        return 0.0
