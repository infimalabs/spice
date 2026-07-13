"""Effective repository configuration from the canonical layered view.

Two kinds of configuration, two homes. Constitution parameters and task
vocabulary are *project truth* — they belong in tracked history, so every clone
and every agent sees the same opinions. Operator-local state (speech voice,
judge binary, personality, worktree agent overrides) is *worktree truth* and
lives in `.spice/config/`.

"""

from __future__ import annotations

import tomllib
from pathlib import Path
from typing import Any


def read_pyproject(repo_root: Path) -> dict[str, Any]:
    """The whole parsed `pyproject.toml`, or {} when missing/malformed."""
    pyproject = repo_root / "pyproject.toml"
    try:
        with pyproject.open("rb") as handle:
            loaded = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}
