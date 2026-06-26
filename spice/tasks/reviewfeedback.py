"""Route task-review feedback through the ordinary inbox steering loop."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

from spice.errors import SpiceError
from spice.mail.inbox import (
    compose_inbox_text,
    default_inbox_name,
    inbox_item_key,
    write_inbox_item,
)
from spice.tasks import config, identity, tw
from spice.worktrees import list_worktrees

REVIEW_FEEDBACK_PRIORITY = "review"


@dataclass(frozen=True)
class ReviewFeedbackResult:
    status: str
    detail: str
    key: str = ""
    target_repo_root: str = ""

    def output_line(self) -> str:
        parts = [f"review-feedback {self.status}"]
        if self.key:
            parts.append(f"key={self.key}")
        if self.target_repo_root:
            parts.append(f"target={self.target_repo_root}")
        if self.detail:
            parts.append(self.detail)
        return "; ".join(parts)


def emit_review_feedback(
    reviewed_row: dict[str, Any],
    *,
    finding: str,
    note: str | None,
    followups: Sequence[str],
    reviewer: str,
    reviewed_at: str,
) -> ReviewFeedbackResult:
    if finding.strip().casefold() == "clean":
        return ReviewFeedbackResult("clean", "clean review does not emit feedback")
    review_author = str(reviewed_row.get("review_author") or "").strip()
    if not review_author:
        result = ReviewFeedbackResult("target-inactive", "review_author is empty")
        _record_feedback_status(reviewed_row, result)
        return result
    target = _resolve_active_author_target(review_author)
    if target.status != "delivered":
        result = ReviewFeedbackResult(target.status, target.detail)
        _record_feedback_status(reviewed_row, result)
        return result
    body = _feedback_body(
        reviewed_row,
        finding=finding,
        note=note,
        followups=followups,
        reviewer=reviewer,
        reviewed_at=reviewed_at,
    )
    path = write_inbox_item(
        Path(target.repo_root),
        default_inbox_name(),
        compose_inbox_text(body=body, priority=REVIEW_FEEDBACK_PRIORITY, stop=False),
        dedupe_pending_text=True,
    )
    result = ReviewFeedbackResult(
        "delivered",
        "source=task-review",
        key=inbox_item_key(path.name),
        target_repo_root=str(target.repo_root),
    )
    _record_feedback_status(reviewed_row, result)
    return result


@dataclass(frozen=True)
class _TargetResolution:
    status: str
    detail: str
    repo_root: str = ""


def _resolve_active_author_target(review_author: str) -> _TargetResolution:
    try:
        records = list_worktrees(cwd=config.repo_root())
    except RuntimeError as exc:
        return _TargetResolution("target-inactive", f"worktree discovery failed: {exc}")
    matches: list[Path] = []
    author_keys = _actor_keys(review_author)
    for record in records:
        if record.bare or not record.path.exists():
            continue
        try:
            status = _agent_status(record.path)
        except SpiceError:
            continue
        if not status.running:
            continue
        if author_keys & _actor_keys(status.thread_id):
            matches.append(record.path.resolve())
    unique = sorted({path for path in matches})
    if len(unique) == 1:
        return _TargetResolution(
            "delivered",
            "active author target resolved",
            str(unique[0]),
        )
    if not unique:
        return _TargetResolution(
            "target-inactive", "no active target for review_author"
        )
    return _TargetResolution(
        "target-ambiguous",
        "multiple active targets for review_author: "
        + ", ".join(path.as_posix() for path in unique),
    )


def _agent_status(repo_root: Path) -> Any:
    from spice.agent.lifecycle import agent_status

    return agent_status(repo_root)


def _actor_keys(value: str) -> set[str]:
    text = str(value or "").strip()
    if not text:
        return set()
    keys = {text}
    if ":" in text:
        keys.add(text.split(":", 1)[1])
    return keys | {tw.canonical_actor(key) for key in keys}


def _feedback_body(
    reviewed_row: dict[str, Any],
    *,
    finding: str,
    note: str | None,
    followups: Sequence[str],
    reviewer: str,
    reviewed_at: str,
) -> str:
    reviewed = identity.render_handle(reviewed_row)
    followup_text = ", ".join(str(item) for item in followups if str(item)) or "-"
    note_text = (note or "-").strip() or "-"
    return "\n".join(
        [
            f"Peer review feedback for {reviewed}",
            "source=task-review",
            f"reviewed_task={reviewed}",
            f"reviewed_at={reviewed_at or '-'}",
            f"reviewer={reviewer or '-'}",
            f"finding={finding or '-'}",
            f"followups={followup_text}",
            "",
            "Review note:",
            note_text,
            "",
            "Allocator note:",
            "Do not switch tasks solely because of this message. Keep the current "
            "claim valid, and inspect linked follow-ups when the allocator assigns "
            "them.",
        ]
    )


def _record_feedback_status(
    reviewed_row: dict[str, Any],
    result: ReviewFeedbackResult,
) -> None:
    uuid = str(reviewed_row.get("uuid") or "").strip()
    if not uuid:
        return
    try:
        tw.run([uuid, "annotate", "--", result.output_line()])
    except SpiceError:
        return
