"""Routine complexity pressure: CCN and length via lizard, flex + sticky.

Same regime as file shape: a routine may reach the flex limit, but one that
ever breached stays held to the base limit (per `(path, routine)` key) until
it shrinks back under. lizard is the single measurement backend; its absence
fails the gate loudly rather than miscounting.

"""

from __future__ import annotations

import csv
import io
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Protocol

from spice.errors import SpiceError
from spice.paths import find_tool
from spice.process.groups import run_bounded_process_group
from spice.flexstate import (
    FlexSliceClaim,
    flex_limit,
    render_flex_slice_claim_redirect,
)
from spice.policy import (
    COMPLEXITY_HOTSPOT_LIMIT,
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    COMPLEXITY_SUFFIXES,
)
from spice.studies import gates
from spice.studies.walk import is_excluded_path

COMPLEXITY_VERSION = 1
COMPLEXITY_PROCESS_TIMEOUT_SECONDS = 30.0
COMPLEXITY_CCN_STICKY_GIT_PATH = "complexity-ccn-sticky.json"
COMPLEXITY_LENGTH_STICKY_GIT_PATH = "complexity-length-sticky.json"
_CCN_STICKY_LEDGER = gates.function_sticky_ledger(
    COMPLEXITY_CCN_STICKY_GIT_PATH,
    version=COMPLEXITY_VERSION,
)
_LENGTH_STICKY_LEDGER = gates.function_sticky_ledger(
    COMPLEXITY_LENGTH_STICKY_GIT_PATH,
    version=COMPLEXITY_VERSION,
)

# lizard --csv columns: nloc, ccn, token_count, param_count, length,
# location, path, function_name, ...
LIZARD_CSV_NLOC = 0
LIZARD_CSV_CCN = 1
LIZARD_CSV_LENGTH = 4
LIZARD_CSV_LOCATION = 5
LIZARD_CSV_PATH = 6
LIZARD_CSV_NAME = 7
LIZARD_CSV_MIN_COLUMNS = 7


@dataclass(frozen=True)
class ComplexityRecord:
    path: str
    function_name: str
    ccn: int
    length: int
    nloc: int

    @property
    def key(self) -> tuple[str, str]:
        return (self.path, self.function_name)


@dataclass(frozen=True)
class ComplexityFinding:
    record: ComplexityRecord
    over_ccn: bool
    over_length: bool
    ccn_limit: int
    length_limit: int
    flex_slice_claim: FlexSliceClaim | None = None


def complexity_hotspot_rows(
    records: list[ComplexityRecord], *, limit: int = COMPLEXITY_HOTSPOT_LIMIT
) -> list[ComplexityRecord]:
    return sorted(
        records,
        key=lambda record: (
            record.ccn,
            record.length,
            record.nloc,
            record.path,
            record.function_name,
        ),
        reverse=True,
    )[:limit]


def render_complexity_hotspots(
    records: list[ComplexityRecord], *, limit: int = COMPLEXITY_HOTSPOT_LIMIT
) -> str:
    hotspots = complexity_hotspot_rows(records, limit=limit)
    if not hotspots:
        return "complexity-hotspots: no routines found"
    lines = [
        f"complexity-hotspots: top {len(hotspots)} of {len(records)} routine(s)",
        "ccn  len  nloc  location",
    ]
    for record in hotspots:
        lines.append(
            f"{record.ccn:>3}  {record.length:>3}  {record.nloc:>4}  "
            f"{record.path}:{record.function_name}"
        )
    return "\n".join(lines)


class ComplexityBounds(Protocol):
    @property
    def max_ccn(self) -> int: ...

    @property
    def ccn_flex_limit(self) -> int: ...

    @property
    def max_length(self) -> int: ...

    @property
    def length_flex_limit(self) -> int: ...

    @property
    def ccn_unlimited(self) -> bool: ...

    @property
    def length_unlimited(self) -> bool: ...


@dataclass(frozen=True)
class _ComplexityBoundSet:
    ccn: gates.BoundedValue
    length: gates.BoundedValue


def require_lizard() -> str:
    located = find_tool("lizard")
    if not located:
        raise SpiceError(
            "lizard is required for the complexity gate; it installs with "
            "spice, so the installation is broken or incomplete"
        )
    return located


