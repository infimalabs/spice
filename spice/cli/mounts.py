"""Mounted commands: a repo's own tools, unified under the spice namespace.

A project's custom tooling deserves one CLI without the harness owning it:
a target repo
declares `[tool.spice.commands]` in its tracked pyproject.toml and each
entry runs as `spice <name> …` with the remaining arguments passed through
verbatim — no argparse mangling between the operator and the tool. Entries
are a command string (shlex-split) or an argv list, executed from the repo
root. Built-in verbs always win; a mount that shadows one fails loudly.
"""

from __future__ import annotations

import re
import shlex
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.cli.parser import BUILTIN_COMMANDS
from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd
from spice.repocfg import commands_table

MOUNT_NAME_RE = re.compile(r"^[a-z][a-z0-9-]*$")


@dataclass(frozen=True)
class MountedCommand:
    name: str
    argv: tuple[str, ...]
    repo_root: Path


def mounted_commands(repo_root: Path) -> dict[str, tuple[str, ...]]:
    """The validated mount table; any malformed entry fails the whole read."""
    mounts: dict[str, tuple[str, ...]] = {}
    for raw_name, raw_argv in commands_table(repo_root).items():
        name = str(raw_name)
        if name in BUILTIN_COMMANDS:
            raise SpiceError(
                f"[tool.spice.commands] entry {name!r} shadows a built-in "
                "spice command; pick another name"
            )
        if not MOUNT_NAME_RE.fullmatch(name):
            raise SpiceError(
                f"[tool.spice.commands] entry {name!r} must match "
                f"{MOUNT_NAME_RE.pattern}"
            )
        mounts[name] = _mount_argv(name, raw_argv)
    return mounts


def _mount_argv(name: str, raw: Any) -> tuple[str, ...]:
    if isinstance(raw, str):
        argv = tuple(shlex.split(raw))
    elif isinstance(raw, list) and all(isinstance(item, str) for item in raw):
        argv = tuple(raw)
    else:
        raise SpiceError(
            f"[tool.spice.commands] entry {name!r} must be a command string "
            "or a list of argv strings"
        )
    if not argv:
        raise SpiceError(f"[tool.spice.commands] entry {name!r} is empty")
    return argv


def find_mounted_command(name: str) -> MountedCommand | None:
    """Resolve `name` to a mount, or None when built-ins/argparse should run.

    Built-in names short-circuit before any configuration is read, so the
    core command surface never pays for (or breaks on) a repo's mount table.
    """
    if name in BUILTIN_COMMANDS:
        return None
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return None
    argv = mounted_commands(repo_root).get(name)
    if argv is None:
        return None
    return MountedCommand(name=name, argv=argv, repo_root=repo_root)


def run_mounted_command(mount: MountedCommand, args: list[str]) -> int:
    result = subprocess.run([*mount.argv, *args], cwd=mount.repo_root, check=False)
    return result.returncode


def mounted_command_names() -> list[str]:
    """Raw declared names for help text; validation happens at dispatch."""
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    return sorted(str(name) for name in commands_table(repo_root))
