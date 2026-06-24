"""Coverage subsumption detector: find tests whose coverage is fully subsumed.

A test T is subsumed by T' if every source line T covers is also covered by T'.
Subsumed tests add zero unique behavioral constraint — they are candidates for
removal or consolidation unless they serve as belt-and-suspenders on a critical
path (which the study flags, not decides).

Requires a .coverage file recorded with per-test context. Generate one with:

    pytest --cov=<package> --cov-context=test

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
            "generate with: pytest --cov=<package> --cov-context=test"
        )

    test_coverage = _load_per_test_coverage(
        coverage_path, package_prefix=package_prefix
    )
    findings = _find_subsumed(test_coverage)
    all_files: set[str] = set()
    for covered in test_coverage.values():
        all_files.update(covered.keys())

    return SubsumptionReport(
        findings=tuple(findings),
        tests_scanned=len(test_coverage),
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


def _load_per_test_coverage(
    path: Path,
    *,
    package_prefix: str | None,
) -> dict[str, dict[str, frozenset[int]]]:
    """Return {test_id: {file: frozenset(line_numbers)}}."""
    con = sqlite3.connect(path)
    try:
        return _read_coverage_db(con, package_prefix=package_prefix)
    finally:
        con.close()


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
) -> list[SubsumptionFinding]:
    total_covered: dict[str, frozenset[tuple[str, int]]] = {}
    for test_id, file_map in test_coverage.items():
        merged: set[tuple[str, int]] = set()
        for file_path, lines in file_map.items():
            merged.update((file_path, ln) for ln in lines)
        total_covered[test_id] = frozenset(merged)

    findings: list[SubsumptionFinding] = []
    test_ids = sorted(total_covered)
    for i, test_a in enumerate(test_ids):
        lines_a = total_covered[test_a]
        if not lines_a:
            continue
        for test_b in test_ids:
            if test_b == test_a:
                continue
            lines_b = total_covered[test_b]
            if lines_a <= lines_b:
                findings.append(
                    SubsumptionFinding(
                        test=test_a,
                        covered_lines=len(lines_a),
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