def collect_complexity_records(
    paths: list[Path],
    *,
    root: Path,
    suffixes: tuple[str, ...] = COMPLEXITY_SUFFIXES,
    timeout_seconds: float = COMPLEXITY_PROCESS_TIMEOUT_SECONDS,
    phase: str = "complexity-collection",
    input_label: str | None = None,
) -> list[ComplexityRecord]:
    targets = [
        path
        for path in paths
        if path.suffix in suffixes
        and not is_excluded_path(path, repo_root=root)
        and (root / path).exists()
    ]
    if not targets:
        return []
    lizard = require_lizard()
    result = run_bounded_process_group(
        [lizard, "--csv", *[str(root / path) for path in targets]],
        timeout_seconds=timeout_seconds,
        phase=phase,
        input_label=input_label or _complexity_input_label(root, targets),
        text=True,
        cwd=root,
    )
    records: list[ComplexityRecord] = []
    for row in csv.reader(io.StringIO(result.stdout)):
        if len(row) < LIZARD_CSV_MIN_COLUMNS:
            continue
        try:
            nloc, ccn = int(row[LIZARD_CSV_NLOC]), int(row[LIZARD_CSV_CCN])
            length = int(row[LIZARD_CSV_LENGTH])
        except ValueError:
            continue
        raw_path = row[LIZARD_CSV_PATH].strip().strip('"')
        try:
            rel_path = Path(raw_path).resolve().relative_to(root.resolve()).as_posix()
        except ValueError:
            rel_path = raw_path
        function_name = (
            row[LIZARD_CSV_NAME].strip().strip('"')
            if len(row) > LIZARD_CSV_NAME
            else row[LIZARD_CSV_LOCATION]
        )
        records.append(
            ComplexityRecord(
                path=rel_path,
                function_name=function_name,
                ccn=ccn,
                length=length,
                nloc=nloc,
            )
        )
    return records


def _complexity_input_label(root: Path, paths: list[Path]) -> str:
    rendered_paths = ",".join(path.as_posix() for path in paths)
    return f"repository={root} paths={rendered_paths}"


def scan_staged_complexity_violations(
    paths: list[Path],
    *,
    root: Path,
    max_ccn: int = COMPLEXITY_MAX_CCN,
    max_length: int = COMPLEXITY_MAX_LENGTH,
    ccn_flex_limit_value: int | None = None,
    length_flex_limit_value: int | None = None,
    bounds_for_path: Callable[[Path], ComplexityBounds] | None = None,
    suffixes: tuple[str, ...] = COMPLEXITY_SUFFIXES,
    persist: bool = False,
    flex_actor: str = "",
    flex_claim_now: float | None = None,
) -> list[ComplexityFinding]:
    """Scan staged routines against the flex+sticky CCN/length limits.

    ``persist`` writes sticky state for the committing gate; ``flex_actor``
    separately records or honors live flex slice claims for files whose
    routines breach flex. Leave ``flex_actor`` empty for read-only scans.
    """
    records = collect_complexity_records(paths, root=root, suffixes=suffixes)
    renames = gates.staged_gate_renames(root)
    ccn_flex = (
        ccn_flex_limit_value
        if ccn_flex_limit_value is not None
        else flex_limit(max_ccn)
    )
    length_flex = (
        length_flex_limit_value
        if length_flex_limit_value is not None
        else flex_limit(max_length)
    )
    default_bounds = _ComplexityBoundSet(
        ccn=gates.BoundedValue.from_base(max_ccn, ccn_flex),
        length=gates.BoundedValue.from_base(max_length, length_flex),
    )

    def resolve_bounds(path: Path) -> _ComplexityBoundSet:
        return _resolved_complexity_bounds(
            path,
            bounds_for_path=bounds_for_path,
            default_bounds=default_bounds,
        )

    ccn_breaches, length_breaches = _complexity_breach_sets(records, resolve_bounds)
    ccn_state = gates.reconcile_sticky_latch(
        _CCN_STICKY_LEDGER,
        root=root,
        renames=renames,
        retain=lambda keys: _retained_complexity_sticky(
            keys,
            root=root,
            measure=lambda record: record.ccn,
            bounds_for_path=lambda path: resolve_bounds(path).ccn,
            suffixes=suffixes,
        ),
        breach_keys=(record.key for record in ccn_breaches),
        persist=persist,
    )
    length_state = gates.reconcile_sticky_latch(
        _LENGTH_STICKY_LEDGER,
        root=root,
        renames=renames,
        retain=lambda keys: _retained_complexity_sticky(
            keys,
            root=root,
            measure=lambda record: record.length,
            bounds_for_path=lambda path: resolve_bounds(path).length,
            suffixes=suffixes,
        ),
        breach_keys=(record.key for record in length_breaches),
        persist=persist,
    )
    peer_claims = gates.peer_flex_slice_claims(
        {Path(record.path) for record in ccn_breaches | length_breaches},
        root=root,
        actor=flex_actor,
        renames=renames,
        now=flex_claim_now,
    )
    return _complexity_findings(
        records,
        resolve_bounds=resolve_bounds,
        updated_ccn_sticky=ccn_state.updated,
        updated_length_sticky=length_state.updated,
        peer_claims=peer_claims,
    )


