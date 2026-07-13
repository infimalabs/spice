"""Generic repository metadata outside Spice's layered configuration.

Spice settings use :mod:`spice.configlayer`; this module reads the remaining
packaging and test metadata from the repository's ``pyproject.toml``.
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
