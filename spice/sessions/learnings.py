"""Durable per-stem operational learnings for successor agents."""

from __future__ import annotations

import json
import re
import string
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from spice.agent.maxims import JudgeBackend, evaluate_maxim, judge_cli_backend
from spice.errors import SpiceError
from spice.paths import atomic_write_text, worktree_runtime_state_root
from spice.sessions import slices as session_slices
from spice.sessions.records import CompactionRecord, TurnRecord
from spice.sessions.slices import SliceRecord

LEARNINGS_DIRNAME = "learnings"
LEARNING_STORE_LIMIT = 200
BRIEFING_LEARNING_LIMIT = 5
LEARNING_RECORD_VERSION = 1
MAX_EXTRACTED_LEARNING_CANDIDATES = 8
MAX_LEARNING_SCAN_MESSAGES = 80
MAX_LEARNING_EVIDENCE_CHARS = 240
MIN_LEARNING_STATEMENT_WORDS = 4
LEARNING_JUDGE_MAX_ATTEMPTS = 2
LEARNING_JUDGE_CRITERION = (
    "The candidate is a durable, repo-general operational fact worth carrying "
    "to successor agents. It is reusable across future work in this repository "
    "and is not task status, speculation, a secret, one-off implementation "
    "trivia, or a statement without reusable operational value."
)
LEARNING_JUDGE_PROMPT_TEMPLATE = (
    'IFF "{statement}" SATISFIES "{maxim}": ANSWER ONLY "YES".\n'
    'IFF "{statement}" DOES NOT SATISFY "{maxim}": ANSWER ONLY "NO".\n'
)
LEARNING_SKIP_REJECTED = "rejected"
LEARNING_SKIP_AMBIGUOUS = "ambiguous"
LEARNING_SKIP_UNAVAILABLE = "unavailable"
LEARNING_SKIP_TIMEOUT = "timeout"
LEARNING_SKIP_JUDGE_ERROR = "judge_error"

_PROJECT_STEM_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
_TRAILING_STATEMENT_NOISE = string.punctuation + string.whitespace
_SENTENCE_RE = re.compile(r"[^.!?\n]+(?:[.!?]|$)")
_ERROR_CUES = (
    "assertionerror",
    "error",
    "exception",
    "failed",
    "failure",
    "traceback",
)
_FIX_CUES = (
    "changed",
    "fixed",
    "passes",
    "resolved",
    "reran",
    "switched",
)
_LEARNING_MARKER_PREFIXES = (
    "lesson learned",
    "lesson",
    "learning",
    "learned that",
    "learned",
    "going forward",
    "next time",
)
_LEARNING_IMPERATIVE_PREFIXES = (
    "always ",
    "avoid ",
    "do not ",
    "don't ",
    "never ",
    "prefer ",
    "remember to ",
    "run ",
    "use ",
)
_FIXED_IT_USING_PREFIX = "fixed it by using "
_FIXED_THIS_USING_PREFIX = "fixed this by using "
_SWITCHED_TO_PREFIX = "switched to "


@dataclass(frozen=True)
class LearningCandidate:
    statement: str
    source_task: str
    project_stem: str
    evidence: str = ""
    source_slice_id: str = ""
    source_turn_ids: tuple[str, ...] = ()


@dataclass(frozen=True)
class LearningRecord:
    statement: str
    normalized_statement: str
    source_task: str
    project_stem: str
    evidence: str
    source_slice_id: str
    source_turn_ids: tuple[str, ...]
    created_at: float
    last_confirmed_at: float
    confirmation_count: int

    def to_json(self) -> dict[str, Any]:
        return {
            "version": LEARNING_RECORD_VERSION,
            "statement": self.statement,
            "normalized_statement": self.normalized_statement,
            "source_task": self.source_task,
            "project_stem": self.project_stem,
            "evidence": self.evidence,
            "source_slice_id": self.source_slice_id,
            "source_turn_ids": list(self.source_turn_ids),
            "created_at": self.created_at,
            "last_confirmed_at": self.last_confirmed_at,
            "confirmation_count": self.confirmation_count,
        }


@dataclass(frozen=True)
class ExtractedLearningCandidate:
    statement: str
    normalized_statement: str
    source_task: str
    project_stem: str
    evidence: str
    source_slice_id: str
    source_turn_ids: tuple[str, ...]
    kind: str

    def to_learning_candidate(self) -> LearningCandidate:
        return LearningCandidate(
            statement=self.statement,
            source_task=self.source_task,
            project_stem=self.project_stem,
            evidence=self.evidence,
            source_slice_id=self.source_slice_id,
            source_turn_ids=self.source_turn_ids,
        )


