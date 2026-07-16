"""Canonical matching for normalized repository-relative paths.

Repository selectors share one base contract: plain patterns select an exact
path and its subtree, while patterns containing glob magic use case-sensitive
``fnmatch`` semantics.  A leading ``**/`` may consume zero directories, so a
repository-wide suffix selector covers files at the root as well as below it.

Selectors that name directory roots use :func:`matches_repo_path_or_ancestor`;
it matches the path itself or any ancestor, so descendants of a glob-selected
directory stay in that selected tree.

The truth table is:

==================== ==================== ==================== =====
Matcher              Path                 Pattern              Match
==================== ==================== ==================== =====
base                 ``src/app.py``       ``src/app.py``       yes
base                 ``src/pkg/app.py``   ``src``              yes
base                 ``src/app.py``       ``src/*.py``         yes
base                 ``pkg/app.py``       ``**/*.py``          yes
base                 ``app.py``           ``**/*.py``          yes
base                 ``Docs/page.md``     ``Docs/**/*.md``     no
path or ancestor     ``A/B/T/file.py``    ``A/**/T``           yes
==================== ==================== ==================== =====

Both paths and patterns accept ``/`` or ``\\`` separators and an optional
leading ``./``.  Glob matching intentionally follows :mod:`fnmatch`: ``*`` can
cross separators because repository paths are matched as complete strings.
"""

from __future__ import annotations

from fnmatch import fnmatchcase
from pathlib import PurePath
from typing import NamedTuple


class PathSpecificity(NamedTuple):
    """Lexicographic precedence for repository path patterns."""

    priority: int
    literal_pattern: bool
    literal_characters: int
    segments: int
    pattern_length: int


def normalize_repo_path(value: str | PurePath) -> str:
    """Return the shared string form for a repository-relative path or pattern."""
    raw = value.as_posix() if isinstance(value, PurePath) else value
    return raw.strip().replace("\\", "/").removeprefix("./")


def has_glob_magic(pattern: str | PurePath) -> bool:
    """Return whether a normalized pattern contains supported glob syntax."""
    return any(char in normalize_repo_path(pattern) for char in "*?[")


def matches_repo_path(path: str | PurePath, pattern: str | PurePath) -> bool:
    """Match one path using canonical glob-or-subtree repository semantics."""
    normalized_path = normalize_repo_path(path)
    normalized_pattern = normalize_repo_path(pattern)
    if not normalized_path or not normalized_pattern:
        return False
    if has_glob_magic(normalized_pattern):
        return any(
            fnmatchcase(normalized_path, variant)
            for variant in _leading_double_star_variants(normalized_pattern)
        )
    prefix = normalized_pattern.rstrip("/")
    return normalized_path == prefix or normalized_path.startswith(prefix + "/")


def matches_repo_path_or_ancestor(
    path: str | PurePath, pattern: str | PurePath
) -> bool:
    """Match a path itself or an ancestor selected by a repository pattern."""
    normalized_path = normalize_repo_path(path)
    segments = normalized_path.split("/")
    return any(
        matches_repo_path("/".join(segments[:end]), pattern)
        for end in range(len(segments), 0, -1)
    )


def path_specificity(pattern: str | PurePath, *, priority: int = 0) -> PathSpecificity:
    """Return stable lexicographic precedence for a repository path pattern."""
    normalized_pattern = normalize_repo_path(pattern)
    literal_pattern = not has_glob_magic(normalized_pattern)
    literal_characters = sum(char not in "*?[]!" for char in normalized_pattern)
    segments = sum(bool(segment) for segment in normalized_pattern.split("/"))
    return PathSpecificity(
        priority,
        literal_pattern,
        literal_characters,
        segments,
        len(normalized_pattern),
    )


def _leading_double_star_variants(pattern: str) -> tuple[str, ...]:
    variants = [pattern]
    shortened = pattern
    while shortened.startswith("**/"):
        shortened = shortened[3:]
        variants.append(shortened)
    return tuple(variants)
