"""File shape pressure: lines and bytes, flex headroom, sticky breaches.

A file may grow to the flex limit (base ×1.5), but a file that ever breached
flex stays held to the base limit until it shrinks back under it. Breach
state persists in the git dir (`spice/file-loc-sticky.json`,
`spice/file-byte-sticky.json`), follows staged renames, and is re-evaluated
(and pruned) on every gate scan: a latch retires the moment any scan sees the
file back at or under its base limit, so a latch first recorded in one
(now-idle) worktree heals as soon as the fix lands on the shared baseline.

"""

from __future__ import annotations

from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Callable, Mapping, Protocol

from spice.flexstate import (
    FlexSliceClaim,
    claim_flex_slice_paths,
    flex_limit,
    git_state_path,
    load_sticky_items,
    render_flex_slice_claim_redirect,
    save_sticky_items,
    sticky_paths_after_renames,
)
from spice.policy import (
    FILE_BYTE_LIMIT,
    FILE_LOC_LIMIT,
    FILE_SHAPE_GENERATED_SOURCE_PATTERNS,
    FILE_SHAPE_GENERATED_LOCKFILE_NAMES,
    FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES,
    FILE_SHAPE_SOURCE_SUFFIXES,
)
from spice.studies.walk import is_excluded_path, staged_renames

FILE_LOC_VERSION = 1
FILE_LOC_STICKY_STATE_GIT_PATH = "spice/file-loc-sticky.json"
FILE_BYTE_STICKY_STATE_GIT_PATH = "spice/file-byte-sticky.json"


@dataclass(frozen=True)
class LocFinding:
    path: str
    line_count: int
    byte_count: int
    over_line_limit: bool
    over_byte_limit: bool
    line_limit: int
    byte_limit: int
    flex_slice_claim: FlexSliceClaim | None = None


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
class _DefaultFileShapeBounds:
    line_limit: int
    line_flex_limit: int
    byte_limit: int
    byte_flex_limit: int
    line_unlimited: bool = False
    byte_unlimited: bool = False


@dataclass(frozen=True)
class _FileShapeScanConfig:
    line_limit: int
    line_flex: int
    byte_limit: int
    byte_flex: int
    bounds_for_path: Callable[[Path], FileShapeBounds] | None
    resolve_bounds: Callable[[Path], FileShapeBounds]
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


def _load_sticky(root: Path, git_path: str) -> set[Path]:
    return load_sticky_items(
        root=root,
        state_path=None,
        git_path=git_path,
        entries_key="paths",
        decode=lambda raw: Path(raw) if isinstance(raw, str) else None,
        version=FILE_LOC_VERSION,
    )


def _save_sticky(paths: set[Path], root: Path, git_path: str) -> None:
    save_sticky_items(
        paths,
        root=root,
        state_path=None,
        git_path=git_path,
        entries_key="paths",
        encode=lambda path: path.as_posix(),
        version=FILE_LOC_VERSION,
    )


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


def _has_glob_magic(pattern: str) -> bool:
    return any(char in pattern for char in "*?[")


def _matches_generated_pattern(path: Path, generated_patterns: tuple[str, ...]) -> bool:
    rel_posix = _repo_path(path).as_posix()
    for raw_pattern in generated_patterns:
        pattern = raw_pattern.strip().replace("\\", "/").removeprefix("./")
        if not pattern:
            continue
        if not _has_glob_magic(pattern):
            prefix = pattern.rstrip("/")
            if rel_posix == prefix or rel_posix.startswith(prefix + "/"):
                return True
            continue
        if fnmatchcase(rel_posix, pattern):
            return True
        if pattern.startswith("**/") and fnmatchcase(rel_posix, pattern[3:]):
            return True
    return False


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
        or _matches_generated_pattern(rel_path, generated_patterns)
        or is_excluded_path(rel_path, repo_root=root)
    ):
        return False
    abs_path = root / rel_path
    if not abs_path.exists() or not abs_path.is_file():
        return False
    return _is_text_blob(abs_path.read_bytes())


