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
    claim_flex_slice_paths,
    flex_limit,
    git_state_path,
    load_sticky_items,
    render_flex_slice_claim_redirect,
    save_sticky_items,
    sticky_function_keys_after_renames,
    sticky_items_after_flex_breaches,
)
from spice.policy import (
    COMPLEXITY_HOTSPOT_LIMIT,
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    COMPLEXITY_SUFFIXES,
)
from spice.studies.walk import is_excluded_path, staged_renames

COMPLEXITY_VERSION = 1
COMPLEXITY_PROCESS_TIMEOUT_SECONDS = 30.0
COMPLEXITY_CCN_STICKY_GIT_PATH = "complexity-ccn-sticky.json"
COMPLEXITY_LENGTH_STICKY_GIT_PATH = "complexity-length-sticky.json"

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
class _DefaultComplexityBounds:
    max_ccn: int
    ccn_flex_limit: int
    max_length: int
    length_flex_limit: int
    ccn_unlimited: bool = False
    length_unlimited: bool = False


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


def _load_sticky(root: Path, git_path: str) -> set[tuple[str, str]]:
    def decode(raw: object) -> tuple[str, str] | None:
        if (
            isinstance(raw, list)
            and len(raw) == 2
            and all(isinstance(item, str) for item in raw)
        ):
            return (raw[0], raw[1])
        return None

    return load_sticky_items(
        root=root,
        state_path=None,
        git_path=git_path,
        entries_key="functions",
        decode=decode,
        version=COMPLEXITY_VERSION,
    )


def _save_sticky(keys: set[tuple[str, str]], root: Path, git_path: str) -> None:
    save_sticky_items(
        keys,
        root=root,
        state_path=None,
        git_path=git_path,
        entries_key="functions",
        encode=list,
        version=COMPLEXITY_VERSION,
    )


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
    renames = staged_renames(root)
    loaded_ccn_sticky = sticky_function_keys_after_renames(
        _load_sticky(root, COMPLEXITY_CCN_STICKY_GIT_PATH), renames
    )
    loaded_length_sticky = sticky_function_keys_after_renames(
        _load_sticky(root, COMPLEXITY_LENGTH_STICKY_GIT_PATH), renames
    )
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
    default_bounds = _DefaultComplexityBounds(
        max_ccn=max_ccn,
        ccn_flex_limit=ccn_flex,
        max_length=max_length,
        length_flex_limit=length_flex,
    )
    resolve_bounds = bounds_for_path or (lambda _path: default_bounds)
    ccn_breaches, length_breaches = _complexity_breach_sets(records, resolve_bounds)
    ccn_sticky = _retained_over_base_sticky(
        loaded_ccn_sticky,
        root=root,
        attribute="ccn",
        fallback_limit=max_ccn,
        resolve_bounds=resolve_bounds,
        suffixes=suffixes,
    )
    length_sticky = _retained_over_base_sticky(
        loaded_length_sticky,
        root=root,
        attribute="length",
        fallback_limit=max_length,
        resolve_bounds=resolve_bounds,
        suffixes=suffixes,
    )
    updated_ccn_sticky = sticky_items_after_flex_breaches(
        records,
        ccn_sticky,
        key_for_item=lambda record: record.key,
        is_breach=lambda record: record in ccn_breaches,
    )
    updated_length_sticky = sticky_items_after_flex_breaches(
        records,
        length_sticky,
        key_for_item=lambda record: record.key,
        is_breach=lambda record: record in length_breaches,
    )
    if persist:
        if updated_ccn_sticky != loaded_ccn_sticky:
            _persist_sticky(updated_ccn_sticky, root, COMPLEXITY_CCN_STICKY_GIT_PATH)
        if updated_length_sticky != loaded_length_sticky:
            _persist_sticky(
                updated_length_sticky, root, COMPLEXITY_LENGTH_STICKY_GIT_PATH
            )
    peer_claims = _peer_flex_slice_claims(
        {Path(record.path) for record in ccn_breaches | length_breaches},
        root=root,
        actor=flex_actor,
        renames=renames,
        now=flex_claim_now,
    )
    return _complexity_findings(
        records,
        resolve_bounds=resolve_bounds,
        updated_ccn_sticky=updated_ccn_sticky,
        updated_length_sticky=updated_length_sticky,
        peer_claims=peer_claims,
    )


