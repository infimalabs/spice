"""File shape pressure: lines and bytes, flex headroom, sticky breaches.

A file may grow to the flex limit (base ×1.5), but a file that ever breached
flex stays held to the base limit until it shrinks back under it. Breach
state persists in the git dir (`.spice/file-loc-sticky.json`,
`.spice/file-byte-sticky.json`), follows staged renames, and is re-evaluated
(and pruned) on every gate scan: a latch retires the moment any scan sees the
file back at or under its base limit, so a latch first recorded in one
(now-idle) worktree heals as soon as the fix lands on the shared baseline.

"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Mapping, Protocol

from spice.flexstate import (
    FlexSliceClaim,
    flex_limit,
    render_flex_slice_claim_redirect,
)
from spice.policy import (
    FILE_BYTE_LIMIT,
    FILE_LOC_LIMIT,
    FILE_SHAPE_GENERATED_SOURCE_PATTERNS,
    FILE_SHAPE_GENERATED_LOCKFILE_NAMES,
    FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES,
    FILE_SHAPE_SOURCE_SUFFIXES,
)
from spice.pathmatch import matches_repo_path
from spice.paths import STATE_DIRNAME
from spice.studies import gates
from spice.studies.walk import is_excluded_path

FILE_LOC_VERSION = 1
FILE_LOC_STICKY_STATE_GIT_PATH = "file-loc-sticky.json"
FILE_BYTE_STICKY_STATE_GIT_PATH = "file-byte-sticky.json"
# Reachable name of each sticky ledger (worktree git-dir state), surfaced on the
# board so a held-at-base failure points at the file holding the latch.
_LINE_STICKY_LEDGER_LABEL = f"{STATE_DIRNAME}/{FILE_LOC_STICKY_STATE_GIT_PATH}"
_BYTE_STICKY_LEDGER_LABEL = f"{STATE_DIRNAME}/{FILE_BYTE_STICKY_STATE_GIT_PATH}"
_LINE_STICKY_LEDGER = gates.path_sticky_ledger(
    FILE_LOC_STICKY_STATE_GIT_PATH,
    version=FILE_LOC_VERSION,
)
_BYTE_STICKY_LEDGER = gates.path_sticky_ledger(
    FILE_BYTE_STICKY_STATE_GIT_PATH,
    version=FILE_LOC_VERSION,
)


@dataclass(frozen=True)
class LocFinding:
    path: str
    line_count: int
    byte_count: int
    over_line_limit: bool
    over_byte_limit: bool
    line_limit: int
    byte_limit: int
    line_flex_breach: bool
    byte_flex_breach: bool
    flex_slice_claim: FlexSliceClaim | None = None

    @property
    def line_latch_held(self) -> bool:
        """Over the line limit only because a latch holds it to base."""
        return self.over_line_limit and not self.line_flex_breach

    @property
    def byte_latch_held(self) -> bool:
        """Over the byte limit only because a latch holds it to base."""
        return self.over_byte_limit and not self.byte_flex_breach

    @property
    def latch_held(self) -> bool:
        return self.line_latch_held or self.byte_latch_held

    @property
    def current_breach(self) -> bool:
        """A dimension measures over the flex limit right now, latch or not."""
        return (self.over_line_limit and self.line_flex_breach) or (
            self.over_byte_limit and self.byte_flex_breach
        )


class FileShapeBounds(Protocol):
    @property
    def line_limit(self) -> int: ...

    @property
    def line_flex_limit(self) -> int: ...

    @property
    def byte_limit(self) -> int: ...

    @property
    def byte_flex_limit(self) -> int: ...

    @property
    def line_unlimited(self) -> bool: ...

    @property
    def byte_unlimited(self) -> bool: ...


@dataclass(frozen=True)
class _FileShapeBoundSet:
    lines: gates.BoundedValue
    bytes: gates.BoundedValue


@dataclass(frozen=True)
class _FileShapeScanConfig:
    line_limit: int
    line_flex: int
    byte_limit: int
    byte_flex: int
    bounds_for_path: Callable[[Path], FileShapeBounds] | None
    resolve_bounds: Callable[[Path], _FileShapeBoundSet]
    source_suffixes: tuple[str, ...]
    generated_patterns: tuple[str, ...]
    repo_doc_paths: set[Path]
    lockfile_suffixes: tuple[str, ...]
    lockfile_names: tuple[str, ...]


def count_file_lines(path: Path) -> int:
    raw = path.read_bytes()
    if not _is_text_blob(raw):
        return 0
    return len(raw.decode("utf-8", errors="replace").splitlines())


def _is_text_blob(raw: bytes) -> bool:
    if b"\0" in raw:
        return False
    try:
        raw.decode("utf-8")
    except UnicodeDecodeError:
        return False
    return True


def count_file_bytes(path: Path) -> int:
    return len(path.read_bytes())


def is_generated_lockfile_path(
    path: Path,
    *,
    lockfile_suffixes: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES,
    lockfile_names: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_NAMES,
) -> bool:
    return path.suffix in lockfile_suffixes or path.name in lockfile_names


def _repo_path(path: Path) -> Path:
    normalized = Path(path.as_posix().strip().removeprefix("./"))
    return normalized


def _is_file_shape_candidate(
    path: Path,
    *,
    root: Path,
    source_suffixes: tuple[str, ...],
    generated_patterns: tuple[str, ...],
    repo_doc_paths: set[Path],
    lockfile_suffixes: tuple[str, ...],
    lockfile_names: tuple[str, ...],
) -> bool:
    rel_path = _repo_path(path)
    if (
        rel_path in repo_doc_paths
        or rel_path.suffix not in source_suffixes
        or is_generated_lockfile_path(
            rel_path,
            lockfile_suffixes=lockfile_suffixes,
            lockfile_names=lockfile_names,
        )
        or any(matches_repo_path(rel_path, pattern) for pattern in generated_patterns)
        or is_excluded_path(rel_path, repo_root=root)
    ):
        return False
    abs_path = root / rel_path
    if not abs_path.exists() or not abs_path.is_file():
        return False
    return _is_text_blob(abs_path.read_bytes())


def _retained_file_shape_sticky(
    paths: set[Path],
    *,
    root: Path,
    measure: Callable[[Path], int],
    bounds_for_path: Callable[[Path], gates.BoundedValue],
    source_suffixes: tuple[str, ...],
    generated_patterns: tuple[str, ...],
    repo_doc_paths: set[Path],
    lockfile_suffixes: tuple[str, ...],
    lockfile_names: tuple[str, ...],
) -> set[Path]:
    retained: set[Path] = set()
    for path in paths:
        rel_path = _repo_path(path)
        if not _is_file_shape_candidate(
            rel_path,
            root=root,
            source_suffixes=source_suffixes,
            generated_patterns=generated_patterns,
            repo_doc_paths=repo_doc_paths,
            lockfile_suffixes=lockfile_suffixes,
            lockfile_names=lockfile_names,
        ):
            continue
        disposition = gates.bounded_disposition(
            measure(root / rel_path),
            bounds_for_path(rel_path),
        )
        if disposition.over_base:
            retained.add(rel_path)
    return retained


def scan_staged_loc_violations(
    paths: list[Path],
    *,
    root: Path,
    limit: int = FILE_LOC_LIMIT,
    flex_limit_value: int | None = None,
    byte_limit: int = FILE_BYTE_LIMIT,
    byte_flex_limit_value: int | None = None,
    bounds_for_path: Callable[[Path], FileShapeBounds] | None = None,
    source_suffixes: tuple[str, ...] = FILE_SHAPE_SOURCE_SUFFIXES,
    generated_patterns: tuple[str, ...] = FILE_SHAPE_GENERATED_SOURCE_PATTERNS,
    repo_doc_paths: set[Path] | frozenset[Path] | None = None,
    lockfile_suffixes: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES,
    lockfile_names: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_NAMES,
    persist: bool = False,
    flex_actor: str = "",
    flex_claim_now: float | None = None,
) -> list[LocFinding]:
    """Scan staged paths against the flex+sticky line/byte limits.

    ``persist`` writes sticky state for the committing gate; ``flex_actor``
    separately records or honors live flex slice claims for flex-breaching
    paths. Leave ``flex_actor`` empty for read-only scans.
    """
    config = _file_shape_scan_config(
        limit=limit,
        flex_limit_value=flex_limit_value,
        byte_limit=byte_limit,
        byte_flex_limit_value=byte_flex_limit_value,
        bounds_for_path=bounds_for_path,
        source_suffixes=source_suffixes,
        generated_patterns=generated_patterns,
        repo_doc_paths=repo_doc_paths,
        lockfile_suffixes=lockfile_suffixes,
        lockfile_names=lockfile_names,
    )
    return _scan_staged_file_shape(
        paths,
        root=root,
        config=config,
        persist=persist,
        flex_actor=flex_actor,
        flex_claim_now=flex_claim_now,
    )


def _file_shape_scan_config(
    *,
    limit: int,
    flex_limit_value: int | None,
    byte_limit: int,
    byte_flex_limit_value: int | None,
    bounds_for_path: Callable[[Path], FileShapeBounds] | None,
    source_suffixes: tuple[str, ...],
    generated_patterns: tuple[str, ...],
    repo_doc_paths: set[Path] | frozenset[Path] | None,
    lockfile_suffixes: tuple[str, ...],
    lockfile_names: tuple[str, ...],
) -> _FileShapeScanConfig:
    line_flex = flex_limit_value if flex_limit_value is not None else flex_limit(limit)
    byte_flex = (
        byte_flex_limit_value
        if byte_flex_limit_value is not None
        else flex_limit(byte_limit)
    )
    default_bounds = _FileShapeBoundSet(
        lines=gates.BoundedValue.from_base(limit, line_flex),
        bytes=gates.BoundedValue.from_base(byte_limit, byte_flex),
    )
    return _FileShapeScanConfig(
        line_limit=limit,
        line_flex=line_flex,
        byte_limit=byte_limit,
        byte_flex=byte_flex,
        bounds_for_path=bounds_for_path,
        resolve_bounds=lambda path: _resolved_file_shape_bounds(
            path,
            bounds_for_path=bounds_for_path,
            default_bounds=default_bounds,
        ),
        source_suffixes=source_suffixes,
        generated_patterns=generated_patterns,
        repo_doc_paths={_repo_path(path) for path in repo_doc_paths or set()},
        lockfile_suffixes=lockfile_suffixes,
        lockfile_names=lockfile_names,
    )


def _resolved_file_shape_bounds(
    path: Path,
    *,
    bounds_for_path: Callable[[Path], FileShapeBounds] | None,
    default_bounds: _FileShapeBoundSet,
) -> _FileShapeBoundSet:
    if bounds_for_path is None:
        return default_bounds
    bounds = bounds_for_path(path)
    return _FileShapeBoundSet(
        lines=gates.BoundedValue(
            base_limit=bounds.line_limit,
            flex_limit=bounds.line_flex_limit,
            unlimited=bounds.line_unlimited,
        ),
        bytes=gates.BoundedValue(
            base_limit=bounds.byte_limit,
            flex_limit=bounds.byte_flex_limit,
            unlimited=bounds.byte_unlimited,
        ),
    )


def _scan_staged_file_shape(
    paths: list[Path],
    *,
    root: Path,
    config: _FileShapeScanConfig,
    persist: bool,
    flex_actor: str,
    flex_claim_now: float | None,
) -> list[LocFinding]:
    renames = gates.staged_gate_renames(root)
    line_breaches, byte_breaches = _file_shape_breach_sets(
        paths,
        root=root,
        config=config,
    )
    line_state = _file_shape_sticky_state(
        _LINE_STICKY_LEDGER,
        root=root,
        renames=renames,
        config=config,
        measure=count_file_lines,
        bounds_for_path=lambda path: config.resolve_bounds(path).lines,
        breaches=line_breaches,
        persist=persist,
    )
    byte_state = _file_shape_sticky_state(
        _BYTE_STICKY_LEDGER,
        root=root,
        renames=renames,
        config=config,
        measure=count_file_bytes,
        bounds_for_path=lambda path: config.resolve_bounds(path).bytes,
        breaches=byte_breaches,
        persist=persist,
    )
    peer_claims = gates.peer_flex_slice_claims(
        line_breaches | byte_breaches,
        root=root,
        actor=flex_actor,
        renames=renames,
        now=flex_claim_now,
    )
    return _scan_file_shape_findings(
        paths,
        root=root,
        config=config,
        updated_line_sticky=line_state.updated,
        updated_byte_sticky=byte_state.updated,
        peer_claims=peer_claims,
    )


def _scan_file_shape_findings(
    paths: list[Path],
    *,
    root: Path,
    config: _FileShapeScanConfig,
    updated_line_sticky: set[Path],
    updated_byte_sticky: set[Path],
    peer_claims: dict[Path, FlexSliceClaim],
) -> list[LocFinding]:
    return scan_loc_violations(
        paths,
        root=root,
        limit=config.line_limit,
        flex_limit_value=config.line_flex,
        byte_limit=config.byte_limit,
        byte_flex_limit_value=config.byte_flex,
        source_suffixes=config.source_suffixes,
        generated_patterns=config.generated_patterns,
        repo_doc_paths=config.repo_doc_paths,
        lockfile_suffixes=config.lockfile_suffixes,
        lockfile_names=config.lockfile_names,
        sticky_paths=updated_line_sticky,
        byte_sticky_paths=updated_byte_sticky,
        bounds_for_path=config.bounds_for_path,
        flex_slice_claims=peer_claims,
    )


def _file_shape_sticky_state(
    ledger: gates.StickyLedger[Path],
    *,
    root: Path,
    renames: dict[Path, Path],
    config: _FileShapeScanConfig,
    measure: Callable[[Path], int],
    bounds_for_path: Callable[[Path], gates.BoundedValue],
    breaches: set[Path],
    persist: bool,
) -> gates.StickyLatchState[Path]:
    return gates.reconcile_sticky_latch(
        ledger,
        root=root,
        renames=renames,
        retain=lambda paths: _retained_file_shape_sticky(
            paths,
            root=root,
            measure=measure,
            bounds_for_path=bounds_for_path,
            source_suffixes=config.source_suffixes,
            generated_patterns=config.generated_patterns,
            repo_doc_paths=config.repo_doc_paths,
            lockfile_suffixes=config.lockfile_suffixes,
            lockfile_names=config.lockfile_names,
        ),
        breach_keys=breaches,
        persist=persist,
    )


def _file_shape_breach_sets(
    paths: list[Path],
    *,
    root: Path,
    config: _FileShapeScanConfig,
) -> tuple[set[Path], set[Path]]:
    line_breaches = _breach_paths(
        paths,
        root=root,
        measure=count_file_lines,
        bounds_for_path=lambda path: config.resolve_bounds(path).lines,
        source_suffixes=config.source_suffixes,
        generated_patterns=config.generated_patterns,
        repo_doc_paths=config.repo_doc_paths,
        lockfile_suffixes=config.lockfile_suffixes,
        lockfile_names=config.lockfile_names,
    )
    byte_breaches = _breach_paths(
        paths,
        root=root,
        measure=count_file_bytes,
        bounds_for_path=lambda path: config.resolve_bounds(path).bytes,
        source_suffixes=config.source_suffixes,
        generated_patterns=config.generated_patterns,
        repo_doc_paths=config.repo_doc_paths,
        lockfile_suffixes=config.lockfile_suffixes,
        lockfile_names=config.lockfile_names,
    )
    return line_breaches, byte_breaches


def _breach_paths(
    paths: list[Path],
    *,
    root: Path,
    measure: Callable[[Path], int],
    bounds_for_path: Callable[[Path], gates.BoundedValue],
    source_suffixes: tuple[str, ...] = FILE_SHAPE_SOURCE_SUFFIXES,
    generated_patterns: tuple[str, ...] = FILE_SHAPE_GENERATED_SOURCE_PATTERNS,
    repo_doc_paths: set[Path] | None = None,
    lockfile_suffixes: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES,
    lockfile_names: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_NAMES,
) -> set[Path]:
    return {
        rel_path
        for rel_path in [
            _repo_path(path)
            for path in paths
            if _is_file_shape_candidate(
                path,
                root=root,
                source_suffixes=source_suffixes,
                generated_patterns=generated_patterns,
                repo_doc_paths=repo_doc_paths or set(),
                lockfile_suffixes=lockfile_suffixes,
                lockfile_names=lockfile_names,
            )
        ]
        if (root / rel_path).exists()
        and gates.bounded_disposition(
            measure(root / rel_path), bounds_for_path(rel_path)
        ).flex_breach
    }


def scan_loc_violations(
    paths: list[Path],
    *,
    root: Path,
    limit: int = FILE_LOC_LIMIT,
    flex_limit_value: int | None = None,
    byte_limit: int = FILE_BYTE_LIMIT,
    byte_flex_limit_value: int | None = None,
    source_suffixes: tuple[str, ...] = FILE_SHAPE_SOURCE_SUFFIXES,
    generated_patterns: tuple[str, ...] = FILE_SHAPE_GENERATED_SOURCE_PATTERNS,
    repo_doc_paths: set[Path] | frozenset[Path] | None = None,
    lockfile_suffixes: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES,
    lockfile_names: tuple[str, ...] = FILE_SHAPE_GENERATED_LOCKFILE_NAMES,
    sticky_paths: set[Path] | None = None,
    byte_sticky_paths: set[Path] | None = None,
    bounds_for_path: Callable[[Path], FileShapeBounds] | None = None,
    flex_slice_claims: Mapping[Path, FlexSliceClaim] | None = None,
) -> list[LocFinding]:
    findings: list[LocFinding] = []
    line_flex = flex_limit_value if flex_limit_value is not None else flex_limit(limit)
    byte_flex = (
        byte_flex_limit_value
        if byte_flex_limit_value is not None
        else flex_limit(byte_limit)
    )
    default_bounds = _FileShapeBoundSet(
        lines=gates.BoundedValue.from_base(limit, line_flex),
        bytes=gates.BoundedValue.from_base(byte_limit, byte_flex),
    )

    def resolve_bounds(path: Path) -> _FileShapeBoundSet:
        return _resolved_file_shape_bounds(
            path,
            bounds_for_path=bounds_for_path,
            default_bounds=default_bounds,
        )

    sticky_paths = sticky_paths or set()
    byte_sticky_paths = byte_sticky_paths or set()
    flex_slice_claims = {
        _repo_path(path): claim for path, claim in (flex_slice_claims or {}).items()
    }
    repo_doc_path_set = {_repo_path(path) for path in repo_doc_paths or set()}
    for rel_path in paths:
        rel_path = _repo_path(rel_path)
        if not _is_file_shape_candidate(
            rel_path,
            root=root,
            source_suffixes=source_suffixes,
            generated_patterns=generated_patterns,
            repo_doc_paths=repo_doc_path_set,
            lockfile_suffixes=lockfile_suffixes,
            lockfile_names=lockfile_names,
        ):
            continue
        abs_path = root / rel_path
        bounds = resolve_bounds(rel_path)
        if bounds.lines.unlimited and bounds.bytes.unlimited:
            continue
        line_count = count_file_lines(abs_path)
        byte_count = count_file_bytes(abs_path)
        line_disposition = gates.bounded_disposition(
            line_count,
            bounds.lines,
            latched=rel_path in sticky_paths,
        )
        byte_disposition = gates.bounded_disposition(
            byte_count,
            bounds.bytes,
            latched=rel_path in byte_sticky_paths,
        )
        if not (line_disposition.over_limit or byte_disposition.over_limit):
            continue
        findings.append(
            LocFinding(
                path=rel_path.as_posix(),
                line_count=line_count,
                byte_count=byte_count,
                over_line_limit=line_disposition.over_limit,
                over_byte_limit=byte_disposition.over_limit,
                line_limit=line_disposition.limit,
                byte_limit=byte_disposition.limit,
                line_flex_breach=line_disposition.flex_breach,
                byte_flex_breach=byte_disposition.flex_breach,
                flex_slice_claim=flex_slice_claims.get(rel_path),
            )
        )
    return findings


def render_loc_board(
    findings: list[LocFinding],
    *,
    limit: int = FILE_LOC_LIMIT,
    flex_limit_value: int | None = None,
    byte_limit: int = FILE_BYTE_LIMIT,
    byte_flex_limit_value: int | None = None,
) -> str:
    if not findings:
        line_flex = (
            flex_limit_value if flex_limit_value is not None else flex_limit(limit)
        )
        byte_flex = (
            byte_flex_limit_value
            if byte_flex_limit_value is not None
            else flex_limit(byte_limit)
        )
        return (
            f"file-loc: ok (line_limit {limit} flex {line_flex} "
            f"byte_limit {byte_limit} byte_flex {byte_flex})"
        )
    lines = [f"file-loc: {len(findings)} violation(s)"]
    for finding in findings:
        reasons = []
        if finding.over_line_limit:
            reasons.append(
                gates.held_at_base_reason(
                    f"{finding.line_count} lines > {finding.line_limit}",
                    held=finding.line_latch_held,
                    ledger_label=_LINE_STICKY_LEDGER_LABEL,
                )
            )
        if finding.over_byte_limit:
            reasons.append(
                gates.held_at_base_reason(
                    f"{finding.byte_count} bytes > {finding.byte_limit}",
                    held=finding.byte_latch_held,
                    ledger_label=_BYTE_STICKY_LEDGER_LABEL,
                )
            )
        if finding.flex_slice_claim is not None:
            reasons.append(render_flex_slice_claim_redirect(finding.flex_slice_claim))
        lines.append(f"  FAIL  {finding.path}: {'; '.join(reasons)}")
    if any(f.current_breach and f.flex_slice_claim is None for f in findings):
        lines.append(
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam"
        )
    if any(finding.latch_held for finding in findings):
        lines.append(gates.render_latch_held_guidance("path"))
    if any(finding.flex_slice_claim is not None for finding in findings):
        lines.append(
            "  peer-held flex slices redirect duplicate refactors; keep changes "
            "append-only or move to another seam"
        )
    return "\n".join(lines)
