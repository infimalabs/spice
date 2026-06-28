"""Pyright type-check lane for a project's own Python package roots.

The constitution's Python counterpart to the serve checkJs lane: it runs
pyright over exactly the package roots `shape` resolves (explicit
`[tool.spice.policy] package_roots`, else derived from the project's packaging
metadata), so it self-scopes like every other lane — a repo with no resolvable
package contributes nothing. The flags are fixed and opinionated; the only
seams are which roots the repo declares and, for non-standard virtualenv
layouts, the Python interpreter pyright should resolve against.
"""

from __future__ import annotations

import os
import shlex
import subprocess
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import find_tool
from spice.repocfg import policy_table, read_pyproject
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

    active = _repo_local_virtual_env(repo_root)
    if active is not None:
        return _required_venv_python(active, "VIRTUAL_ENV")

    local = repo_root / ".venv"
    if local.exists():
        return _required_venv_python(local, ".venv")

    return _uv_project_interpreter(repo_root)


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
                "pyright is required for python typechecking; install pyright "
                "or uv, or run `spice dev doctor` for environment details"
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


def _configured_typecheck_interpreter(repo_root: Path) -> Path | None:
    raw = policy_table(repo_root).get(PYTHON_TYPECHECK_INTERPRETER_KEY)
    if raw is None:
        return None
    if not isinstance(raw, str) or not raw.strip():
        raise SpiceError(
            f"[tool.spice.policy] {PYTHON_TYPECHECK_INTERPRETER_KEY} must be a "
            "non-empty Python interpreter path"
        )
    path = Path(raw.strip()).expanduser()
    if not path.is_absolute():
        path = repo_root / path
    return _required_python(path, PYTHON_TYPECHECK_INTERPRETER_KEY)


def _repo_local_virtual_env(repo_root: Path) -> Path | None:
    raw = os.environ.get("VIRTUAL_ENV")  # env-policy: allow
    if not raw:
        return None
    venv = Path(raw).expanduser()
    resolved_venv = venv.resolve()
    resolved_root = repo_root.resolve()
    if resolved_venv == resolved_root or resolved_root in resolved_venv.parents:
        return resolved_venv
    return None


def _required_venv_python(venv: Path, source: str) -> Path:
    candidates = (
        venv / "bin" / "python",
        venv / "bin" / "python3",
        venv / "Scripts" / "python.exe",
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    raise SpiceError(
        f"python-typecheck {source} exists but has no Python interpreter at "
        f"{venv / 'bin' / 'python'}"
    )


def _uv_project_interpreter(repo_root: Path) -> Path | None:
    if not _uv_project_configured(repo_root):
        return None
    uv = find_tool("uv")
    if not uv:
        raise SpiceError(
            "python-typecheck detected a uv-managed project but uv is not installed"
        )
    result = subprocess.run(
        [
            uv,
            "run",
            "--directory",
            str(repo_root),
            "--project",
            str(repo_root),
            "--no-sync",
            "python",
            "-c",
            "import sys; print(sys.executable)",
        ],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    if result.returncode != 0:
        output = "\n".join(
            part for part in (result.stdout.strip(), result.stderr.strip()) if part
        )
        message = "python-typecheck failed to resolve the uv project interpreter"
        if output:
            message += ":\n" + output
        raise SpiceError(message)
    resolved = result.stdout.strip()
    if not resolved:
        raise SpiceError("python-typecheck uv project interpreter resolution was empty")
    return _required_python(Path(resolved), "uv project interpreter")


def _uv_project_configured(repo_root: Path) -> bool:
    if (repo_root / "uv.lock").is_file():
        return True
    tool = read_pyproject(repo_root).get("tool")
    return isinstance(tool, dict) and isinstance(tool.get("uv"), dict)


def _required_python(path: Path, source: str) -> Path:
    if path.is_file():
        return path
    raise SpiceError(f"python-typecheck {source} does not exist: {path}")