@dataclass(frozen=True)
class LearningJudgeSkip:
    candidate: LearningCandidate
    reason: str
    detail: str = ""


@dataclass(frozen=True)
class LearningJudgeFilterResult:
    kept: tuple[LearningCandidate, ...]
    skipped: tuple[LearningJudgeSkip, ...]


@dataclass(frozen=True)
class _LearningMessage:
    text: str
    turn_id: str


def learning_store_path(repo_root: str | Path, project_stem: str) -> Path:
    stem = _validated_project_stem(project_stem)
    return (
        worktree_runtime_state_root(Path(repo_root))
        / LEARNINGS_DIRNAME
        / f"{stem}.jsonl"
    )


def load_learning_records(
    repo_root: str | Path, project_stem: str
) -> list[LearningRecord]:
    path = learning_store_path(repo_root, project_stem)
    if not path.exists():
        return []
    records: list[LearningRecord] = []
    for line_number, line in enumerate(
        path.read_text(encoding="utf-8").splitlines(), 1
    ):
        if not line.strip():
            continue
        try:
            payload = json.loads(line)
        except json.JSONDecodeError as exc:
            raise SpiceError(
                f"invalid learning record JSON in {path} line {line_number}: {exc}"
            ) from exc
        records.append(_record_from_json(payload, path=path, line_number=line_number))
    return records


def confirm_learning_candidates(
    repo_root: str | Path,
    project_stem: str,
    candidates: Iterable[LearningCandidate],
    *,
    now: float | None = None,
) -> list[LearningRecord]:
    stem = _validated_project_stem(project_stem)
    timestamp = float(time.time() if now is None else now)
    by_normalized = {
        record.normalized_statement: record
        for record in load_learning_records(repo_root, stem)
    }
    confirmed: list[LearningRecord] = []
    for candidate in candidates:
        record = _record_for_candidate(candidate, fallback_stem=stem, now=timestamp)
        previous = by_normalized.get(record.normalized_statement)
        if previous is not None:
            record = LearningRecord(
                statement=previous.statement,
                normalized_statement=previous.normalized_statement,
                source_task=previous.source_task,
                project_stem=previous.project_stem,
                evidence=previous.evidence,
                source_slice_id=previous.source_slice_id,
                source_turn_ids=previous.source_turn_ids,
                created_at=previous.created_at,
                last_confirmed_at=timestamp,
                confirmation_count=previous.confirmation_count + 1,
            )
        by_normalized[record.normalized_statement] = record
        confirmed.append(record)
    records = _evict_least_recently_confirmed(by_normalized.values())
    _write_learning_records(repo_root, stem, records)
    survivors = {record.normalized_statement: record for record in records}
    return [
        survivors[record.normalized_statement]
        for record in confirmed
        if record.normalized_statement in survivors
    ]


def top_learning_records(
    repo_root: str | Path,
    project_stem: str,
) -> list[LearningRecord]:
    return _rank_records(load_learning_records(repo_root, project_stem))[
        :BRIEFING_LEARNING_LIMIT
    ]


def claim_to_done_learning_slice(
    turns: Iterable[TurnRecord],
    compactions: Iterable[CompactionRecord],
    *,
    claim_started_at: str,
    done_at: str,
) -> SliceRecord | None:
    start = str(claim_started_at or "").strip()
    end = str(done_at or "").strip()
    if not start or not end or start > end:
        return None
    return session_slices.build_exact_slice(
        list(turns),
        list(compactions),
        start_ts=start,
        end_ts=end,
        basis="claim_to_done",
    )


def extract_learning_candidates_from_task_slice(
    turns: Iterable[TurnRecord],
    compactions: Iterable[CompactionRecord],
    *,
    claim_started_at: str,
    done_at: str,
    source_task: str,
    project_stem: str,
    max_candidates: int = MAX_EXTRACTED_LEARNING_CANDIDATES,
) -> tuple[ExtractedLearningCandidate, ...]:
    turn_rows = list(turns)
    slice_record = claim_to_done_learning_slice(
        turn_rows,
        list(compactions),
        claim_started_at=claim_started_at,
        done_at=done_at,
    )
    if slice_record is None:
        return ()
    slice_turns = session_slices.turns_overlapping(
        turn_rows, slice_record.start_ts, slice_record.end_ts
    )
    return extract_learning_candidates_from_slice(
        slice_record,
        slice_turns,
        source_task=source_task,
        project_stem=project_stem,
        max_candidates=max_candidates,
    )


