"""The `spice` executable: worktree switching, parsing, dispatch, exit codes."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import (
    repo_root_from_cwd,
    runtime_uses_worktree_spice,
    worktree_spice_environment,
    worktree_spice_python_command,
    worktree_spice_source,
)
from spice.worktrees import resolve_worktree_target

SIGINT_EXIT_CODE = 130


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    argv, worktree_target = _extract_worktree_target(argv)
    if worktree_target:
        try:
            _switch_worktree(worktree_target)
        except RuntimeError as exc:
            print(f"spice: {exc}", file=sys.stderr)
            return 2
    _reexec_worktree_spice_if_needed(argv)

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


def _reexec_worktree_spice_if_needed(argv: list[str]) -> None:
    repo_root = repo_root_from_cwd()
    if worktree_spice_source(repo_root) is None:
        return
    if runtime_uses_worktree_spice(repo_root):
        return
    command = worktree_spice_python_command(repo_root, argv)
    if command is None:
        return
    os.execvpe(command[0], command, worktree_spice_environment(repo_root))


if __name__ == "__main__":
    raise SystemExit(main())
