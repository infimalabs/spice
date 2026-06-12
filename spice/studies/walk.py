"""Shared walking rules: what the studies look at and what they never touch.

Library seam: target-repo tools may import public walkers, policy-exclusion
helpers, staged rename reads, and `git_blob_text`; underscored names remain
private.
"""

from __future__ import annotations

import subprocess
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Iterable, Iterator

from spice.repocfg import policy_table, string_list

_RENAME_STATUS_FIELDS = 3
EXCLUDED_PATH_PARTS = frozenset(
    {
        ".git",
        ".spice",
        ".venv",
        ".ruff_cache",
        ".pytest_cache",
        "__pycache__",
        "node_modules",
        "site-packages",
    }
)


def policy_path_exclusions(repo_root: Path) -> tuple[str, ...]:
    return tuple(string_list(policy_table(repo_root).get("exclude")))


def is_excluded_path(
    path: Path,
    *,
    repo_root: Path | None = None,
    policy_exclusions: Iterable[str] | None = None,
) -> bool:
    """True when ``path`` lives inside a directory the studies never scan.

    The risk surface is explicit path arguments: a path inside a vendored
    venv or cache would otherwise drag thousands of stubs into a report.
    Centralising the part-name set keeps every walker honest. Repos may also
    declare tracked policy exclusions for generated sources that are committed
    but should not count against the constitution gates.
    """
    if any(
        part in EXCLUDED_PATH_PARTS or part.startswith(".spice") for part in path.parts
    ):
        return True
    if policy_exclusions is None and repo_root is not None:
        policy_exclusions = policy_path_exclusions(repo_root)
    return any(
        _matches_policy_exclusion(path, pattern)
        for pattern in (policy_exclusions or ())
    )


def _matches_policy_exclusion(path: Path, pattern: str) -> bool:
    normalized_path = _normalized_git_path(path)
    normalized_pattern = _normalized_policy_pattern(pattern)
    if not normalized_path or not normalized_pattern:
        return False
    if _has_glob_magic(normalized_pattern):
        return fnmatchcase(normalized_path, normalized_pattern)
    prefix = normalized_pattern.rstrip("/")
    return normalized_path == prefix or normalized_path.startswith(prefix + "/")


def _normalized_git_path(path: Path) -> str:
    return path.as_posix().strip().removeprefix("./")


def _normalized_policy_pattern(pattern: str) -> str:
    return pattern.strip().replace("\\", "/").removeprefix("./")


def _has_glob_magic(pattern: str) -> bool:
    return any(char in pattern for char in "*?[")


def iter_source_files(root: Path, *, suffixes: Iterable[str]) -> Iterator[Path]:
    suffix_set = frozenset(suffixes)
    exclusions = policy_path_exclusions(root)
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        if path.suffix not in suffix_set:
            continue
        rel_path = path.relative_to(root)
        if is_excluded_path(rel_path, policy_exclusions=exclusions):
            continue
        yield path


def staged_paths(
    repo_root: Path, pattern: str | None = None, *, honor_policy: bool = True
) -> list[Path]:
    exclusions = policy_path_exclusions(repo_root) if honor_policy else ()
    command = [
        "git",
        "diff",
        "--cached",
        "--find-renames",
        "--name-only",
        "--diff-filter=ACMR",
    ]
    if pattern:
        command.extend(["--", pattern])
    result = subprocess.run(
        command, capture_output=True, text=True, cwd=repo_root, check=True
    )
    return [
        Path(line.strip())
        for line in result.stdout.splitlines()
        if line.strip()
        and not is_excluded_path(Path(line.strip()), policy_exclusions=exclusions)
    ]


def staged_renames(repo_root: Path) -> dict[Path, Path]:
    exclusions = policy_path_exclusions(repo_root)
    result = subprocess.run(
        [
            "git",
            "diff",
            "--cached",
            "--find-renames",
            "--name-status",
            "--diff-filter=R",
        ],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=True,
    )
    renames: dict[Path, Path] = {}
    for line in result.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) != _RENAME_STATUS_FIELDS:
            continue
        old_path, new_path = Path(parts[1]), Path(parts[2])
        if not is_excluded_path(
            old_path, policy_exclusions=exclusions
        ) and not is_excluded_path(new_path, policy_exclusions=exclusions):
            renames[old_path] = new_path
    return renames


def tracked_paths(repo_root: Path) -> list[Path]:
    exclusions = policy_path_exclusions(repo_root)
    result = subprocess.run(
        ["git", "ls-files"],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=True,
    )
    return [
        Path(line.strip())
        for line in result.stdout.splitlines()
        if line.strip()
        and not is_excluded_path(Path(line.strip()), policy_exclusions=exclusions)
    ]


def partially_staged_paths(repo_root: Path) -> list[Path]:
    """Files staged AND modified again in the worktree (the fully-staged rule)."""
    staged = {path.as_posix() for path in staged_paths(repo_root, honor_policy=False)}
    result = subprocess.run(
        ["git", "diff", "--name-only"],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=True,
    )
    unstaged = {line.strip() for line in result.stdout.splitlines() if line.strip()}
    return [Path(path) for path in sorted(staged & unstaged)]


def git_blob_text(repo_root: Path, ref: str, path: Path) -> str | None:
    result = subprocess.run(
        ["git", "show", f"{ref}:{path.as_posix()}"],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    return result.stdout if result.returncode == 0 else None
