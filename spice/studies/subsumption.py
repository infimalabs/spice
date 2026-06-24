"""Coverage subsumption detector: find tests whose coverage is fully subsumed.

A test T is subsumed by T' if every source line (and branch arc) T covers is
also covered by T'.  Subsumed tests add zero unique behavioral constraint —
they are candidates for removal or consolidation unless they serve as
belt-and-suspenders on a critical path (which the study flags, not decides).

Requires a .coverage file recorded with per-test context. Generate one with:

    pytest --cov=<package> --cov-context=test --cov-branch

The --cov-branch flag records arc (branch) coverage so that two tests covering
the same lines but distinct branches are not incorrectly flagged as subsumed.

Library seam: public dataclasses and scan/render helpers are importable by
target-repo tools; underscored names remain private.
"""

from __future__ import annotations

import sqlite3
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class SubsumptionFinding:
    test: str
    covered_lines: int
    subsumed_by: str


@dataclass(frozen=True)
class SubsumptionReport:
    findings: tuple[SubsumptionFinding, ...]
    tests_scanned: int
    source_files_scanned: int


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

    con = sqlite3.connect(coverage_path)
    try:
        test_coverage = _read_coverage_db(con, package_prefix=package_prefix)
        test_arcs = _load_per_test_arcs(con, package_prefix=package_prefix)
    finally:
        con.close()

    findings = _find_subsumed(test_coverage, test_arcs)

    # Both test_coverage and test_arcs are already package-prefix filtered.
    all_files: set[str] = set()
    for covered in test_coverage.values():
        all_files.update(covered.keys())
    for arc_set in test_arcs.values():
        all_files.update(file_path for file_path, _from, _to in arc_set)

    all_test_ids = set(test_coverage.keys()) | test_arcs.keys()

    return SubsumptionReport(
        findings=tuple(findings),
        tests_scanned=len(all_test_ids),
        source_files_scanned=len(all_files),
    )


def render_subsumption_board(report: SubsumptionReport) -> list[str]:
    """Render a text board of subsumption findings."""
    rows: list[str] = []
    rows.append(
        f"subsumption: scanned {report.tests_scanned} tests"
        f" over {report.source_files_scanned} source files"
    )
    if not report.findings:
        rows.append("  no subsumed tests found")
        return rows
    rows.append(f"  {len(report.findings)} subsumed test(s):")
    for f in report.findings:
        rows.append(f"  {f.test}")
        rows.append(f"    lines: {f.covered_lines}  subsumed by: {f.subsumed_by}")
    return rows


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
    all_ids = set(test_coverage.keys())
    if test_arcs:
        all_ids |= test_arcs.keys()
    for test_id in all_ids:
        file_map = test_coverage.get(test_id, {})
        line_pts: set[tuple] = set()
        for file_path, lines in file_map.items():
            line_pts.update(("l", file_path, ln) for ln in lines)
        line_counts[test_id] = len(line_pts)
        features: set[tuple] = set(line_pts)
        if test_arcs:
            for arc in test_arcs.get(test_id, frozenset()):
                features.add(("a",) + arc)
        feature_sets[test_id] = frozenset(features)

    findings: list[SubsumptionFinding] = []
    test_ids = sorted(feature_sets)
    for test_a in test_ids:
        features_a = feature_sets[test_a]
        if not features_a:
            continue
        for test_b in test_ids:
            if test_b == test_a:
                continue
            if features_a <= feature_sets[test_b]:
                findings.append(
                    SubsumptionFinding(
                        test=test_a,
                        covered_lines=line_counts[test_a],
                        subsumed_by=test_b,
                    )
                )
                break
    return findings


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