def _complexity_breach_sets(
    records: list[ComplexityRecord],
    resolve_bounds: Callable[[Path], _ComplexityBoundSet],
) -> tuple[set[ComplexityRecord], set[ComplexityRecord]]:
    ccn_breaches = {
        record
        for record in records
        if gates.bounded_disposition(
            record.ccn,
            resolve_bounds(Path(record.path)).ccn,
        ).flex_breach
    }
    length_breaches = {
        record
        for record in records
        if gates.bounded_disposition(
            record.length,
            resolve_bounds(Path(record.path)).length,
        ).flex_breach
    }
    return ccn_breaches, length_breaches


def _resolved_complexity_bounds(
    path: Path,
    *,
    bounds_for_path: Callable[[Path], ComplexityBounds] | None,
    default_bounds: _ComplexityBoundSet,
) -> _ComplexityBoundSet:
    if bounds_for_path is None:
        return default_bounds
    bounds = bounds_for_path(path)
    return _ComplexityBoundSet(
        ccn=gates.BoundedValue(
            base_limit=bounds.max_ccn,
            flex_limit=bounds.ccn_flex_limit,
            unlimited=bounds.ccn_unlimited,
        ),
        length=gates.BoundedValue(
            base_limit=bounds.max_length,
            flex_limit=bounds.length_flex_limit,
            unlimited=bounds.length_unlimited,
        ),
    )


def _complexity_findings(
    records: list[ComplexityRecord],
    *,
    resolve_bounds: Callable[[Path], _ComplexityBoundSet],
    updated_ccn_sticky: set[tuple[str, str]],
    updated_length_sticky: set[tuple[str, str]],
    peer_claims: dict[Path, FlexSliceClaim],
) -> list[ComplexityFinding]:
    findings: list[ComplexityFinding] = []
    for record in records:
        bounds = resolve_bounds(Path(record.path))
        ccn_disposition = gates.bounded_disposition(
            record.ccn,
            bounds.ccn,
            latched=record.key in updated_ccn_sticky,
        )
        length_disposition = gates.bounded_disposition(
            record.length,
            bounds.length,
            latched=record.key in updated_length_sticky,
        )
        if ccn_disposition.over_limit or length_disposition.over_limit:
            findings.append(
                ComplexityFinding(
                    record=record,
                    over_ccn=ccn_disposition.over_limit,
                    over_length=length_disposition.over_limit,
                    ccn_limit=ccn_disposition.limit,
                    length_limit=length_disposition.limit,
                    flex_slice_claim=peer_claims.get(Path(record.path)),
                )
            )
    return findings


def _retained_complexity_sticky(
    sticky: set[tuple[str, str]],
    *,
    root: Path,
    measure: Callable[[ComplexityRecord], int],
    bounds_for_path: Callable[[Path], gates.BoundedValue],
    suffixes: tuple[str, ...],
) -> set[tuple[str, str]]:
    """The still-latched subset: routines still over their base limit.

    A latch only records that a routine once breached flex; it is retired the
    moment the routine is back at or under its base limit. Evaluating that here
    on every scan — not only after a fully clean commit — is what lets a latch
    recorded in one (now-idle) worktree heal as soon as any scan sees the
    routine under base again.
    """
    if not sticky:
        return set()
    live_paths = sorted({Path(path) for path, _name in sticky})
    records = collect_complexity_records(live_paths, root=root, suffixes=suffixes)
    by_key = {record.key: record for record in records}
    return {
        key
        for key in sticky
        if key in by_key
        and gates.bounded_disposition(
            measure(by_key[key]),
            bounds_for_path(Path(by_key[key].path)),
        ).over_base
    }


def render_complexity_board(
    findings: list[ComplexityFinding],
    *,
    max_ccn: int = COMPLEXITY_MAX_CCN,
    max_length: int = COMPLEXITY_MAX_LENGTH,
) -> str:
    if not findings:
        return f"complexity: ok (ccn_limit {max_ccn} length_limit {max_length})"
    lines = [f"complexity: {len(findings)} violation(s)"]
    for finding in findings:
        reasons = []
        if finding.over_ccn:
            reasons.append(f"ccn {finding.record.ccn} > {finding.ccn_limit}")
        if finding.over_length:
            reasons.append(f"length {finding.record.length} > {finding.length_limit}")
        if finding.flex_slice_claim is not None:
            reasons.append(render_flex_slice_claim_redirect(finding.flex_slice_claim))
        lines.append(
            f"  FAIL  {finding.record.path}:{finding.record.function_name}: "
            f"{'; '.join(reasons)}"
        )
    if any(finding.flex_slice_claim is not None for finding in findings):
        lines.append(
            "  peer-held flex slices redirect duplicate refactors; keep changes "
            "append-only or move to another seam"
        )
    return "\n".join(lines)
