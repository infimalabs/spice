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

from spice.repocfg import policy_table, read_pyproject, string_list

_RENAME_STATUS_FIELDS = 3
TEST_PATHS_KEY = "test_paths"
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


def test_path_patterns(repo_root: Path) -> tuple[str, ...]:
    """Repo-relative test-root patterns, in configured derivation precedence."""
    policy_patterns = string_list(policy_table(repo_root).get(TEST_PATHS_KEY))
    if policy_patterns:
        return _normalized_patterns(policy_patterns)
    pytest_patterns = _pytest_testpaths(repo_root)
    if pytest_patterns:
        return _normalized_patterns(pytest_patterns)
    return ("tests",)


def configured_test_roots(repo_root: Path) -> list[Path]:
    """Existing concrete test directories for iterating callers."""
    roots: list[Path] = []
    for pattern in test_path_patterns(repo_root):
        roots.extend(_existing_test_roots(repo_root, pattern))
    return _dedupe_paths(roots)


def is_test_path(path: Path, repo_root: Path) -> bool:
    """True when ``path`` is at or below a configured test location."""
    relative = _repo_relative_path(path, repo_root)
    if relative is None:
        return False
    rel_posix = _normalized_git_path(relative)
    if not rel_posix:
        return False
    return any(
        _matches_test_path_pattern(rel_posix, pattern)
        for pattern in test_path_patterns(repo_root)
    )


def _pytest_testpaths(repo_root: Path) -> list[str]:
    tool = read_pyproject(repo_root).get("tool")
    if not isinstance(tool, dict):
        return []
    pytest_table = tool.get("pytest")
    if not isinstance(pytest_table, dict):
        return []
    options = pytest_table.get("ini_options")
    if not isinstance(options, dict):
        return []
    raw = options.get("testpaths")
    if isinstance(raw, str):
        return _string_testpaths(raw)
    return string_list(raw)


def _string_testpaths(raw: str) -> list[str]:
    values: list[str] = []
    for item in raw.split():
        value = item.strip()
        if value and value not in values:
            values.append(value)
    return values


def _normalized_patterns(patterns: Iterable[str]) -> tuple[str, ...]:
    normalized: list[str] = []
    for pattern in patterns:
        value = _normalized_policy_pattern(pattern).rstrip("/")
        if value and value not in normalized:
            normalized.append(value)
    return tuple(normalized)


def _existing_test_roots(repo_root: Path, pattern: str) -> list[Path]:
    if _has_glob_magic(pattern):
        return sorted(path for path in repo_root.glob(pattern) if path.is_dir())
    root = repo_root / pattern
    return [root] if root.is_dir() else []


def _dedupe_paths(paths: Iterable[Path]) -> list[Path]:
    deduped: list[Path] = []
    seen: set[Path] = set()
    for path in paths:
        resolved = path.resolve()
        if resolved in seen:
            continue
        seen.add(resolved)
        deduped.append(path)
    return deduped


def _repo_relative_path(path: Path, repo_root: Path) -> Path | None:
    if not path.is_absolute():
        return path
    try:
        return path.resolve().relative_to(repo_root.resolve())
    except ValueError:
        return None


def _matches_test_path_pattern(rel_posix: str, pattern: str) -> bool:
    if _has_glob_magic(pattern):
        return any(
            fnmatchcase(candidate, pattern) for candidate in _path_ancestors(rel_posix)
        )
    return rel_posix == pattern or rel_posix.startswith(pattern + "/")


def _path_ancestors(rel_posix: str) -> Iterator[str]:
    path = Path(rel_posix)
    candidates = [path, *path.parents]
    for candidate in candidates:
        value = candidate.as_posix()
        if value and value != ".":
            yield value


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


def changed_paths(
    repo_root: Path,
    baseline_ref: str,
    pattern: str | None = None,
    *,
    honor_policy: bool = True,
) -> list[Path]:
    exclusions = policy_path_exclusions(repo_root) if honor_policy else ()
    command = [
        "git",
        "diff",
        "--name-only",
        "--diff-filter=ACMR",
        baseline_ref,
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
