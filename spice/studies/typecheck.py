"""Pyright type-check lane for a project's own Python package roots.

The constitution's Python counterpart to the serve checkJs lane: it runs
pyright over exactly the package roots `shape` resolves (explicit
`[tool.spice.policy] package_roots`, else derived from the project's packaging
metadata), so it self-scopes like every other lane — a repo with no resolvable
package contributes nothing. The flags are fixed and opinionated; the only
seam is which roots the repo declares.
"""

from __future__ import annotations

import shlex
import subprocess
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import find_tool
from spice.studies.shape import configured_package_roots

# Fixed, opinionated: fail on type errors, in the repo's [tool.pyright] mode.
PYRIGHT_ARGS = (
    "--level",
    "error",
)


def python_typecheck_targets(repo_root: Path) -> tuple[str, ...]:
    """The package roots to type-check; empty when the repo declares none."""
    return tuple(
        root.relative_to(repo_root).as_posix()
        for root in configured_package_roots(repo_root)
    )


def python_typecheck_argv(targets: tuple[str, ...]) -> tuple[str, ...]:
    """`pyright <fixed args> <targets>`, preferring an installed pyright and
    falling back to `uvx pyright` so no dependency has to be vendored."""
    pyright = find_tool("pyright")
    if pyright:
        base: tuple[str, ...] = (pyright,)
    else:
        uvx = find_tool("uvx")
        if not uvx:
            raise SpiceError(
                "pyright is required for python typechecking; install pyright "
                "or uv, or run `spice dev doctor` for environment details"
            )
        base = (uvx, "pyright")
    return (*base, *PYRIGHT_ARGS, *targets)


def run_python_typecheck(repo_root: Path) -> None:
    targets = python_typecheck_targets(repo_root)
    if not targets:
        # The lane gates a repo's own package roots; a repo that declares or
        # derives none has nothing in this lane.
        return
    argv = python_typecheck_argv(targets)
    result = subprocess.run(
        list(argv),
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    if result.returncode == 0:
        return
    output = "\n".join(
        part for part in (result.stdout.strip(), result.stderr.strip()) if part
    )
    message = f"{shlex.join(argv)} exited {result.returncode}"
    if output:
        message += ":\n" + output
    raise SpiceError(message)
