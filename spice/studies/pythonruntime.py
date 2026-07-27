"""The bound project's own Python interpreter, for lanes that need its dev deps.

Spice is routinely launched by an installed harness interpreter that
deliberately carries no repository development dependencies, so the process
running spice is not the process able to run the project's tools. Every lane
that needs pytest, pyright, or any other declared development dependency
resolves the bound checkout's interpreter through this one policy: an activated
repo-local virtualenv, else the checkout's own ``.venv``, else the interpreter a
uv-managed project declares.

Ambient PATH is not in that order and never becomes an answer. A project
declaring none of these has no test runtime here, and each caller says so in
its own terms rather than substituting whatever happens to be reachable.
"""

from __future__ import annotations

import os
from pathlib import Path

from spice.config.pyproject import read_pyproject
from spice.errors import SpiceError
from spice.paths import find_tool
from spice.process.tool import run_tool_command


def project_python_interpreter(repo_root: Path) -> Path | None:
    """The checkout's declared interpreter, or None when it declares none."""
    active = _repo_local_virtual_env(repo_root)
    if active is not None:
        return _required_venv_python(active, "VIRTUAL_ENV")

    local = repo_root / ".venv"
    if local.exists():
        return _required_venv_python(local, ".venv")

    return _uv_project_interpreter(repo_root)


def required_python_interpreter(path: Path, source: str) -> Path:
    """The interpreter at ``path``, or a refusal naming which source declared it."""
    if path.is_file():
        return path
    raise SpiceError(f"{source} does not exist: {path}")


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
        f"{source} exists but has no Python interpreter at {venv / 'bin' / 'python'}"
    )


def _uv_project_interpreter(repo_root: Path) -> Path | None:
    if not _uv_project_configured(repo_root):
        return None
    uv = find_tool("uv")
    if not uv:
        raise SpiceError("detected a uv-managed project but uv is not installed")
    # The typecheck lane has owned this resolution since it was written, and its
    # deadline class still bounds it; the interpreter is the same one either
    # lane asks for, so the wait it is allowed to take is the same too.
    result = run_tool_command(
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
        policy="typecheck",
        operation="resolve uv project interpreter",
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    if result.returncode != 0:
        output = "\n".join(
            part for part in (result.stdout.strip(), result.stderr.strip()) if part
        )
        message = "failed to resolve the uv project interpreter"
        if output:
            message += ":\n" + output
        raise SpiceError(message)
    resolved = result.stdout.strip()
    if not resolved:
        raise SpiceError("uv project interpreter resolution was empty")
    return required_python_interpreter(Path(resolved), "uv project interpreter")


def _uv_project_configured(repo_root: Path) -> bool:
    if (repo_root / "uv.lock").is_file():
        return True
    tool = read_pyproject(repo_root).get("tool")
    return isinstance(tool, dict) and isinstance(tool.get("uv"), dict)