def extract_learning_candidates_from_slice(
    slice_record: SliceRecord,
    turns: Iterable[TurnRecord],
    *,
    source_task: str,
    project_stem: str,
    max_candidates: int = MAX_EXTRACTED_LEARNING_CANDIDATES,
) -> tuple[ExtractedLearningCandidate, ...]:
    limit = max(0, int(max_candidates))
    if limit <= 0:
        return ()
    stem = _validated_project_stem(project_stem)
    messages = _learning_messages(turns)[:MAX_LEARNING_SCAN_MESSAGES]
    candidates: list[ExtractedLearningCandidate] = []
    seen: set[str] = set()
    pending_errors: list[_LearningMessage] = []
    for message in messages:
        if pending_errors and _is_fix_message(message.text):
            _append_extracted_candidate(
                candidates,
                seen,
                _fix_statement(message.text),
                source_task=source_task,
                project_stem=stem,
                evidence=_evidence_snippet(pending_errors[-1].text, message.text),
                source_slice_id=slice_record.slice_id,
                source_turn_ids=_dedupe_turn_ids(
                    (pending_errors[-1].turn_id, message.turn_id)
                ),
                kind="error_to_fix",
                limit=limit,
            )
            if len(candidates) >= limit:
                break
        for statement in _explicit_learning_statements(message.text):
            _append_extracted_candidate(
                candidates,
                seen,
                statement,
                source_task=source_task,
                project_stem=stem,
                evidence=_evidence_snippet(message.text),
                source_slice_id=slice_record.slice_id,
                source_turn_ids=_dedupe_turn_ids((message.turn_id,)),
                kind="explicit",
                limit=limit,
            )
            if len(candidates) >= limit:
                break
        if len(candidates) >= limit:
            break
        if _is_error_message(message.text):
            pending_errors.append(message)
    return tuple(candidates)


def judge_filter_learning_candidates(
    candidates: Iterable[LearningCandidate],
    *,
    backend: JudgeBackend = judge_cli_backend,
    max_attempts: int = LEARNING_JUDGE_MAX_ATTEMPTS,
) -> LearningJudgeFilterResult:
    kept: list[LearningCandidate] = []
    skipped: list[LearningJudgeSkip] = []
    for candidate in candidates:
        try:
            verdict = evaluate_maxim(
                LEARNING_JUDGE_CRITERION,
                _learning_judge_statement(candidate),
                template=LEARNING_JUDGE_PROMPT_TEMPLATE,
                backend=backend,
                max_attempts=max_attempts,
            )
        except Exception as exc:  # judge failures are distillation skips.
            skipped.append(
                LearningJudgeSkip(
                    candidate=candidate,
                    reason=_learning_judge_error_reason(exc),
                    detail=str(exc),
                )
            )
            continue
        if verdict.agrees:
            kept.append(candidate)
            continue
        skipped.append(
            LearningJudgeSkip(
                candidate=candidate,
                reason=LEARNING_SKIP_REJECTED,
                detail="judge returned NO",
            )
        )
    return LearningJudgeFilterResult(kept=tuple(kept), skipped=tuple(skipped))


def normalize_learning_statement(statement: str) -> str:
    normalized = " ".join(str(statement or "").split()).rstrip(
        _TRAILING_STATEMENT_NOISE
    )
    if not normalized:
        raise SpiceError("learning statement must be non-empty")
    return normalized.casefold()


def _learning_judge_statement(candidate: LearningCandidate) -> str:
    fields = [
        ("statement", candidate.statement),
        ("evidence", candidate.evidence),
        ("source_task", candidate.source_task),
        ("project_stem", candidate.project_stem),
    ]
    return "; ".join(
        f"{name}: {_compact_text(value)}"
        for name, value in fields
        if _compact_text(value)
    )


def _learning_judge_error_reason(exc: Exception) -> str:
    if isinstance(exc, subprocess.TimeoutExpired):
        return LEARNING_SKIP_TIMEOUT
    detail = str(exc).casefold()
    if "single yes/no" in detail:
        return LEARNING_SKIP_AMBIGUOUS
    if "could not launch" in detail or "exited with code" in detail:
        return LEARNING_SKIP_UNAVAILABLE
    return LEARNING_SKIP_JUDGE_ERROR