def _complexity_breach_sets(
    records: list[ComplexityRecord],
    resolve_bounds: Callable[[Path], ComplexityBounds],
) -> tuple[set[ComplexityRecord], set[ComplexityRecord]]:
    ccn_breaches = {
        record
        for record in records
        if (
            not resolve_bounds(Path(record.path)).ccn_unlimited
            and record.ccn > resolve_bounds(Path(record.path)).ccn_flex_limit
        )
    }
    length_breaches = {
        record
        for record in records
        if (
            not resolve_bounds(Path(record.path)).length_unlimited
            and record.length > resolve_bounds(Path(record.path)).length_flex_limit
        )
    }
    return ccn_breaches, length_breaches


def _peer_flex_slice_claims(
    paths: set[Path],
    *,
    root: Path,
    actor: str,
    renames: dict[Path, Path],
    now: float | None,
) -> dict[Path, FlexSliceClaim]:
    claim_decisions = claim_flex_slice_paths(
        paths,
        root=root,
        actor=actor,
        renames=renames,
        now=now,
    )
    return {
        path: decision.claim
        for path, decision in claim_decisions.items()
        if decision.peer_held
    }


def _complexity_findings(
    records: list[ComplexityRecord],
    *,
    resolve_bounds: Callable[[Path], ComplexityBounds],
    updated_ccn_sticky: set[tuple[str, str]],
    updated_length_sticky: set[tuple[str, str]],
    peer_claims: dict[Path, FlexSliceClaim],
) -> list[ComplexityFinding]:
    findings: list[ComplexityFinding] = []
    for record in records:
        bounds = resolve_bounds(Path(record.path))
        if bounds.ccn_unlimited and bounds.length_unlimited:
            continue
        ccn_limit = (
            bounds.max_ccn
            if record.key in updated_ccn_sticky
            else bounds.ccn_flex_limit
        )
        length_limit = (
            bounds.max_length
            if record.key in updated_length_sticky
            else bounds.length_flex_limit
        )
        over_ccn = False if bounds.ccn_unlimited else record.ccn > ccn_limit
        over_length = False if bounds.length_unlimited else record.length > length_limit
        if over_ccn or over_length:
            findings.append(
                ComplexityFinding(
                    record=record,
                    over_ccn=over_ccn,
                    over_length=over_length,
                    ccn_limit=ccn_limit,
                    length_limit=length_limit,
                    flex_slice_claim=peer_claims.get(Path(record.path)),
                )
            )
    return findings


def _retained_over_base_sticky(
    sticky: set[tuple[str, str]],
    *,
    root: Path,
    attribute: str,
    fallback_limit: int,
    resolve_bounds: Callable[[Path], ComplexityBounds],
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
        and _complexity_bound_is_retained(
            by_key[key],
            attribute=attribute,
            fallback_limit=fallback_limit,
            bounds=resolve_bounds(Path(by_key[key].path)),
        )
    }


def _persist_sticky(keys: set[tuple[str, str]], root: Path, git_path: str) -> None:
    """Write the latch set, or delete the state file once nothing stays latched."""
    if keys:
        _save_sticky(keys, root, git_path)
        return
    state_path = git_state_path(git_path, root=root)
    if state_path.exists():
        state_path.unlink()


def _complexity_bound_is_retained(
    record: ComplexityRecord,
    *,
    attribute: str,
    fallback_limit: int,
    bounds: ComplexityBounds,
) -> bool:
    if attribute == "ccn":
        return not bounds.ccn_unlimited and record.ccn > bounds.max_ccn
    if attribute == "length":
        return not bounds.length_unlimited and record.length > bounds.max_length
    return getattr(record, attribute) > fallback_limit


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
