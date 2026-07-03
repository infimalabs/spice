"""Durable per-stem operational learnings for successor agents."""

from __future__ import annotations

import json
import re
import string
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from spice.errors import SpiceError
from spice.paths import atomic_write_text, state_dir

LEARNINGS_DIRNAME = "learnings"
LEARNING_STORE_LIMIT = 200
BRIEFING_LEARNING_LIMIT = 5
LEARNING_RECORD_VERSION = 1

_PROJECT_STEM_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
_TRAILING_STATEMENT_NOISE = string.punctuation + string.whitespace


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


def learning_store_path(repo_root: str | Path, project_stem: str) -> Path:
    stem = _validated_project_stem(project_stem)
    return state_dir(Path(repo_root)) / LEARNINGS_DIRNAME / f"{stem}.jsonl"


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


def normalize_learning_statement(statement: str) -> str:
    normalized = " ".join(str(statement or "").split()).rstrip(
        _TRAILING_STATEMENT_NOISE
    )
    if not normalized:
        raise SpiceError("learning statement must be non-empty")
    return normalized.casefold()


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
    LearningCandidate,
    LearningRecord,
    confirm_learning_candidates,
    learning_store_path,
    load_learning_records,
    top_learning_records,
)