def _learning_messages(turns: Iterable[TurnRecord]) -> list[_LearningMessage]:
    messages: list[_LearningMessage] = []
    for turn in sorted(turns, key=lambda row: (row.start_ts, row.turn_id or "")):
        turn_id = str(turn.turn_id or "").strip()
        for _role, text in turn.ordered_messages:
            clean = _compact_text(text)
            if not clean:
                continue
            messages.append(
                _LearningMessage(
                    text=clean,
                    turn_id=turn_id,
                )
            )
    return messages


def _append_extracted_candidate(
    candidates: list[ExtractedLearningCandidate],
    seen: set[str],
    statement: str,
    *,
    source_task: str,
    project_stem: str,
    evidence: str,
    source_slice_id: str,
    source_turn_ids: tuple[str, ...],
    kind: str,
    limit: int,
) -> None:
    if len(candidates) >= limit:
        return
    statement = _clean_statement(statement)
    if len(statement.split()) < MIN_LEARNING_STATEMENT_WORDS:
        return
    try:
        normalized = normalize_learning_statement(statement)
    except SpiceError:
        return
    if normalized in seen:
        return
    seen.add(normalized)
    candidates.append(
        ExtractedLearningCandidate(
            statement=statement,
            normalized_statement=normalized,
            source_task=str(source_task or "").strip(),
            project_stem=project_stem,
            evidence=evidence,
            source_slice_id=str(source_slice_id or "").strip(),
            source_turn_ids=source_turn_ids,
            kind=kind,
        )
    )


def _explicit_learning_statements(text: str) -> list[str]:
    statements: list[str] = []
    for sentence in _sentences(text):
        lower = sentence.casefold()
        marker_statement = _marker_learning_statement(sentence, lower)
        if marker_statement:
            statements.append(marker_statement)
            continue
        if lower.startswith(_LEARNING_IMPERATIVE_PREFIXES):
            statements.append(sentence)
    return statements


def _marker_learning_statement(sentence: str, lower: str) -> str:
    for prefix in _LEARNING_MARKER_PREFIXES:
        if lower.startswith(prefix):
            return sentence[len(prefix) :].lstrip(" :-,")
    return ""


def _is_error_message(text: str) -> bool:
    lower = text.casefold()
    return any(cue in lower for cue in _ERROR_CUES)


def _is_fix_message(text: str) -> bool:
    lower = text.casefold()
    return any(cue in lower for cue in _FIX_CUES)


def _fix_statement(text: str) -> str:
    for sentence in _sentences(text):
        lower = sentence.casefold()
        if _FIXED_IT_USING_PREFIX in lower:
            start = lower.index(_FIXED_IT_USING_PREFIX) + len(_FIXED_IT_USING_PREFIX)
            return "Use " + sentence[start:]
        if _FIXED_THIS_USING_PREFIX in lower:
            start = lower.index(_FIXED_THIS_USING_PREFIX) + len(
                _FIXED_THIS_USING_PREFIX
            )
            return "Use " + sentence[start:]
        if lower.startswith(_SWITCHED_TO_PREFIX):
            return "Use " + sentence[len(_SWITCHED_TO_PREFIX) :]
        if any(cue in lower for cue in _FIX_CUES):
            return sentence
    return text


def _sentences(text: str) -> list[str]:
    return [_clean_statement(match.group(0)) for match in _SENTENCE_RE.finditer(text)]


def _clean_statement(text: str) -> str:
    return _compact_text(text).strip(" :-,").rstrip(_TRAILING_STATEMENT_NOISE)


def _compact_text(text: object) -> str:
    return " ".join(str(text or "").split())


def _evidence_snippet(*parts: str) -> str:
    text = " -> ".join(_compact_text(part) for part in parts if _compact_text(part))
    if len(text) <= MAX_LEARNING_EVIDENCE_CHARS:
        return text
    return text[: MAX_LEARNING_EVIDENCE_CHARS - 3].rstrip() + "..."


def _dedupe_turn_ids(turn_ids: Iterable[str]) -> tuple[str, ...]:
    seen: set[str] = set()
    result: list[str] = []
    for turn_id in turn_ids:
        clean = str(turn_id or "").strip()
        if not clean or clean in seen:
            continue
        seen.add(clean)
        result.append(clean)
    return tuple(result)


