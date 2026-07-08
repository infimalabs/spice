"""Dirty worktree and Git posture helpers for session briefing."""

from __future__ import annotations

import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, TypedDict

from spice.errors import SpiceError
from spice.mail.inbox import format_relative_seconds
from spice.paths import repo_root_from_cwd
from spice.policy import MAGIC_BASELINE_REF
from spice.policyconfig import ComplexityPolicy, resolve_policy
from spice.studies import complexity, fileloc, magicnums, repodocs, shape
from spice.studies.walk import is_excluded_path

DIRTY_PRESSURE_PREVIEW_LIMIT = 6
PREVIEW_CHARS = 200


@dataclass(frozen=True)
class DirtyComplexityRegression:
    path: str
    function_name: str
    metric: str
    value: int
    active_threshold: int
    baseline_value: int | None


class DirtyWorktreePressure(TypedDict, total=False):
    available: bool
    dirtyPathCount: int
    scannedPathCount: int
    fileCountWithPressure: int
    totalFindings: int
    fileLocFindingCount: int
    complexityRegressionCount: int
    magicRegressionCount: int
    severity: str
    summary: list[str]
    summaryOverflow: int
    errors: list[str]
    oldestDirtyAgeSeconds: int
    oldestDirtyPath: str
    newestDirtyAgeSeconds: int
    newestDirtyPath: str


def _clip(text: str | None, limit: int = PREVIEW_CHARS) -> str:
    if not text:
        return "-"
    flat = " ".join(text.split())
    if len(flat) <= limit:
        return flat
    return flat[: limit - 1].rstrip() + "…"


def dirty_path_count() -> int:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return 0
    pressure = _build_dirty_worktree_pressure(repo_root=repo_root)
    return int(pressure.get("dirtyPathCount") or 0)


def git_posture_lines() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return ["Git", "  repo=-"]
    branch = _git_read(repo_root, "branch", "--show-current") or "-"
    upstream = _git_read(
        repo_root, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"
    )
    ahead = behind = "0"
    if upstream:
        delta = _git_read(
            repo_root, "rev-list", "--left-right", "--count", "HEAD...@{u}"
        )
        parts = delta.split()
        if len(parts) == 2:
            ahead, behind = parts
    else:
        upstream = "-"
        ahead = behind = "-"
    dirty_pressure = _build_dirty_worktree_pressure(repo_root=repo_root)
    dirty_count = int(dirty_pressure.get("dirtyPathCount") or 0)
    dirty_text = "clean" if dirty_count == 0 else f"{dirty_count} path(s)"
    lines = [
        "Git",
        f"  branch={branch} upstream={upstream} ahead={ahead} behind={behind}",
        f"  dirty={dirty_text}",
    ]
    if dirty_count:
        lines.extend(_dirty_pressure_lines(dirty_pressure))
    return lines


def _empty_dirty_worktree_pressure() -> DirtyWorktreePressure:
    return {
        "available": True,
        "dirtyPathCount": 0,
        "scannedPathCount": 0,
        "fileCountWithPressure": 0,
        "totalFindings": 0,
        "fileLocFindingCount": 0,
        "complexityRegressionCount": 0,
        "magicRegressionCount": 0,
        "severity": "none",
        "summary": [],
        "summaryOverflow": 0,
        "errors": [],
    }


def _build_dirty_worktree_pressure(*, repo_root: Path) -> DirtyWorktreePressure:
    dirty = _dirty_paths(repo_root)
    if not dirty:
        return _empty_dirty_worktree_pressure()
    relevant_paths = [
        path
        for path in dirty
        if not is_excluded_path(path, repo_root=repo_root)
        and (repo_root / path).exists()
    ]
    file_loc_findings, complexity_regressions, magic_regressions, errors = (
        _collect_dirty_pressure_findings(relevant_paths, repo_root=repo_root)
    )
    per_file_rules, ordered_summary = _dirty_pressure_summary(
        file_loc_findings,
        complexity_regressions,
        magic_regressions,
    )
    total_findings = (
        len(file_loc_findings) + len(complexity_regressions) + len(magic_regressions)
    )
    return {
        "available": True,
        "dirtyPathCount": len(dirty),
        "scannedPathCount": len(relevant_paths),
        "fileCountWithPressure": len(per_file_rules),
        "totalFindings": total_findings,
        "fileLocFindingCount": len(file_loc_findings),
        "complexityRegressionCount": len(complexity_regressions),
        "magicRegressionCount": len(magic_regressions),
        "severity": _dirty_pressure_severity(
            file_loc_findings=file_loc_findings,
            complexity_regressions=complexity_regressions,
            magic_regressions=magic_regressions,
            errors=errors,
        ),
        "summary": ordered_summary[:DIRTY_PRESSURE_PREVIEW_LIMIT],
        "summaryOverflow": max(0, len(ordered_summary) - DIRTY_PRESSURE_PREVIEW_LIMIT),
        "errors": errors,
        **_dirty_path_ages(dirty, repo_root=repo_root),
    }


