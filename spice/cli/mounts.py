"""Mounted commands: repo-owned command paths unified under the spice namespace."""

from __future__ import annotations

import os
import re
import shlex
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.cli.parser import BUILTIN_COMMANDS
from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd, worktree_spice_environment
from spice.repocfg import commands_table

MOUNT_SEGMENT_RE = re.compile(r"^[a-z][a-z0-9-]*$")
MOUNTED_COMMAND_ENV = "SPICE_MOUNTED_COMMAND"  # env-policy: allow
VISIBLE_PROG_ENV = "SPICE_VISIBLE_PROG"  # env-policy: allow


@dataclass(frozen=True)
class MountedCommand:
    path: tuple[str, ...]
    argv: tuple[str, ...]
    repo_root: Path

    @property
    def name(self) -> str:
        return ".".join(self.path)

    @property
    def visible_prog(self) -> str:
        return "spice " + " ".join(self.path)


def mounted_commands(repo_root: Path) -> dict[tuple[str, ...], tuple[str, ...]]:
    """The validated mount table; any malformed entry fails the whole read."""
    mounts: dict[tuple[str, ...], tuple[str, ...]] = {}
    for raw_name, raw_argv in commands_table(repo_root).items():
        path = mount_command_path(str(raw_name))
        if len(path) == 1 and path[0] in BUILTIN_COMMANDS:
            raise SpiceError(
                f"[tool.spice.commands] entry {raw_name!r} shadows a built-in "
                "spice command; pick another name"
            )
        mounts[path] = _mount_argv(str(raw_name), raw_argv)
    return mounts


def mount_command_path(raw_name: str) -> tuple[str, ...]:
    parts = tuple(raw_name.split("."))
    if not parts or any(not MOUNT_SEGMENT_RE.fullmatch(part) for part in parts):
        raise SpiceError(
            f"[tool.spice.commands] entry {raw_name!r} must be dot-separated "
            f"segments matching {MOUNT_SEGMENT_RE.pattern}"
        )
    return parts


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


def find_mounted_command(argv: list[str]) -> tuple[MountedCommand, list[str]] | None:
    """Resolve the longest mounted command path from argv, or None."""
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return None
    mounts = mounted_commands(repo_root)
    if not mounts:
        return None
    best_path: tuple[str, ...] | None = None
    for path in mounts:
        if len(path) > len(argv):
            continue
        if tuple(argv[: len(path)]) != path:
            continue
        if best_path is None or len(path) > len(best_path):
            best_path = path
    if best_path is None:
        return None
    mount = MountedCommand(path=best_path, argv=mounts[best_path], repo_root=repo_root)
    return mount, argv[len(best_path) :]


def run_mounted_command(mount: MountedCommand, args: list[str]) -> int:
    env = worktree_spice_environment(mount.repo_root, base_env=os.environ)
    env[MOUNTED_COMMAND_ENV] = "1"
    env[VISIBLE_PROG_ENV] = mount.visible_prog
    result = subprocess.run(
        [*mount.argv, *args], cwd=mount.repo_root, env=env, check=False
    )
    return result.returncode


def mounted_command_names() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    return sorted(".".join(path) for path in mounted_commands(repo_root))
