"""The `spice` executable: worktree switching, parsing, dispatch, exit codes."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd
from spice.toolprocess import run_parent_lifetime_command
from spice.worktrees import resolve_worktree_target

SIGINT_EXIT_CODE = 130

# Hook shims invoke the ambient `spice` on PATH, which for a `uv tool install`
# resolves to one fixed source checkout regardless of which worktree is being
# committed -- so `dev` gate backends (pre-commit, typechecks, ...) would read
# package-relative data (e.g. serve_web_js_targets) from a stale sibling
# instead of the worktree actually being operated on. Scoped to `dev` (the
# hook/gate backend namespace, not the hot interactive command surface like
# `agent run`) to keep the extra resolution check off paths run many times
# per session.
SELFEXEC_ENV = "SPICE_SELFEXEC_ROOT"  # env-policy: allow


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    argv, worktree_target = _extract_worktree_target(argv)
    if worktree_target:
        try:
            _switch_worktree(worktree_target)
        except RuntimeError as exc:
            print(f"spice: {exc}", file=sys.stderr)
            return 2
    if argv[:1] == ["dev"]:
        reexec_code = _reexec_dev_command_for_worktree_checkout(argv)
        if reexec_code is not None:
            return reexec_code
    try:
        return _dispatch(argv)
    except SpiceError as exc:
        print(f"spice: {exc}", file=sys.stderr)
        return 2
    except subprocess.CalledProcessError as exc:
        return int(exc.returncode)
    except KeyboardInterrupt:
        print("spice: interrupted", file=sys.stderr)
        return SIGINT_EXIT_CODE


def _dispatch(argv: list[str]) -> int:
    if argv[:2] == ["agent", "run"]:
        from spice.agent.wrap import run_agent_command

        return run_agent_command(repo_root_from_cwd(), argv[2:])

    if argv[:2] == ["dev", "pytest"]:
        # argparse.REMAINDER cannot start with a flag token, and pytest
        # invocations usually do (`pytest -q ...`), so this forwards ahead of
        # parsing — the same shape as `agent run` above.
        from spice.hooks.devpytest import run_checkout_pytest
        from spice.paths import require_repo_root

        return run_checkout_pytest(require_repo_root(), argv[2:])

    if argv and not argv[0].startswith("-"):
        from spice.cli.mounts import find_mounted_command, run_mounted_command

        resolved = find_mounted_command(argv)
        if resolved is not None:
            mount, remainder = resolved
            return run_mounted_command(mount, remainder)

    from spice.cli.parser import build_parser

    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.func(args))


def _extract_worktree_target(argv: list[str]) -> tuple[list[str], str | None]:
    try:
        index = argv.index("--worktree")
    except ValueError:
        return argv, None
    target_index = index + 1
    if target_index >= len(argv) or argv[target_index].startswith("-"):
        raise SystemExit("spice: --worktree requires a target")
    return [*argv[:index], *argv[target_index + 1 :]], argv[target_index]


def _switch_worktree(target: str) -> None:
    current = repo_root_from_cwd() or Path.cwd().resolve()
    resolved = resolve_worktree_target(target, cwd=current)
    if current.resolve() == resolved.resolve():
        return
    print(f"spice: worktree={current} -> {resolved}", file=sys.stderr)
    os.chdir(resolved)


def _worktree_local_python(repo_root: Path) -> Path | None:
    """The worktree's own venv interpreter, iff this worktree is a spice checkout.

    `spice/` is a namespace package (no `__init__.py`), so its presence is
    checked against this very file instead -- guaranteed to exist in any
    checkout that has this re-exec logic at all.
    """
    if not (repo_root / "spice" / "cli" / "entry.py").is_file():
        return None
    candidate = repo_root / ".venv" / "bin" / "python"
    return candidate if candidate.is_file() else None


def _reexec_dev_command_for_worktree_checkout(argv: list[str]) -> int | None:
    """Re-run a `dev` gate backend under the current worktree's own checkout.

    None means "run in-process as usual" (no local checkout found, or already
    running from it). An int is the exit code of the re-executed command.
    """
    if os.environ.get(SELFEXEC_ENV):  # env-policy: allow
        return None
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return None
    python = _worktree_local_python(repo_root)
    if python is None:
        return None
    # Compare venv roots, not resolved interpreter binaries: a venv's own
    # `python` is commonly a symlink to one shared system interpreter, so two
    # different worktrees' venvs can resolve to the identical binary path --
    # `sys.prefix` is the venv root itself and does not collapse that way.
    if Path(sys.prefix).resolve() == (repo_root / ".venv").resolve():
        return None
    env = dict(os.environ)  # env-policy: allow
    env[SELFEXEC_ENV] = str(repo_root)
    result = run_parent_lifetime_command(
        [str(python), "-m", "spice", *argv], cwd=repo_root, env=env, check=False
    )
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
