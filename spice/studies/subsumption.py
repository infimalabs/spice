"""Coverage-containment candidates for bounded test-suite review.

Line and branch-arc containment is candidate evidence only. It cannot establish
that assertions, inputs, side effects, or contractual intent are redundant.
Equal-feature tests are grouped with one deterministic representative retained.

Record and scan checkout-safe coverage with declared development dependencies:

    uv sync --group dev
    uv run spice study subsumption --record --package spice

The recording workflow uses an explicit disposable coverage path by default.
Pass ``--retain-coverage PATH`` only when the SQLite artifact should survive.
"""

from __future__ import annotations

import hashlib
import os
import sqlite3
import sys
import tempfile
import xml.etree.ElementTree as ET
from collections import defaultdict
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Sequence

from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection
from spice.toolprocess import run_tool_command

COHORT_ID_HEX_LENGTH = 12


@dataclass(frozen=True)
class SubsumptionFinding:
    test: str
    covered_lines: int
    covered_arcs: int
    covered_features: int
    subsumed_by: str
    relation: str
    cohort_id: str


@dataclass(frozen=True)
class SubsumptionCohort:
    cohort_id: str
    relation: str
    representative: str
    candidates: tuple[str, ...]


@dataclass(frozen=True)
class SubsumptionReport:
    findings: tuple[SubsumptionFinding, ...]
    cohorts: tuple[SubsumptionCohort, ...]
    tests_scanned: int
    source_files_scanned: int
    coverage_contexts: int
    excluded_test_contexts: int
    context_free_contexts: int
    suite_tests: int | None = None
    coverage_artifact: str | None = None


def record_subsumption(
    root: Path,
    *,
    package: str,
    package_prefix: str | None = None,
    coverage_output: Path | None = None,
    pytest_args: Sequence[str] = (),
) -> SubsumptionReport:
    """Record branch-aware per-test coverage, scan it, and clean temp artifacts."""
    root = root.resolve()
    retained_path = _resolve_retained_path(root, coverage_output)
    with tempfile.TemporaryDirectory(prefix="spice-subsumption-") as temp_name:
        temp_root = Path(temp_name)
        coverage_path = retained_path or temp_root / "coverage.db"
        junit_path = temp_root / "pytest.xml"
        coverage_config = temp_root / "coveragerc"
        coverage_config.write_text("[run]\nbranch = True\n", encoding="utf-8")
        command = (
            sys.executable,
            "-B",
            "-m",
            "pytest",
            "-p",
            "no:cacheprovider",
            f"--basetemp={temp_root / 'pytest'}",
            f"--junitxml={junit_path}",
            f"--cov={package}",
            "--cov-context=test",
            "--cov-branch",
            "--cov-report=",
            f"--cov-config={coverage_config}",
            *pytest_args,
        )
        env = os.environ.copy()  # env-policy: allow - preserve pytest environment
        env["COVERAGE_FILE"] = str(coverage_path)
        completed = run_tool_command(
            list(command),
            policy="coverage",
            operation="record subsumption coverage",
            cwd=root,
            env=env,
            capture_output=False,
            check=False,
        )
        if completed.returncode != 0:
            raise SpiceError(
                "subsumption coverage run failed with exit "
                f"{completed.returncode}: {' '.join(command)}"
            )
        if not coverage_path.is_file():
            raise SpiceError(
                f"subsumption coverage run produced no database at {coverage_path}"
            )
        suite_tests = _junit_test_count(junit_path)
        report = scan_subsumption(coverage_path, package_prefix=package_prefix)
        return replace(
            report,
            suite_tests=suite_tests,
            coverage_artifact=str(retained_path) if retained_path else None,
        )


