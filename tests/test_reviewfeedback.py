from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from spice.mail.inbox import (
    collect_inbox_items,
    inbox_request_body,
    parse_inbox_payload,
    pending_inbox_count,
)
from spice.tasks import reviewfeedback
from spice.worktrees import WorktreeRecord


def _reviewed_row() -> dict[str, object]:
    return {
        "uuid": "reviewed-uuid",
        "incepted": "1jNJvRyn",
        "project": "task.review",
        "description": "Reviewed task",
        "review_author": "agent-a",
    }


def _patch_targets(
    monkeypatch,
    *,
    cwd: Path,
    targets: list[tuple[Path, bool, str]],
) -> list[list[str]]:
    calls: list[list[str]] = []
    for path, _running, _thread_id in targets:
        path.mkdir(parents=True, exist_ok=True)
    records = [
        WorktreeRecord(path=path, branch="refs/heads/main", bare=False)
        for path, _running, _thread_id in targets
    ]
    status_by_path = {
        path.resolve(): SimpleNamespace(running=running, thread_id=thread_id)
        for path, running, thread_id in targets
    }

    monkeypatch.setattr(reviewfeedback.config, "repo_root", lambda: cwd)
    monkeypatch.setattr(reviewfeedback, "list_worktrees", lambda cwd: records)
    monkeypatch.setattr(
        reviewfeedback,
        "_agent_status",
        lambda path: status_by_path[Path(path).resolve()],
    )
    monkeypatch.setattr(reviewfeedback.tw, "run", lambda args: calls.append(args))
    return calls


def test_review_feedback_delivers_deduped_review_guidance(tmp_path, monkeypatch):
    calls = _patch_targets(
        monkeypatch,
        cwd=tmp_path,
        targets=[(tmp_path / "repo-a", True, "agent-a")],
    )

    first = reviewfeedback.emit_review_feedback(
        _reviewed_row(),
        finding="changes",
        note="needs coverage",
        followups=["FOLLOW-1", "FOLLOW-2"],
        reviewer="agent-b",
        reviewed_at="2026-01-02T00:00:00Z",
    )
    second = reviewfeedback.emit_review_feedback(
        _reviewed_row(),
        finding="changes",
        note="needs coverage",
        followups=["FOLLOW-1", "FOLLOW-2"],
        reviewer="agent-b",
        reviewed_at="2026-01-02T00:00:00Z",
    )
    items = collect_inbox_items(tmp_path / "repo-a")
    payload = parse_inbox_payload(items[0].text)
    body = inbox_request_body(items[0].text)

    assert first.status == "delivered"
    assert second.status == "delivered"
    assert first.key == second.key
    assert pending_inbox_count(tmp_path / "repo-a") == 1
    assert payload.priority == "review"
    # The [REVIEW] prefix rides the same idiom as [MAXIM]: it IS the priority
    # marker, so parsing strips it from the body and no Priority: header exists.
    assert items[0].text.startswith(f"{reviewfeedback.REVIEW_FEEDBACK_PREFIX} ")
    assert "Priority:" not in items[0].text
    assert body == "\n".join(
        [
            "Peer feedback for REVIEW-1jNJvRyn:",
            "",
            "> needs coverage",
            "",
            "\N{SMILING FACE WITH SMILING EYES} Verbalize any noteworthy lessons "
            "and internalize this as guidance—any follow-ups it raises are already "
            "on the board, so there is nothing to capture or act on now! Keep "
            "working your current task; do not re-file what the system already "
            "filed.",
        ]
    )
    assert calls == [
        ["reviewed-uuid", "annotate", "--", first.output_line()],
        ["reviewed-uuid", "annotate", "--", second.output_line()],
    ]


def test_review_feedback_records_inactive_target_noop(tmp_path, monkeypatch):
    calls = _patch_targets(
        monkeypatch,
        cwd=tmp_path,
        targets=[(tmp_path / "repo-a", False, "agent-a")],
    )

    result = reviewfeedback.emit_review_feedback(
        _reviewed_row(),
        finding="changes",
        note="needs work",
        followups=["FOLLOW-1"],
        reviewer="agent-b",
        reviewed_at="2026-01-02T00:00:00Z",
    )

    assert result.status == "target-inactive"
    assert pending_inbox_count(tmp_path / "repo-a") == 0
    assert calls[0][1] == "annotate"
    assert "target-inactive" in calls[0][-1]


def test_review_feedback_records_ambiguous_target_noop(tmp_path, monkeypatch):
    calls = _patch_targets(
        monkeypatch,
        cwd=tmp_path,
        targets=[
            (tmp_path / "repo-a", True, "agent-a"),
            (tmp_path / "repo-b", True, "agent-a"),
        ],
    )

    result = reviewfeedback.emit_review_feedback(
        _reviewed_row(),
        finding="changes",
        note="needs work",
        followups=["FOLLOW-1"],
        reviewer="agent-b",
        reviewed_at="2026-01-02T00:00:00Z",
    )

    assert result.status == "target-ambiguous"
    assert pending_inbox_count(tmp_path / "repo-a") == 0
    assert pending_inbox_count(tmp_path / "repo-b") == 0
    assert calls[0][1] == "annotate"
    assert "target-ambiguous" in calls[0][-1]


def test_clean_review_feedback_does_not_emit_or_annotate(tmp_path, monkeypatch):
    calls = _patch_targets(
        monkeypatch,
        cwd=tmp_path,
        targets=[(tmp_path / "repo-a", True, "agent-a")],
    )

    result = reviewfeedback.emit_review_feedback(
        _reviewed_row(),
        finding="clean",
        note="looks good",
        followups=[],
        reviewer="agent-b",
        reviewed_at="2026-01-02T00:00:00Z",
    )

    assert result.status == "clean"
    assert pending_inbox_count(tmp_path / "repo-a") == 0
    assert calls == []


def test_review_feedback_refuses_delivery_to_the_emitting_tree(tmp_path, monkeypatch):
    tree = tmp_path / "repo-a"
    calls = _patch_targets(
        monkeypatch,
        cwd=tree,
        targets=[(tree, True, "agent-a")],
    )

    result = reviewfeedback.emit_review_feedback(
        _reviewed_row(),
        finding="changes",
        note="needs work",
        followups=["FOLLOW-1"],
        reviewer="agent-b",
        reviewed_at="2026-01-02T00:00:00Z",
    )

    assert result.status == "self-tree"
    assert result.target_repo_root == str(tree.resolve())
    assert pending_inbox_count(tree) == 0
    assert calls[0][1] == "annotate"
    assert "self-tree" in calls[0][-1]


def test_review_feedback_target_selection_compares_trees_and_nothing_else(tmp_path):
    emitting = tmp_path / "repo-e"
    peer = tmp_path / "repo-f"

    refused = reviewfeedback._select_target([emitting], emitting)
    delivered = reviewfeedback._select_target([peer], emitting)
    peer_preferred = reviewfeedback._select_target(sorted([emitting, peer]), emitting)

    assert refused.status == "self-tree"
    assert refused.repo_root == str(emitting)
    assert delivered.status == "delivered"
    assert delivered.repo_root == str(peer)
    assert peer_preferred.status == "delivered"
    assert peer_preferred.repo_root == str(peer)
    assert refused.status != delivered.status