def _dirty_paths(repo_root: Path) -> list[Path]:
    raw_paths: set[Path] = set()
    command_specs = (
        ("diff", "--name-only", "-z", "--diff-filter=ACMRD"),
        ("diff", "--cached", "--name-only", "-z", "--diff-filter=ACMRD"),
        ("ls-files", "--others", "--exclude-standard", "-z"),
    )
    for args in command_specs:
        for raw_path in _git_read_z(repo_root, *args):
            candidate = Path(raw_path)
            if candidate.parts:
                raw_paths.add(candidate)
    return sorted(raw_paths)


def _collect_dirty_pressure_findings(
    relevant_paths: list[Path], *, repo_root: Path
) -> tuple[
    list[fileloc.LocFinding],
    list[DirtyComplexityRegression],
    list[magicnums.MagicFinding],
    list[str],
]:
    errors: list[str] = []
    file_loc_findings: list[fileloc.LocFinding] = []
    complexity_regressions: list[DirtyComplexityRegression] = []
    magic_regressions: list[magicnums.MagicFinding] = []
    resolved = resolve_policy(repo_root)
    file_shape = resolved.file_shape
    complexity_bounds = resolved.complexity
    generated_patterns = (
        *resolved.file_shape_paths.generated_patterns,
        *shape.generated_path_patterns(repo_root),
    )

    try:
        file_loc_findings = fileloc.scan_loc_violations(
            relevant_paths,
            limit=file_shape.line_limit,
            flex_limit_value=file_shape.line_flex_limit,
            byte_limit=file_shape.byte_limit,
            byte_flex_limit_value=file_shape.byte_flex_limit,
            root=repo_root,
            source_suffixes=resolved.file_shape_paths.source_suffixes,
            generated_patterns=generated_patterns,
            repo_doc_paths=set(
                repodocs.repo_truth_doc_candidate_paths(repo_root, resolved)
            ),
            lockfile_suffixes=resolved.lockfiles.suffixes,
            lockfile_names=resolved.lockfiles.names,
            bounds_for_path=resolved.jittered_file_shape_for_path,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("file-loc", exc))

    try:
        complexity_regressions = _scan_dirty_complexity_pressure(
            relevant_paths,
            repo_root=repo_root,
            suffixes=resolved.languages.complexity,
            ccn_threshold=complexity_bounds.ccn_flex_limit,
            length_threshold=complexity_bounds.length_flex_limit,
            bounds_for_path=resolved.jittered_complexity_for_path,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("complexity", exc))

    try:
        magic_regressions = magicnums.detect_magic_regressions(
            relevant_paths,
            root=repo_root,
            baseline_ref=resolved.magic.baseline_ref,
            examine_threshold=resolved.magic.examine_threshold,
            examine_threshold_for_path=resolved.magic_examine_threshold_for_path,
            suffixes=resolved.languages.magic,
            c_grammar_suffixes=resolved.languages.c_grammar,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("magic-numbers", exc))

    return file_loc_findings, complexity_regressions, magic_regressions, errors


def _scan_dirty_complexity_pressure(
    paths: list[Path],
    *,
    repo_root: Path,
    suffixes: tuple[str, ...],
    ccn_threshold: int,
    length_threshold: int,
    bounds_for_path: Callable[[Path], ComplexityPolicy] | None = None,
) -> list[DirtyComplexityRegression]:
    current_paths = [path for path in paths if (repo_root / path).exists()]
    if not current_paths:
        return []
    current_records = complexity.collect_complexity_records(
        current_paths, root=repo_root, suffixes=suffixes
    )
    with tempfile.TemporaryDirectory(prefix="spice-complexity-baseline-") as temp_dir:
        temp_root = Path(temp_dir)
        baseline_paths = _materialize_complexity_baseline_paths(
            current_paths,
            repo_root=repo_root,
            temp_root=temp_root,
        )
        baseline_records = complexity.collect_complexity_records(
            baseline_paths,
            root=temp_root,
            suffixes=suffixes,
        )
    return _detect_dirty_complexity_regressions(
        current_records,
        baseline_records,
        ccn_threshold=ccn_threshold,
        length_threshold=length_threshold,
        bounds_for_path=bounds_for_path,
    )


