"""Routine complexity pressure: CCN and length via lizard, flex + sticky.

Same regime as file shape: a routine may reach the flex limit, but one that
ever breached stays held to the base limit (per `(path, routine)` key) until
it shrinks back under. lizard is the single measurement backend; its absence
fails the gate loudly rather than miscounting.

Library seam: target-repo tools may import the public finding/record
dataclasses, scan/collect helpers, lizard requirement helper, and
`render_complexity_board`; underscored names remain private.
"""

from __future__ import annotations

import csv
import io
import subprocess
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import find_tool
from spice.flexstate import (
    flex_limit,
    git_state_path,
    load_sticky_items,
    save_sticky_items,
    sticky_function_keys_after_renames,
    sticky_items_after_flex_breaches,
)
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    COMPLEXITY_SUFFIXES,
)
from spice.studies.walk import is_excluded_path, staged_renames

COMPLEXITY_VERSION = 1
COMPLEXITY_CCN_STICKY_GIT_PATH = "spice/complexity-ccn-sticky.json"
COMPLEXITY_LENGTH_STICKY_GIT_PATH = "spice/complexity-length-sticky.json"
LIZARD_SUFFIXES = COMPLEXITY_SUFFIXES

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


def require_lizard() -> str:
    located = find_tool("lizard")
    if not located:
        raise SpiceError(
            "lizard is required for the complexity gate; it installs with "
            "spice, so the installation is broken or incomplete"
        )
    return located


def collect_complexity_records(
    paths: list[Path], *, root: Path
) -> list[ComplexityRecord]:
    targets = [
        path
        for path in paths
        if path.suffix in LIZARD_SUFFIXES
        and not is_excluded_path(path, repo_root=root)
        and (root / path).exists()
    ]
    if not targets:
        return []
    lizard = require_lizard()
    result = subprocess.run(
        [lizard, "--csv", *[str(root / path) for path in targets]],
        capture_output=True,
        text=True,
        cwd=root,
        check=False,
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
    persist: bool = False,
) -> list[ComplexityFinding]:
    """Scan staged routines against the flex+sticky CCN/length limits.

    New flex breaches are folded into the sticky set used to compute this
    call's findings. Persisting that set to the git dir is **opt-in**: pass
    ``persist=True`` (the committing gate does); a reporting or study caller
    leaves it ``False`` so the scan is a pure query that never advances shared
    sticky state. A ``persist=True`` scan must be paired with
    ``clear_complexity_sticky_state`` on gate success — scan ratchets up, clear
    prunes down once the tree passes.
    """
    records = collect_complexity_records(paths, root=root)
    renames = staged_renames(root)
    ccn_sticky = sticky_function_keys_after_renames(
        _load_sticky(root, COMPLEXITY_CCN_STICKY_GIT_PATH), renames
    )
    length_sticky = sticky_function_keys_after_renames(
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
    updated_ccn_sticky = sticky_items_after_flex_breaches(
        records,
        ccn_sticky,
        key_for_item=lambda record: record.key,
        is_breach=lambda record: record.ccn > ccn_flex,
    )
    updated_length_sticky = sticky_items_after_flex_breaches(
        records,
        length_sticky,
        key_for_item=lambda record: record.key,
        is_breach=lambda record: record.length > length_flex,
    )
    if persist:
        if updated_ccn_sticky != ccn_sticky:
            _save_sticky(updated_ccn_sticky, root, COMPLEXITY_CCN_STICKY_GIT_PATH)
        if updated_length_sticky != length_sticky:
            _save_sticky(updated_length_sticky, root, COMPLEXITY_LENGTH_STICKY_GIT_PATH)
    findings: list[ComplexityFinding] = []
    for record in records:
        ccn_limit = max_ccn if record.key in updated_ccn_sticky else ccn_flex
        length_limit = (
            max_length if record.key in updated_length_sticky else length_flex
        )
        over_ccn = record.ccn > ccn_limit
        over_length = record.length > length_limit
        if over_ccn or over_length:
            findings.append(
                ComplexityFinding(
                    record=record,
                    over_ccn=over_ccn,
                    over_length=over_length,
                    ccn_limit=ccn_limit,
                    length_limit=length_limit,
                )
            )
    return findings


def clear_complexity_sticky_state(
    *,
    root: Path,
    max_ccn: int = COMPLEXITY_MAX_CCN,
    max_length: int = COMPLEXITY_MAX_LENGTH,
) -> None:
    for git_path, attribute, limit in (
        (COMPLEXITY_CCN_STICKY_GIT_PATH, "ccn", max_ccn),
        (COMPLEXITY_LENGTH_STICKY_GIT_PATH, "length", max_length),
    ):
        state_path = git_state_path(git_path, root=root)
        if not state_path.exists():
            continue
        sticky = _load_sticky(root, git_path)
        live_paths = sorted({Path(path) for path, _name in sticky})
        records = collect_complexity_records(live_paths, root=root)
        by_key = {record.key: record for record in records}
        retained = {
            key
            for key in sticky
            if key in by_key and getattr(by_key[key], attribute) > limit
        }
        if retained:
            _save_sticky(retained, root, git_path)
        else:
            state_path.unlink()


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
        lines.append(
            f"  FAIL  {finding.record.path}:{finding.record.function_name}: "
            f"{'; '.join(reasons)}"
        )
    return "\n".join(lines)