def _resolve_retained_path(root: Path, output: Path | None) -> Path | None:
    if output is None:
        return None
    path = output if output.is_absolute() else root / output
    path = path.resolve()
    if path.exists():
        raise SpiceError(f"refusing to overwrite retained coverage artifact: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def _junit_test_count(path: Path) -> int:
    root = ET.parse(path).getroot()
    if "tests" in root.attrib:
        return int(root.attrib["tests"])
    return sum(
        int(suite.attrib.get("tests", "0")) for suite in root.findall("testsuite")
    )


def scan_subsumption(
    coverage_path: Path,
    *,
    package_prefix: str | None = None,
) -> SubsumptionReport:
    """Read a .coverage SQLite file and return subsumption findings."""
    if not coverage_path.is_file():
        raise FileNotFoundError(
            f"coverage file not found: {coverage_path}; "
            "generate with: pytest --cov=<package> --cov-context=test --cov-branch"
        )

    with sqlite_connection(coverage_path) as con:
        test_coverage = _read_coverage_db(con, package_prefix=package_prefix)
        test_arcs = _load_per_test_arcs(con, package_prefix=package_prefix)
        raw_contexts = _coverage_context_names(con)

    findings = _find_subsumed(test_coverage, test_arcs)
    cohorts = _cohorts(findings)

    # Both test_coverage and test_arcs are already package-prefix filtered.
    all_files: set[str] = set()
    for covered in test_coverage.values():
        all_files.update(covered.keys())
    for arc_set in test_arcs.values():
        all_files.update(file_path for file_path, _from, _to in arc_set)

    all_test_ids = set(test_coverage.keys()) | test_arcs.keys()
    normalized_contexts = {
        normalized
        for context in raw_contexts
        if (normalized := _normalize_context(context))
    }
    context_free_contexts = sum(
        1 for context in raw_contexts if not _normalize_context(context)
    )

    return SubsumptionReport(
        findings=tuple(findings),
        cohorts=tuple(cohorts),
        tests_scanned=len(all_test_ids),
        source_files_scanned=len(all_files),
        coverage_contexts=len(normalized_contexts),
        excluded_test_contexts=len(normalized_contexts - all_test_ids),
        context_free_contexts=context_free_contexts,
    )


def render_subsumption_board(
    report: SubsumptionReport, *, limit: int | None = None
) -> list[str]:
    """Render a text board of subsumption findings."""
    rows: list[str] = []
    suite = str(report.suite_tests) if report.suite_tests is not None else "unknown"
    excluded_suite = (
        max(report.suite_tests - report.tests_scanned, 0)
        if report.suite_tests is not None
        else None
    )
    rows.append(
        f"subsumption: suite={suite} analyzed_contexts={report.tests_scanned}"
        f" production_files={report.source_files_scanned}"
    )
    rows.append(
        "  denominator: "
        f"coverage_contexts={report.coverage_contexts} "
        f"excluded_contexts={report.excluded_test_contexts} "
        f"context_free={report.context_free_contexts} "
        f"suite_without_analyzed_context={excluded_suite if excluded_suite is not None else 'unknown'}"
    )
    rows.append(
        "  review candidates only: coverage containment does not prove redundant "
        "assertions, inputs, side effects, or intent"
    )
    if report.coverage_artifact:
        rows.append(f"  retained coverage: {report.coverage_artifact}")
    if not report.findings:
        rows.append("  no coverage-containment candidates found")
        return rows
    rows.append(
        f"  {len(report.findings)} candidate(s) in {len(report.cohorts)} "
        "deterministic cohort(s):"
    )
    shown = 0
    for cohort in report.cohorts:
        rows.append(
            f"  cohort {cohort.cohort_id} relation={cohort.relation} "
            f"representative={cohort.representative} count={len(cohort.candidates)}"
        )
        cohort_findings = [
            finding
            for finding in report.findings
            if finding.cohort_id == cohort.cohort_id
        ]
        for finding in cohort_findings:
            if limit is not None and shown >= limit:
                break
            rows.append(
                f"    {finding.test} lines={finding.covered_lines} "
                f"arcs={finding.covered_arcs} features={finding.covered_features} "
                f"contained_by={finding.subsumed_by}"
            )
            shown += 1
        if limit is not None and shown >= limit:
            break
    omitted = len(report.findings) - shown
    if omitted:
        rows.append(f"  bounded output: {omitted} additional candidate(s) omitted")
    return rows


def _coverage_context_names(con: sqlite3.Connection) -> tuple[str, ...]:
    return tuple(
        str(row[0])
        for row in con.execute("SELECT DISTINCT context FROM context ORDER BY context")
    )


def _load_per_test_arcs(
    con: sqlite3.Connection,
    *,
    package_prefix: str | None,
) -> dict[str, frozenset[tuple[str, int, int]]]:
    """Return {test_id: frozenset((file, fromno, tono))} if arc table exists."""
    tables = {
        r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    if "arc" not in tables:
        return {}
    result: dict[str, list[tuple[str, int, int]]] = {}
    rows = con.execute(
        "SELECT f.path, c.context, a.fromno, a.tono "
        "FROM arc a "
        "JOIN file f ON a.file_id = f.id "
        "JOIN context c ON a.context_id = c.id "
        "WHERE c.context != ''"
    ).fetchall()
    for file_path, context, fromno, tono in rows:
        if package_prefix and package_prefix not in file_path:
            continue
        test_id = _normalize_context(context)
        if not test_id:
            continue
        result.setdefault(test_id, [])
        result[test_id].append((file_path, fromno, tono))
    return {k: frozenset(v) for k, v in result.items()}


def _read_coverage_db(
    con: sqlite3.Connection,
    *,
    package_prefix: str | None,
) -> dict[str, dict[str, frozenset[int]]]:
    tables = {
        r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    if "line_bits" in tables:
        return _read_v7_schema(con, package_prefix=package_prefix)
    if "lines" in tables:
        return _read_v6_schema(con, package_prefix=package_prefix)
    raise ValueError(
        "unrecognised .coverage schema; regenerate with a supported coverage.py version"
    )


def _read_v7_schema(
    con: sqlite3.Connection,
    *,
    package_prefix: str | None,
) -> dict[str, dict[str, frozenset[int]]]:
    result: dict[str, dict[str, frozenset[int]]] = {}
    rows = con.execute(
        "SELECT f.path, c.context, l.numbits "
        "FROM line_bits l "
        "JOIN file f ON l.file_id = f.id "
        "JOIN context c ON l.context_id = c.id "
        "WHERE c.context != ''"
    ).fetchall()
    for file_path, context, numbits in rows:
        if package_prefix and not file_path.endswith(
            tuple(_py_suffixes(package_prefix))
        ):
            if package_prefix not in file_path:
                continue
        test_id = _normalize_context(context)
        if not test_id:
            continue
        lines = frozenset(_decode_numbits(numbits))
        result.setdefault(test_id, {})
        existing = result[test_id].get(file_path, frozenset())
        result[test_id][file_path] = existing | lines
    return result


def _read_v6_schema(
    con: sqlite3.Connection,
    *,
    package_prefix: str | None,
) -> dict[str, dict[str, frozenset[int]]]:
    result: dict[str, dict[str, frozenset[int]]] = {}
    rows = con.execute(
        "SELECT f.path, c.context, l.lineno "
        "FROM lines l "
        "JOIN file f ON l.file_id = f.id "
        "JOIN context c ON l.context_id = c.id "
        "WHERE c.context != ''"
    ).fetchall()
    for file_path, context, lineno in rows:
        if package_prefix and package_prefix not in file_path:
            continue
        test_id = _normalize_context(context)
        if not test_id:
            continue
        result.setdefault(test_id, {})
        existing = result[test_id].get(file_path, frozenset())
        result[test_id][file_path] = existing | {lineno}
    return result


def _find_subsumed(
    test_coverage: dict[str, dict[str, frozenset[int]]],
    test_arcs: dict[str, frozenset[tuple[str, int, int]]] | None = None,
) -> list[SubsumptionFinding]:
    # Build per-test feature sets: line points + arc points for subsumption check.
    # Including arcs prevents false positives when two tests cover the same lines
    # but distinct branches.  Arc-only tests (present in test_arcs but not in
    # test_coverage) are fully included so branch-only databases are handled.
    feature_sets: dict[str, frozenset] = {}
    line_counts: dict[str, int] = {}
    arc_counts: dict[str, int] = {}
    all_ids = set(test_coverage.keys())
    if test_arcs:
        all_ids |= test_arcs.keys()
    for test_id in all_ids:
        file_map = test_coverage.get(test_id, {})
        line_pts: set[tuple] = set()
        for file_path, lines in file_map.items():
            line_pts.update(("l", file_path, ln) for ln in lines)
        line_counts[test_id] = len(line_pts)
        arc_counts[test_id] = len((test_arcs or {}).get(test_id, frozenset()))
        test_features: set[tuple] = set(line_pts)
        if test_arcs:
            for arc in test_arcs.get(test_id, frozenset()):
                test_features.add(("a",) + arc)
        feature_sets[test_id] = frozenset(test_features)

    feature_groups: dict[frozenset, list[str]] = defaultdict(list)
    for test_id, features in feature_sets.items():
        if features:
            feature_groups[features].append(test_id)
    groups = sorted(
        (
            (features, tuple(sorted(members)))
            for features, members in feature_groups.items()
        ),
        key=lambda item: item[1][0],
    )

    findings: list[SubsumptionFinding] = []
    for _features, members in groups:
        if len(members) < 2:
            continue
        representative = members[0]
        cohort_id = _stable_cohort_id("equal-feature", representative)
        for test_id in members[1:]:
            findings.append(
                SubsumptionFinding(
                    test=test_id,
                    covered_lines=line_counts[test_id],
                    covered_arcs=arc_counts[test_id],
                    covered_features=len(feature_sets[test_id]),
                    subsumed_by=representative,
                    relation="equal-feature",
                    cohort_id=cohort_id,
                )
            )

    # Preserve the deterministic representative of every equal-feature class.
    # Strict containment candidates are emitted only for singleton classes.
    for features, members in groups:
        if len(members) != 1:
            continue
        test_id = members[0]
        supersets = [
            (len(other_features) - len(features), other_members[0])
            for other_features, other_members in groups
            if features < other_features
        ]
        if not supersets:
            continue
        _distance, representative = min(supersets)
        findings.append(
            SubsumptionFinding(
                test=test_id,
                covered_lines=line_counts[test_id],
                covered_arcs=arc_counts[test_id],
                covered_features=len(feature_sets[test_id]),
                subsumed_by=representative,
                relation="strict-subset",
                cohort_id=_stable_cohort_id("strict-subset", representative),
            )
        )
    return sorted(findings, key=lambda finding: (finding.cohort_id, finding.test))


def _stable_cohort_id(relation: str, representative: str) -> str:
    digest = hashlib.sha256(f"{relation}\0{representative}".encode()).hexdigest()[
        :COHORT_ID_HEX_LENGTH
    ]
    return f"{relation}-{digest}"


def _cohorts(findings: list[SubsumptionFinding]) -> list[SubsumptionCohort]:
    grouped: dict[tuple[str, str, str], list[str]] = defaultdict(list)
    for finding in findings:
        grouped[(finding.cohort_id, finding.relation, finding.subsumed_by)].append(
            finding.test
        )
    cohorts = [
        SubsumptionCohort(
            cohort_id=cohort_id,
            relation=relation,
            representative=representative,
            candidates=tuple(sorted(candidates)),
        )
        for (cohort_id, relation, representative), candidates in grouped.items()
    ]
    return sorted(
        cohorts,
        key=lambda cohort: (
            -len(cohort.candidates),
            cohort.relation,
            cohort.representative,
        ),
    )


def _normalize_context(context: str) -> str:
    if not context or context.startswith("|"):
        return ""
    return context.split("|")[0].strip()


def _decode_numbits(numbits: bytes) -> list[int]:
    lines: list[int] = []
    for byte_index, byte_val in enumerate(numbits):
        for bit in range(8):
            if byte_val & (1 << bit):
                lines.append(byte_index * 8 + bit + 1)
    return lines


def _py_suffixes(prefix: str) -> list[str]:
    return [f"/{prefix}/", f"\\{prefix}\\"]
