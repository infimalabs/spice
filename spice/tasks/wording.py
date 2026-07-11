"""Detect suspect wording in task creation text."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

from spice import policy, textcontext
from spice.agent import maxims
from spice.paths import repo_root_from_cwd
from spice.policyconfig import resolve_policy
from spice.studies import taste

TASK_WORDING_SOURCE_TITLE = "title"
TASK_WORDING_SOURCE_DESCRIPTION = "description"
TASK_WORDING_SOURCE_ACCEPTANCE = "acceptance"
TASK_WORDING_FAMILY_TASTE = "taste"


@dataclass(frozen=True)
class TaskWordingMatch:
    source: str
    matched: str
    trigger_family: str
    reason: str


def detect_task_creation_wording(
    *,
    title: str,
    description: str | None = None,
    acceptance: Sequence[str] = (),
    project: str | None = None,
    flow: Sequence[str] = (),
    repo_root: Path | None = None,
    driver_name: str | None = None,
) -> tuple[TaskWordingMatch, ...]:
    # Creation callers may carry metadata through this seam; only prose is scanned.
    _ = (project, flow)
    root = repo_root if repo_root is not None else repo_root_from_cwd()
    items = _task_creation_text_items(
        title=title,
        description=description,
        acceptance=acceptance,
    )
    matches: list[TaskWordingMatch] = []
    words = (
        dict(resolve_policy(root).taste.words)
        if root is not None
        else dict(policy.TASTE_WORD_SUGGESTIONS)
    )
    for finding in taste.scan_taste_texts(
        items,
        words=words,
        match_filter=_is_not_explicitly_negated,
    ):
        matches.append(
            TaskWordingMatch(
                source=finding.source,
                matched=finding.word,
                trigger_family=TASK_WORDING_FAMILY_TASTE,
                reason=_taste_reason(finding.suggestion),
            )
        )
    for source, text in items:
        for match in maxims.triggered_maxim_matches(
            [text],
            repo_root=root,
            driver_name=driver_name,
            match_filter=_is_not_explicitly_negated,
        ):
            matches.append(
                TaskWordingMatch(
                    source=source,
                    matched=match.trigger,
                    trigger_family=match.bag_name,
                    reason=match.message,
                )
            )
    return tuple(matches)


def _is_not_explicitly_negated(text: str, match_start: int) -> bool:
    return not textcontext.has_explicit_negation_before(text, match_start)


def _task_creation_text_items(
    *,
    title: str,
    description: str | None,
    acceptance: Sequence[str],
) -> tuple[tuple[str, str], ...]:
    items: list[tuple[str, str]] = []
    if title:
        items.append((TASK_WORDING_SOURCE_TITLE, title))
    if description:
        items.append((TASK_WORDING_SOURCE_DESCRIPTION, description))
    for item in acceptance:
        if item:
            items.append((TASK_WORDING_SOURCE_ACCEPTANCE, item))
    return tuple(items)


def _taste_reason(suggestion: str) -> str:
    if suggestion:
        return f"consider {suggestion!r}"
    return "consider rephrasing; it adds no value"
