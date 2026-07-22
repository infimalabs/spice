"""`spice dev pytest`: the checkout test runner behind the pytest wrapper word."""

from __future__ import annotations

import sys
from pathlib import Path

from spice.errors import SpiceError


def run_checkout_pytest(repo_root: Path, pytest_args: list[str]) -> int:
    """Run pytest in-process under the worktree venv with the checkout imported.

    The `dev` re-exec seam in `spice.cli.entry` has already switched this
    process onto ``<repo_root>/.venv/bin/python`` when that venv exists, and
    ``python -m spice`` binds the checkout `spice` package in ``sys.modules``
    before pytest plugin discovery — so an installed spice distribution cannot
    shadow changed sources. Any other interpreter is refused instead of
    falling back to an ambient PATH pytest, which may lack the declared dev
    dependency group entirely.
    """
    venv_root = (repo_root / ".venv").resolve()
    if Path(sys.prefix).resolve() != venv_root:
        raise SpiceError(
            "create the venv with `uv sync` and retry; spice dev pytest runs "
            f"under the worktree venv {venv_root}, but this interpreter is "
            f"{sys.executable}"
        )
    import pytest

    return int(pytest.main(list(pytest_args)))
