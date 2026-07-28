"""Pyright type-check lane for a project's own Python package roots.

The constitution's Python counterpart to the serve checkJs lane: it runs
pyright over exactly the package roots `shape` resolves (explicit
`[policy] package_roots`, else derived from the project's packaging
metadata), so it self-scopes like every other lane — a repo with no resolvable
package contributes nothing. The flags are fixed and opinionated; the only
seams are which roots the repo declares and, for non-standard virtualenv
layouts, the Python interpreter pyright should resolve against.
"""

from __future__ import annotations

import shlex
from pathlib import Path

from spice.config.layers import effective_table
from spice.config.trust import require_repository_config_approval
from spice.errors import SpiceError
from spice.paths import find_tool
from spice.process.tool import run_typecheck_command
from spice.studies.pythonruntime import (
    project_python_interpreter,
    required_python_interpreter,
)
from spice.studies.shape import configured_package_roots

# Fixed, opinionated: fail on type errors, in the repo's [tool.pyright] mode.
PYRIGHT_ARGS = (
    "--level",
    "error",
)
PYTHON_TYPECHECK_INTERPRETER_KEY = "python_typecheck_interpreter"


def python_typecheck_targets(repo_root: Path) -> tuple[str, ...]:
    """The package roots to type-check; empty when the repo declares none."""
    return tuple(
        root.relative_to(repo_root).as_posix()
        for root in configured_package_roots(repo_root)
    )


def python_typecheck_interpreter(repo_root: Path) -> Path | None:
    """The target-repo Python interpreter pyright should resolve imports with."""
    configured = _configured_typecheck_interpreter(repo_root)
    if configured is not None:
        return configured

    return project_python_interpreter(repo_root)


def python_typecheck_argv(repo_root: Path, targets: tuple[str, ...]) -> tuple[str, ...]:
    """`pyright <fixed args> <targets>`, preferring an installed pyright and
    falling back to `uvx pyright` so no dependency has to be vendored."""
    pyright = find_tool("pyright")
    if pyright:
        base: tuple[str, ...] = (pyright,)
    else:
        uvx = find_tool("uvx")
        if not uvx:
            raise SpiceError(
                "install pyright or uv, or run `spice dev doctor` for "
                "environment details; pyright is required for python "
                "typechecking"
            )
        base = (uvx, "pyright")
    interpreter = python_typecheck_interpreter(repo_root)
    pythonpath = ("--pythonpath", str(interpreter)) if interpreter is not None else ()
    return (*base, *PYRIGHT_ARGS, *pythonpath, *targets)


def run_python_typecheck(repo_root: Path) -> None:
    targets = python_typecheck_targets(repo_root)
    if not targets:
        # The lane gates a repo's own package roots; a repo that declares or
        # derives none has nothing in this lane.
        return
    argv = python_typecheck_argv(repo_root, targets)
    require_repository_config_approval(
        repo_root,
        ("policy", PYTHON_TYPECHECK_INTERPRETER_KEY),
        command=shlex.join(argv),
    )
    run_typecheck_command(
        argv,
        operation="run Python typecheck",
        cwd=repo_root,
    )


def _configured_typecheck_interpreter(repo_root: Path) -> Path | None:
    raw = effective_table(repo_root, "policy").get(PYTHON_TYPECHECK_INTERPRETER_KEY)
    if raw is None:
        return None
    if not isinstance(raw, str) or not raw.strip():
        raise SpiceError(
            f"[policy] {PYTHON_TYPECHECK_INTERPRETER_KEY} must be a "
            "non-empty Python interpreter path"
        )
    path = Path(raw.strip()).expanduser()
    if not path.is_absolute():
        path = repo_root / path
    return required_python_interpreter(path, PYTHON_TYPECHECK_INTERPRETER_KEY)
