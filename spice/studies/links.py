"""Tracked markdown link target case checks."""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Iterable

from spice.gitprocess import run_git_command
from spice.studies.walk import (
    is_excluded_path,
    policy_path_exclusions,
    staged_renames,
    tracked_paths as repo_tracked_paths,
)

_MARKDOWN_LINK_RE = re.compile(r"!?\[[^\]\n]*\]\(([^)\n]*)\)")
_URL_SCHEME_RE = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")


@dataclass(frozen=True)
class MarkdownLinkCaseFinding:
    source_path: Path
    line: int
    raw_target: str
    resolved_path: Path
    expected_path: Path


def markdown_link_case_findings(
    repo_root: Path,
    *,
    paths: Iterable[Path] | None = None,
    tracked_paths: Iterable[Path] | None = None,
) -> list[MarkdownLinkCaseFinding]:
    tracked = (
        tuple(tracked_paths)
        if tracked_paths is not None
        else tuple(repo_tracked_paths(repo_root))
    )
    tracked = _commit_candidate_tracked_paths(repo_root, tracked)
    source_paths = tuple(paths) if paths is not None else tracked
    tracked_by_casefold = _tracked_casefold_map(tracked)
    findings: list[MarkdownLinkCaseFinding] = []
    for source_path in sorted(
        _tracked_markdown_paths(source_paths), key=lambda path: path.as_posix()
    ):
        abs_source = repo_root / source_path
        if not abs_source.is_file():
            continue
        text = abs_source.read_text(encoding="utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            for raw_target in _markdown_link_targets(line):
                resolved = _resolve_markdown_target(source_path, raw_target)
                if resolved is None:
                    continue
                expected = tracked_by_casefold.get(resolved.as_posix().casefold())
                if expected is not None and expected != resolved:
                    findings.append(
                        MarkdownLinkCaseFinding(
                            source_path=source_path,
                            line=line_number,
                            raw_target=raw_target,
                            resolved_path=resolved,
                            expected_path=expected,
                        )
                    )
    return findings


def render_markdown_link_case_board(
    findings: list[MarkdownLinkCaseFinding],
) -> str:
    if not findings:
        return "markdown-links: ok"
    rows = [
        "markdown-links: "
        f"{len(findings)} case-mismatched tracked markdown link target(s)"
    ]
    rows.extend(
        (
            f"  FAIL  {finding.source_path.as_posix()}:{finding.line} "
            f"{finding.raw_target} -> {finding.expected_path.as_posix()}"
        )
        for finding in findings
    )
    return "\n".join(rows)


def _tracked_markdown_paths(paths: tuple[Path, ...]) -> tuple[Path, ...]:
    return tuple(path for path in paths if path.suffix.casefold() == ".md")


def _tracked_casefold_map(paths: tuple[Path, ...]) -> dict[str, Path]:
    return {path.as_posix().casefold(): path for path in paths}


def _commit_candidate_tracked_paths(
    repo_root: Path, paths: tuple[Path, ...]
) -> tuple[Path, ...]:
    try:
        renames = staged_renames(repo_root)
        deletes = _staged_deletes(repo_root)
    except subprocess.CalledProcessError:
        return paths
    updated = set(paths)
    updated.difference_update(deletes)
    for old_path, new_path in renames.items():
        updated.discard(old_path)
        updated.add(new_path)
    return tuple(sorted(updated, key=lambda path: path.as_posix()))


def _staged_deletes(repo_root: Path) -> set[Path]:
    exclusions = policy_path_exclusions(repo_root)
    result = run_git_command(
        ["git", "diff", "--cached", "--name-only", "--diff-filter=D"],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=True,
    )
    deletes: set[Path] = set()
    for line in result.stdout.splitlines():
        raw_path = line.strip()
        if not raw_path:
            continue
        path = Path(raw_path)
        if not is_excluded_path(path, policy_exclusions=exclusions):
            deletes.add(path)
    return deletes


def _markdown_link_targets(line: str) -> list[str]:
    return [
        target
        for match in _MARKDOWN_LINK_RE.finditer(line)
        if (target := _markdown_link_destination(match.group(1)))
    ]


def _markdown_link_destination(raw: str) -> str:
    stripped = raw.strip()
    if not stripped:
        return ""
    if stripped.startswith("<"):
        end = stripped.find(">")
        return stripped[1:end].strip() if end >= 0 else stripped[1:].strip()
    return stripped.split(maxsplit=1)[0]


def _resolve_markdown_target(source_path: Path, raw_target: str) -> Path | None:
    path_target = _target_path_part(raw_target)
    if _out_of_scope_target(raw_target, path_target):
        return None
    return _normalize_relative_path(source_path.parent, PurePosixPath(path_target))


def _target_path_part(raw_target: str) -> str:
    return raw_target.split("#", 1)[0].split("?", 1)[0].strip()


def _out_of_scope_target(raw_target: str, path_target: str) -> bool:
    stripped = raw_target.strip()
    if not stripped or stripped.startswith("#"):
        return True
    if _URL_SCHEME_RE.match(stripped):
        return True
    return not path_target or path_target.startswith("/")


def _normalize_relative_path(source_dir: Path, target: PurePosixPath) -> Path | None:
    parts: list[str] = []
    for part in (*PurePosixPath(source_dir.as_posix()).parts, *target.parts):
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                return None
            parts.pop()
            continue
        parts.append(part)
    return Path(*parts) if parts else Path(".")