def _materialize_complexity_baseline_paths(
    paths: list[Path], *, repo_root: Path, temp_root: Path
) -> list[Path]:
    materialized: list[Path] = []
    for path in paths:
        result = subprocess.run(
            [
                "git",
                "-C",
                str(repo_root),
                "show",
                f"{MAGIC_BASELINE_REF}:{path.as_posix()}",
            ],
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            continue
        target = temp_root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(result.stdout)
        materialized.append(path)
    return materialized


def _detect_dirty_complexity_regressions(
    current_records: list[complexity.ComplexityRecord],
    baseline_records: list[complexity.ComplexityRecord],
    *,
    ccn_threshold: int,
    length_threshold: int,
    bounds_for_path: Callable[[Path], ComplexityPolicy] | None = None,
) -> list[DirtyComplexityRegression]:
    baseline_index = _complexity_record_index(baseline_records)
    regressions: list[DirtyComplexityRegression] = []
    for record in sorted(
        current_records,
        key=lambda item: (item.path, item.function_name),
    ):
        baseline = baseline_index.get(record.key)
        bounds = bounds_for_path(Path(record.path)) if bounds_for_path else None
        active_ccn_threshold = (
            bounds.ccn_flex_limit if bounds is not None else ccn_threshold
        )
        active_length_threshold = (
            bounds.length_flex_limit if bounds is not None else length_threshold
        )
        ccn_unlimited = bounds.ccn_unlimited if bounds is not None else False
        length_unlimited = bounds.length_unlimited if bounds is not None else False
        if (
            not ccn_unlimited
            and record.ccn > active_ccn_threshold
            and (baseline is None or record.ccn > baseline.ccn)
        ):
            regressions.append(
                DirtyComplexityRegression(
                    path=record.path,
                    function_name=record.function_name,
                    metric="ccn",
                    value=record.ccn,
                    active_threshold=active_ccn_threshold,
                    baseline_value=baseline.ccn if baseline is not None else None,
                )
            )
        if not length_unlimited and record.length > active_length_threshold:
            regressions.append(
                DirtyComplexityRegression(
                    path=record.path,
                    function_name=record.function_name,
                    metric="length",
                    value=record.length,
                    active_threshold=active_length_threshold,
                    baseline_value=baseline.length if baseline is not None else None,
                )
            )
    return regressions


def _complexity_record_index(
    records: list[complexity.ComplexityRecord],
) -> dict[tuple[str, str], complexity.ComplexityRecord]:
    index: dict[tuple[str, str], complexity.ComplexityRecord] = {}
    for record in records:
        incumbent = index.get(record.key)
        if incumbent is None or (record.ccn, record.length) > (
            incumbent.ccn,
            incumbent.length,
        ):
            index[record.key] = record
    return index


def _dirty_pressure_summary(
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
) -> tuple[dict[str, set[str]], list[str]]:
    per_file_rules, file_loc_index, complexity_index, magic_index = (
        _index_dirty_pressure_rules(
            file_loc_findings,
            complexity_regressions,
            magic_regressions,
        )
    )
    ordered_summary = [
        f"{path} [{' ,'.join(sorted(labels))}]".replace(" ,", ",")
        for path, labels in sorted(
            per_file_rules.items(),
            key=lambda item: _dirty_pressure_severity_key(
                item[0],
                item[1],
                file_loc_index=file_loc_index,
                complexity_index=complexity_index,
                magic_index=magic_index,
            ),
        )
    ]
    return per_file_rules, ordered_summary


def _index_dirty_pressure_rules(
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
) -> tuple[
    dict[str, set[str]],
    dict[str, list[fileloc.LocFinding]],
    dict[str, list[DirtyComplexityRegression]],
    dict[str, list[magicnums.MagicFinding]],
]:
    per_file_rules: dict[str, set[str]] = {}
    file_loc_index: dict[str, list[fileloc.LocFinding]] = {}
    complexity_index: dict[str, list[DirtyComplexityRegression]] = {}
    magic_index: dict[str, list[magicnums.MagicFinding]] = {}

    def mark(path: str, label: str) -> None:
        per_file_rules.setdefault(path, set()).add(label)

    for finding in file_loc_findings:
        file_loc_index.setdefault(finding.path, []).append(finding)
        mark(finding.path, "file-loc")
    for regression in complexity_regressions:
        complexity_index.setdefault(regression.path, []).append(regression)
        mark(regression.path, f"complexity-{regression.metric}")
    for finding in magic_regressions:
        magic_index.setdefault(finding.path, []).append(finding)
        mark(finding.path, "magic")
    return per_file_rules, file_loc_index, complexity_index, magic_index


def _dirty_pressure_severity_key(
    path: str,
    labels: set[str],
    *,
    file_loc_index: dict[str, list[fileloc.LocFinding]],
    complexity_index: dict[str, list[DirtyComplexityRegression]],
    magic_index: dict[str, list[magicnums.MagicFinding]],
) -> tuple[object, ...]:
    loc_findings = file_loc_index.get(path, [])
    complexity_findings = complexity_index.get(path, [])
    magic_findings = magic_index.get(path, [])
    max_line_over = max(
        (
            max(0, finding.line_count - finding.line_limit)
            for finding in loc_findings
            if finding.over_line_limit
        ),
        default=0,
    )
    max_byte_over = max(
        (
            max(0, finding.byte_count - finding.byte_limit)
            for finding in loc_findings
            if finding.over_byte_limit
        ),
        default=0,
    )
    max_complexity_over = max(
        (
            max(0, regression.value - regression.active_threshold)
            for regression in complexity_findings
        ),
        default=0,
    )
    max_magic_value = max(
        (_magic_literal_abs(finding.literal) for finding in magic_findings),
        default=0.0,
    )
    return (
        -len(loc_findings),
        -len(labels),
        -max_line_over,
        -len(complexity_findings),
        -max_complexity_over,
        -len(magic_findings),
        -max_magic_value,
        -max_byte_over,
        path,
    )


def _dirty_pressure_severity(
    *,
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
    errors: list[str],
) -> str:
    if errors:
        return "unknown"
    if file_loc_findings or complexity_regressions:
        return "high"
    if magic_regressions:
        return "medium"
    return "none"


def _dirty_path_ages(paths: list[Path], *, repo_root: Path) -> DirtyWorktreePressure:
    now = time.time()
    rows: list[tuple[float, str]] = []
    for path in paths:
        try:
            mtime = (repo_root / path).stat().st_mtime
        except OSError:
            continue
        rows.append((max(0.0, now - mtime), path.as_posix()))
    if not rows:
        return {}
    oldest_age, oldest_path = max(rows, key=lambda row: (row[0], row[1]))
    newest_age, newest_path = min(rows, key=lambda row: (row[0], row[1]))
    return {
        "oldestDirtyAgeSeconds": int(oldest_age),
        "oldestDirtyPath": oldest_path,
        "newestDirtyAgeSeconds": int(newest_age),
        "newestDirtyPath": newest_path,
    }


def _dirty_pressure_lines(pressure: DirtyWorktreePressure) -> list[str]:
    if not pressure.get("available"):
        return ["  pressure=unavailable"]
    dirty_paths = int(pressure.get("dirtyPathCount") or 0)
    scanned_paths = int(pressure.get("scannedPathCount") or 0)
    lines = [
        "  pressure "
        f"severity={pressure.get('severity') or 'unknown'} "
        f"findings={int(pressure.get('totalFindings') or 0)} "
        f"files={int(pressure.get('fileCountWithPressure') or 0)} "
        f"scanned={scanned_paths}/{dirty_paths} "
        f"file-loc={int(pressure.get('fileLocFindingCount') or 0)} "
        f"complexity={int(pressure.get('complexityRegressionCount') or 0)} "
        f"magic-numbers={int(pressure.get('magicRegressionCount') or 0)}"
    ]
    age_line = _dirty_pressure_age_line(pressure)
    if age_line:
        lines.append(f"  {age_line}")
    errors = pressure.get("errors") or []
    lines.extend(f"  pressure_error={error}" for error in errors if error)
    summary_rows = pressure.get("summary") or []
    lines.extend(
        f"  pressure_file={summary}"
        for summary in summary_rows[:3]
        if isinstance(summary, str)
    )
    overflow = int(pressure.get("summaryOverflow") or 0)
    if overflow:
        lines.append(
            f"  pressure_more={overflow} additional dirty files carry findings"
        )
    return lines


def _dirty_pressure_age_line(pressure: DirtyWorktreePressure) -> str | None:
    oldest = pressure.get("oldestDirtyAgeSeconds")
    newest = pressure.get("newestDirtyAgeSeconds")
    if oldest is None or newest is None:
        return None
    return (
        "dirty_age="
        f"oldest={pressure.get('oldestDirtyPath') or '-'}:"
        f"{_format_dirty_age(oldest)} "
        f"newest={pressure.get('newestDirtyPath') or '-'}:"
        f"{_format_dirty_age(newest)}"
    )


def _format_dirty_age(raw_seconds: int | float) -> str:
    seconds = float(raw_seconds)
    return format_relative_seconds(seconds).removesuffix(" ago")


def _dirty_pressure_error(label: str, exc: BaseException) -> str:
    return f"{label}: {_clip(str(exc), 120)}"


def _magic_literal_abs(literal: str) -> float:
    try:
        return abs(float(literal.replace("_", "")))
    except ValueError:
        return 0.0


def _git_read(repo_root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
        text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else ""


def _git_read_z(repo_root: Path, *args: str) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return []
    raw = result.stdout.decode("utf-8", errors="surrogateescape")
    return [part for part in raw.split("\0") if part]
