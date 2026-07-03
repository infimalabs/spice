from __future__ import annotations

import pytest

from spice.errors import SpiceError
from spice.sessions.learnings import (
    BRIEFING_LEARNING_LIMIT,
    LEARNING_STORE_LIMIT,
    LearningCandidate,
    confirm_learning_candidates,
    learning_store_path,
    load_learning_records,
    top_learning_records,
)

FIRST_CONFIRMATION_AT = 10.0
SECOND_CONFIRMATION_AT = 20.0
REFRESHED_OLDEST_CONFIRMATION_AT = 300.0
NEWEST_CONFIRMATION_AT = 301.0
TOP_REFRESH_CONFIRMATION_AT = 100.0


def _candidate(statement: str, *, stem: str = "session.learnings") -> LearningCandidate:
    return LearningCandidate(
        statement=statement,
        source_task="TASK-1",
        project_stem=stem,
        evidence=f"evidence for {statement}",
        source_slice_id="slice-1",
        source_turn_ids=("turn-1",),
    )


def _candidate_with_evidence(statement: str, evidence: str) -> LearningCandidate:
    return LearningCandidate(
        statement=statement,
        source_task="TASK-2",
        project_stem="session.learnings",
        evidence=evidence,
        source_slice_id="slice-2",
        source_turn_ids=("turn-2",),
    )


def test_missing_learning_store_reads_empty_and_path_is_per_stem(tmp_path):
    path = learning_store_path(tmp_path, "session.learnings")

    assert path == tmp_path / ".spice" / "learnings" / "session.learnings.jsonl"
    assert load_learning_records(tmp_path, "session.learnings") == []


@pytest.mark.parametrize("stem", ["", "../escape", "session/learnings"])
def test_learning_store_rejects_unsafe_project_stems(tmp_path, stem):
    with pytest.raises(SpiceError, match="invalid learning project stem"):
        learning_store_path(tmp_path, stem)


def test_learning_store_dedupes_duplicate_confirmations(tmp_path):
    confirm_learning_candidates(
        tmp_path,
        "session.learnings",
        [_candidate("Use spice dev pre-commit for the staged gate.")],
        now=FIRST_CONFIRMATION_AT,
    )
    confirmed = confirm_learning_candidates(
        tmp_path,
        "session.learnings",
        [
            _candidate_with_evidence(
                "  use spice dev pre-commit for the staged gate  ",
                "replacement duplicate evidence",
            )
        ],
        now=SECOND_CONFIRMATION_AT,
    )

    records = load_learning_records(tmp_path, "session.learnings")
    assert len(records) == 1
    assert confirmed == records
    assert records[0].statement == "Use spice dev pre-commit for the staged gate."
    assert records[0].normalized_statement == (
        "use spice dev pre-commit for the staged gate"
    )
    assert records[0].evidence == (
        "evidence for Use spice dev pre-commit for the staged gate."
    )
    assert records[0].source_task == "TASK-1"
    assert records[0].source_slice_id == "slice-1"
    assert records[0].source_turn_ids == ("turn-1",)
    assert records[0].created_at == FIRST_CONFIRMATION_AT
    assert records[0].last_confirmed_at == SECOND_CONFIRMATION_AT
    assert records[0].confirmation_count == 2


def test_learning_store_evicts_least_recently_confirmed_records(tmp_path):
    for index in range(LEARNING_STORE_LIMIT):
        confirm_learning_candidates(
            tmp_path,
            "session.learnings",
            [_candidate(f"Durable fact {index}")],
            now=float(index),
        )
    confirm_learning_candidates(
        tmp_path,
        "session.learnings",
        [_candidate("Durable fact 0")],
        now=REFRESHED_OLDEST_CONFIRMATION_AT,
    )
    confirm_learning_candidates(
        tmp_path,
        "session.learnings",
        [_candidate("Durable fact 200")],
        now=NEWEST_CONFIRMATION_AT,
    )

    normalized = {
        record.normalized_statement
        for record in load_learning_records(tmp_path, "session.learnings")
    }
    assert len(normalized) == LEARNING_STORE_LIMIT
    assert "durable fact 0" in normalized
    assert "durable fact 1" not in normalized
    assert "durable fact 200" in normalized


def test_top_learning_records_returns_five_recent_confirmations(tmp_path):
    for index in range(BRIEFING_LEARNING_LIMIT + 1):
        confirm_learning_candidates(
            tmp_path,
            "session.learnings",
            [_candidate(f"Durable fact {index}")],
            now=float(index),
        )
    confirm_learning_candidates(
        tmp_path,
        "session.learnings",
        [_candidate("Durable fact 0")],
        now=TOP_REFRESH_CONFIRMATION_AT,
    )

    top = top_learning_records(tmp_path, "session.learnings")

    assert [record.normalized_statement for record in top] == [
        "durable fact 0",
        "durable fact 5",
        "durable fact 4",
        "durable fact 3",
        "durable fact 2",
    ]
    assert len(top) == BRIEFING_LEARNING_LIMIT


def test_learning_store_raises_on_malformed_jsonl(tmp_path):
    path = learning_store_path(tmp_path, "session.learnings")
    path.parent.mkdir(parents=True)
    path.write_text("{not json}\n", encoding="utf-8")

    with pytest.raises(SpiceError, match="invalid learning record JSON"):
        load_learning_records(tmp_path, "session.learnings")
