from __future__ import annotations

import json
import subprocess

import pytest

from spice.errors import SpiceError
from spice.sessions import learnings, records
from spice.sessions.learnings import (
    BRIEFING_LEARNING_LIMIT,
    LEARNING_STORE_LIMIT,
    LearningCandidate,
    confirm_learning_candidates,
    claim_to_done_learning_slice,
    extract_learning_candidates_from_task_slice,
    judge_filter_learning_candidates,
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


def test_learning_extractor_selects_claim_to_done_slice_and_candidates(tmp_path):
    transcript = tmp_path / "learning.jsonl"
    _write_jsonl(
        transcript,
        [
            *_turn_events(
                "turn-before",
                start_second=0,
                messages=[("assistant", "Lesson: Ignore outside transcript noise.")],
            ),
            *_turn_events(
                "turn-fix",
                start_second=10,
                messages=[
                    (
                        "assistant",
                        "The pytest command failed with ModuleNotFoundError.",
                    ),
                    (
                        "assistant",
                        "I fixed it by using uv run python for test commands.",
                    ),
                ],
            ),
            *_turn_events(
                "turn-lesson",
                start_second=20,
                messages=[
                    (
                        "assistant",
                        "Lesson: Keep command output visible when validating fixes.",
                    )
                ],
            ),
            *_turn_events(
                "turn-after",
                start_second=40,
                messages=[("assistant", "Lesson: Ignore work after task done.")],
            ),
        ],
    )
    turns = records.collect_turns([transcript])
    compactions = records.collect_compactions([transcript])

    slice_record = claim_to_done_learning_slice(
        turns,
        compactions,
        claim_started_at=_ts(10),
        done_at=_ts(30),
    )
    candidates = extract_learning_candidates_from_task_slice(
        turns,
        compactions,
        claim_started_at=_ts(10),
        done_at=_ts(30),
        source_task="LEARNIN-00000001",
        project_stem="session.learnings",
    )

    assert slice_record is not None
    assert slice_record.basis == "claim_to_done"
    assert slice_record.turn_ids == ["turn-fix", "turn-lesson"]
    assert [
        (
            candidate.kind,
            candidate.statement,
            candidate.normalized_statement,
            candidate.source_turn_ids,
        )
        for candidate in candidates
    ] == [
        (
            "error_to_fix",
            "Use uv run python for test commands",
            "use uv run python for test commands",
            ("turn-fix",),
        ),
        (
            "explicit",
            "Keep command output visible when validating fixes",
            "keep command output visible when validating fixes",
            ("turn-lesson",),
        ),
    ]
    assert candidates[0].source_task == "LEARNIN-00000001"
    assert candidates[0].project_stem == "session.learnings"
    assert candidates[0].source_slice_id == slice_record.slice_id
    assert "ModuleNotFoundError" in candidates[0].evidence
    assert "uv run python" in candidates[0].evidence
    assert [
        candidate.to_learning_candidate().statement for candidate in candidates
    ] == [
        "Use uv run python for test commands",
        "Keep command output visible when validating fixes",
    ]


def test_learning_extractor_is_bounded_deduped_and_side_effect_free(
    tmp_path, monkeypatch
):
    transcript = tmp_path / "bounded.jsonl"
    messages = [
        ("assistant", f"Lesson: Candidate fact {index} should stay ordered.")
        for index in range(6)
    ]
    messages.append(("assistant", "Lesson: Candidate fact 0 should stay ordered."))
    _write_jsonl(
        transcript,
        [
            *_turn_events(
                "turn-many",
                start_second=0,
                messages=messages,
            )
        ],
    )
    turns = records.collect_turns([transcript])
    compactions = records.collect_compactions([transcript])

    def fail_storage_call(*_args, **_kwargs):
        raise AssertionError("extraction must not write or read the learning store")

    monkeypatch.setattr(learnings, "confirm_learning_candidates", fail_storage_call)
    monkeypatch.setattr(learnings, "load_learning_records", fail_storage_call)

    candidates = extract_learning_candidates_from_task_slice(
        turns,
        compactions,
        claim_started_at=_ts(0),
        done_at=_ts(10),
        source_task="LEARNIN-00000002",
        project_stem="session.learnings",
        max_candidates=3,
    )

    assert [candidate.normalized_statement for candidate in candidates] == [
        "candidate fact 0 should stay ordered",
        "candidate fact 1 should stay ordered",
        "candidate fact 2 should stay ordered",
    ]
    assert len(candidates) == 3
    assert {candidate.source_slice_id for candidate in candidates} == {
        candidates[0].source_slice_id
    }
    assert all(candidate.source_turn_ids == ("turn-many",) for candidate in candidates)
    assert not (tmp_path / ".spice" / "learnings").exists()


def test_learning_judge_filter_keeps_yes_and_skips_no_candidates():
    durable = _candidate("Use spice dev pre-commit for the staged gate.")
    status = _candidate("This task is currently waiting for review.")
    answers = iter(("YES", "NO"))
    prompts: list[str] = []

    def backend(prompt: str) -> str:
        prompts.append(prompt)
        return next(answers)

    result = judge_filter_learning_candidates(
        (durable, status),
        backend=backend,
        max_attempts=1,
    )

    assert result.kept == (durable,)
    assert [(skip.candidate, skip.reason, skip.detail) for skip in result.skipped] == [
        (status, learnings.LEARNING_SKIP_REJECTED, "judge returned NO")
    ]
    assert len(prompts) == 2
    assert "durable, repo-general operational fact" in prompts[0]


def test_learning_judge_filter_records_ambiguous_reply_as_skip():
    candidate = _candidate("Use uv run python for repo test commands.")

    result = judge_filter_learning_candidates(
        (candidate,),
        backend=lambda _prompt: "MAYBE",
        max_attempts=1,
    )

    assert result.kept == ()
    assert [(skip.candidate, skip.reason) for skip in result.skipped] == [
        (candidate, learnings.LEARNING_SKIP_AMBIGUOUS)
    ]
    assert "single YES/NO" in result.skipped[0].detail


def test_learning_judge_filter_records_unavailable_backend_as_skip():
    candidate = _candidate("Use spice task next after task phase boundaries.")

    def unavailable(_prompt: str) -> str:
        raise SpiceError("could not launch 'afm-cli': missing")

    result = judge_filter_learning_candidates((candidate,), backend=unavailable)

    assert result.kept == ()
    assert [(skip.candidate, skip.reason, skip.detail) for skip in result.skipped] == [
        (
            candidate,
            learnings.LEARNING_SKIP_UNAVAILABLE,
            "could not launch 'afm-cli': missing",
        )
    ]


def test_learning_judge_filter_records_timeout_as_skip():
    candidate = _candidate("Use blocking process waits for validation commands.")

    def timeout(_prompt: str) -> str:
        raise subprocess.TimeoutExpired("judge", 5)

    result = judge_filter_learning_candidates((candidate,), backend=timeout)

    assert result.kept == ()
    assert [(skip.candidate, skip.reason) for skip in result.skipped] == [
        (candidate, learnings.LEARNING_SKIP_TIMEOUT)
    ]


def _turn_events(
    turn_id: str,
    *,
    start_second: int,
    messages: list[tuple[str, str]],
) -> list[dict[str, object]]:
    events: list[dict[str, object]] = [
        {
            "timestamp": _ts(start_second),
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": turn_id},
        }
    ]
    for index, (role, text) in enumerate(messages, 1):
        events.append(
            {
                "timestamp": _ts(start_second + index),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": role,
                    "content": [{"text": text}],
                },
            }
        )
    events.append(
        {
            "timestamp": _ts(start_second + len(messages) + 1),
            "type": "event_msg",
            "payload": {"type": "task_complete"},
        }
    )
    return events


def _write_jsonl(path, events) -> None:
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )


def _ts(second: int) -> str:
    return f"2026-01-01T00:00:{second:02d}.000Z"
