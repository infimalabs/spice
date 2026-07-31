"""`spice dev pytest`: the checkout test runner behind the pytest wrapper word."""

from __future__ import annotations

import sys
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
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

    args = list(pytest_args)
    exit_code = int(pytest.main(args))
    if exit_code != int(pytest.ExitCode.NO_TESTS_COLLECTED):
        return exit_code

    # xdist can collapse a missing path or node id into "no tests ran" and
    # exit 5. Let pytest parse the same arguments without distribution and
    # capture a collection-only diagnostic pass; replay it only when pytest
    # itself classifies the selector as a usage error. A legitimate empty
    # selection therefore keeps the original output and exit code.
    diagnostic_stdout = StringIO()
    diagnostic_stderr = StringIO()
    with (
        redirect_stdout(diagnostic_stdout),
        redirect_stderr(diagnostic_stderr),
    ):
        diagnostic_exit_code = int(pytest.main([*args, "-n", "0", "--collect-only"]))
    if diagnostic_exit_code != int(pytest.ExitCode.USAGE_ERROR):
        return exit_code

    sys.stderr.write(diagnostic_stderr.getvalue())
    sys.stdout.write(diagnostic_stdout.getvalue())
    return diagnostic_exit_code