def _record_for_candidate(
    candidate: LearningCandidate, *, fallback_stem: str, now: float
) -> LearningRecord:
    stem = _validated_project_stem(candidate.project_stem or fallback_stem)
    statement = " ".join(str(candidate.statement or "").split())
    normalized = normalize_learning_statement(statement)
    if not statement:
        raise SpiceError("learning statement must be non-empty")
    return LearningRecord(
        statement=statement,
        normalized_statement=normalized,
        source_task=str(candidate.source_task or "").strip(),
        project_stem=stem,
        evidence=" ".join(str(candidate.evidence or "").split()),
        source_slice_id=str(candidate.source_slice_id or "").strip(),
        source_turn_ids=tuple(
            str(turn_id).strip()
            for turn_id in candidate.source_turn_ids
            if str(turn_id).strip()
        ),
        created_at=now,
        last_confirmed_at=now,
        confirmation_count=1,
    )


def _record_from_json(payload: Any, *, path: Path, line_number: int) -> LearningRecord:
    if not isinstance(payload, dict):
        raise SpiceError(
            f"learning record in {path} line {line_number} must be an object"
        )
    version = payload.get("version")
    if version != LEARNING_RECORD_VERSION:
        raise SpiceError(
            f"learning record in {path} line {line_number} has unsupported "
            f"version {version!r}"
        )
    statement = _required_string(payload, "statement", path, line_number)
    normalized = _required_string(payload, "normalized_statement", path, line_number)
    project_stem = _validated_project_stem(
        _required_string(payload, "project_stem", path, line_number)
    )
    turn_ids = payload.get("source_turn_ids", [])
    if not isinstance(turn_ids, list) or not all(
        isinstance(turn_id, str) for turn_id in turn_ids
    ):
        raise SpiceError(
            f"learning record in {path} line {line_number} has invalid source_turn_ids"
        )
    return LearningRecord(
        statement=statement,
        normalized_statement=normalized,
        source_task=_required_string(payload, "source_task", path, line_number),
        project_stem=project_stem,
        evidence=_required_string(payload, "evidence", path, line_number),
        source_slice_id=_required_string(payload, "source_slice_id", path, line_number),
        source_turn_ids=tuple(turn_ids),
        created_at=_required_number(payload, "created_at", path, line_number),
        last_confirmed_at=_required_number(
            payload, "last_confirmed_at", path, line_number
        ),
        confirmation_count=_required_int(
            payload, "confirmation_count", path, line_number
        ),
    )


def _write_learning_records(
    repo_root: str | Path, project_stem: str, records: list[LearningRecord]
) -> None:
    path = learning_store_path(repo_root, project_stem)
    text = "".join(
        json.dumps(record.to_json(), sort_keys=True) + "\n" for record in records
    )
    atomic_write_text(path, text)


def _rank_records(records: Iterable[LearningRecord]) -> list[LearningRecord]:
    return sorted(
        records,
        key=lambda record: (
            -record.last_confirmed_at,
            -record.confirmation_count,
            record.normalized_statement,
        ),
    )


def _evict_least_recently_confirmed(
    records: Iterable[LearningRecord],
) -> list[LearningRecord]:
    return _rank_records(records)[:LEARNING_STORE_LIMIT]


def _validated_project_stem(project_stem: str) -> str:
    stem = str(project_stem or "").strip()
    if not _PROJECT_STEM_RE.fullmatch(stem):
        raise SpiceError(f"invalid learning project stem {project_stem!r}")
    return stem


def _required_string(payload: dict[str, Any], field: str, path: Path, line: int) -> str:
    value = payload.get(field)
    if not isinstance(value, str):
        raise SpiceError(f"learning record in {path} line {line} has invalid {field}")
    return value


def _required_number(
    payload: dict[str, Any], field: str, path: Path, line: int
) -> float:
    value = payload.get(field)
    if not isinstance(value, (int, float)):
        raise SpiceError(f"learning record in {path} line {line} has invalid {field}")
    return float(value)


def _required_int(payload: dict[str, Any], field: str, path: Path, line: int) -> int:
    value = payload.get(field)
    if not isinstance(value, int):
        raise SpiceError(f"learning record in {path} line {line} has invalid {field}")
    return value


LEARNING_STORAGE_SURFACE = (
    ExtractedLearningCandidate,
    LearningJudgeFilterResult,
    LearningJudgeSkip,
    LearningCandidate,
    LearningRecord,
    claim_to_done_learning_slice,
    confirm_learning_candidates,
    extract_learning_candidates_from_slice,
    extract_learning_candidates_from_task_slice,
    judge_filter_learning_candidates,
    learning_store_path,
    load_learning_records,
    top_learning_records,
)