def _retained_over_base_sticky(
    paths: set[Path],
    *,
    root: Path,
    measure: Callable[[Path], int],
    limit_for_path: Callable[[Path], int],
    unlimited_for_path: Callable[[Path], bool],
    source_suffixes: tuple[str, ...],
    generated_patterns: tuple[str, ...],
    repo_doc_paths: set[Path],
    lockfile_suffixes: tuple[str, ...],
    lockfile_names: tuple[str, ...],
) -> set[Path]:
    """The still-latched subset: candidates still over their base limit.

    A latch only records that a file once breached flex; it is retired the
    moment the file is back at or under its base limit. Evaluating that here on
    every scan — not only after a fully clean commit — is what lets a latch
    recorded in one (now-idle) worktree heal as soon as any scan sees the file
    under base again.
    """
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
        if not unlimited_for_path(rel_path) and measure(root / rel_path) > (
            limit_for_path(rel_path)
        ):
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
    default_bounds = _DefaultFileShapeBounds(
        line_limit=limit,
        line_flex_limit=line_flex,
        byte_limit=byte_limit,
        byte_flex_limit=byte_flex,
    )
    return _FileShapeScanConfig(
        line_limit=limit,
        line_flex=line_flex,
        byte_limit=byte_limit,
        byte_flex=byte_flex,
        bounds_for_path=bounds_for_path,
        resolve_bounds=bounds_for_path or (lambda _path: default_bounds),
        source_suffixes=source_suffixes,
        generated_patterns=generated_patterns,
        repo_doc_paths={_repo_path(path) for path in repo_doc_paths or set()},
        lockfile_suffixes=lockfile_suffixes,
        lockfile_names=lockfile_names,
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
    renames = staged_renames(root)
    loaded_line_sticky, loaded_byte_sticky, line_sticky, byte_sticky = (
        _current_file_shape_sticky(root=root, renames=renames, config=config)
    )
    line_breaches, byte_breaches = _file_shape_breach_sets(
        paths,
        root=root,
        config=config,
    )
    updated_line_sticky = line_sticky | line_breaches
    updated_byte_sticky = byte_sticky | byte_breaches
    if persist:
        _save_file_shape_sticky(
            root=root,
            loaded_line_sticky=loaded_line_sticky,
            loaded_byte_sticky=loaded_byte_sticky,
            updated_line_sticky=updated_line_sticky,
            updated_byte_sticky=updated_byte_sticky,
        )
    peer_claims = _peer_flex_slice_claims(
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
        updated_line_sticky=updated_line_sticky,
        updated_byte_sticky=updated_byte_sticky,
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


def _current_file_shape_sticky(
    *,
    root: Path,
    renames: dict[Path, Path],
    config: _FileShapeScanConfig,
) -> tuple[set[Path], set[Path], set[Path], set[Path]]:
    loaded_line_sticky = sticky_paths_after_renames(
        _load_sticky(root, FILE_LOC_STICKY_STATE_GIT_PATH), renames
    )
    loaded_byte_sticky = sticky_paths_after_renames(
        _load_sticky(root, FILE_BYTE_STICKY_STATE_GIT_PATH), renames
    )
    line_sticky = _retained_over_base_sticky(
        loaded_line_sticky,
        root=root,
        measure=count_file_lines,
        limit_for_path=lambda path: config.resolve_bounds(path).line_limit,
        unlimited_for_path=lambda path: config.resolve_bounds(path).line_unlimited,
        source_suffixes=config.source_suffixes,
        generated_patterns=config.generated_patterns,
        repo_doc_paths=config.repo_doc_paths,
        lockfile_suffixes=config.lockfile_suffixes,
        lockfile_names=config.lockfile_names,
    )
    byte_sticky = _retained_over_base_sticky(
        loaded_byte_sticky,
        root=root,
        measure=count_file_bytes,
        limit_for_path=lambda path: config.resolve_bounds(path).byte_limit,
        unlimited_for_path=lambda path: config.resolve_bounds(path).byte_unlimited,
        source_suffixes=config.source_suffixes,
        generated_patterns=config.generated_patterns,
        repo_doc_paths=config.repo_doc_paths,
        lockfile_suffixes=config.lockfile_suffixes,
        lockfile_names=config.lockfile_names,
    )
    return loaded_line_sticky, loaded_byte_sticky, line_sticky, byte_sticky


def _save_file_shape_sticky(
    *,
    root: Path,
    loaded_line_sticky: set[Path],
    loaded_byte_sticky: set[Path],
    updated_line_sticky: set[Path],
    updated_byte_sticky: set[Path],
) -> None:
    if updated_line_sticky != loaded_line_sticky:
        _persist_sticky(updated_line_sticky, root, FILE_LOC_STICKY_STATE_GIT_PATH)
    if updated_byte_sticky != loaded_byte_sticky:
        _persist_sticky(updated_byte_sticky, root, FILE_BYTE_STICKY_STATE_GIT_PATH)


def _persist_sticky(paths: set[Path], root: Path, git_path: str) -> None:
    """Write the latch set, or delete the state file once nothing stays latched."""
    if paths:
        _save_sticky(paths, root, git_path)
        return
    state_path = git_state_path(git_path, root=root)
    if state_path.exists():
        state_path.unlink()


def _file_shape_breach_sets(
    paths: list[Path],
    *,
    root: Path,
    config: _FileShapeScanConfig,
) -> tuple[set[Path], set[Path]]:
    line_breaches = _breach_paths(
        paths,
        root=root,
        flex=config.line_flex,
        measure=count_file_lines,
        flex_for_path=lambda path: config.resolve_bounds(path).line_flex_limit,
        unlimited_for_path=lambda path: config.resolve_bounds(path).line_unlimited,
        source_suffixes=config.source_suffixes,
        generated_patterns=config.generated_patterns,
        repo_doc_paths=config.repo_doc_paths,
        lockfile_suffixes=config.lockfile_suffixes,
        lockfile_names=config.lockfile_names,
    )
    byte_breaches = _breach_paths(
        paths,
        root=root,
        flex=config.byte_flex,
        measure=count_file_bytes,
        flex_for_path=lambda path: config.resolve_bounds(path).byte_flex_limit,
        unlimited_for_path=lambda path: config.resolve_bounds(path).byte_unlimited,
        source_suffixes=config.source_suffixes,
        generated_patterns=config.generated_patterns,
        repo_doc_paths=config.repo_doc_paths,
        lockfile_suffixes=config.lockfile_suffixes,
        lockfile_names=config.lockfile_names,
    )
    return line_breaches, byte_breaches


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


def _breach_paths(
    paths: list[Path],
    *,
    root: Path,
    flex: int,
    measure: Callable[[Path], int],
    flex_for_path: Callable[[Path], int] | None = None,
    unlimited_for_path: Callable[[Path], bool] | None = None,
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
        and not (
            unlimited_for_path(rel_path) if unlimited_for_path is not None else False
        )
        and measure(root / rel_path)
        > (flex_for_path(rel_path) if flex_for_path is not None else flex)
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
    default_bounds = _DefaultFileShapeBounds(
        line_limit=limit,
        line_flex_limit=line_flex,
        byte_limit=byte_limit,
        byte_flex_limit=byte_flex,
    )
    resolve_bounds = bounds_for_path or (lambda _path: default_bounds)
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
        if bounds.line_unlimited and bounds.byte_unlimited:
            continue
        active_line_limit = (
            bounds.line_limit if rel_path in sticky_paths else bounds.line_flex_limit
        )
        active_byte_limit = (
            bounds.byte_limit
            if rel_path in byte_sticky_paths
            else bounds.byte_flex_limit
        )
        line_count = count_file_lines(abs_path)
        byte_count = count_file_bytes(abs_path)
        over_lines = False if bounds.line_unlimited else line_count > active_line_limit
        over_bytes = False if bounds.byte_unlimited else byte_count > active_byte_limit
        if not (over_lines or over_bytes):
            continue
        findings.append(
            LocFinding(
                path=rel_path.as_posix(),
                line_count=line_count,
                byte_count=byte_count,
                over_line_limit=over_lines,
                over_byte_limit=over_bytes,
                line_limit=active_line_limit,
                byte_limit=active_byte_limit,
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
            reasons.append(f"{finding.line_count} lines > {finding.line_limit}")
        if finding.over_byte_limit:
            reasons.append(f"{finding.byte_count} bytes > {finding.byte_limit}")
        if finding.flex_slice_claim is not None:
            reasons.append(render_flex_slice_claim_redirect(finding.flex_slice_claim))
        lines.append(f"  FAIL  {finding.path}: {'; '.join(reasons)}")
    if any(finding.flex_slice_claim is None for finding in findings):
        lines.append(
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam"
        )
    if any(finding.flex_slice_claim is not None for finding in findings):
        lines.append(
            "  peer-held flex slices redirect duplicate refactors; keep changes "
            "append-only or move to another seam"
        )
    return "\n".join(lines)
