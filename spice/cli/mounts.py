"""Mounted commands: repo-owned command paths unified under the spice namespace."""

from __future__ import annotations

import os
import re
import shlex
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.cli.parser import (
    BUILTIN_COMMANDS,
    CommandPathRegistration,
    command_path_registry,
)
from spice.errors import SpiceError
from spice.paths import repo_root_from_cwd
from spice.process.tool import run_parent_lifetime_command
from spice.config.layers import contextualize_config_error, effective_commands

MOUNT_SEGMENT_RE = re.compile(r"^[a-z][a-z0-9-]*$")
MOUNTED_COMMAND_ENV = "SPICE_MOUNTED_COMMAND"  # env-policy: allow
VISIBLE_PROG_ENV = "SPICE_VISIBLE_PROG"  # env-policy: allow
RUNTIME_PYTHON_ENV = "SPICE_RUNTIME_PYTHON"  # env-policy: allow


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
    try:
        return _mounted_commands(repo_root)
    except SpiceError as exc:
        raise contextualize_config_error(repo_root, exc, "commands") from exc


def _mounted_commands(repo_root: Path) -> dict[tuple[str, ...], tuple[str, ...]]:
    mounts: dict[tuple[str, ...], tuple[str, ...]] = {}
    command_paths = command_path_registry()
    for raw_name, raw_argv in effective_commands(repo_root).items():
        path = mount_command_path(str(raw_name))
        if len(path) == 1 and path[0] in BUILTIN_COMMANDS:
            raise SpiceError(
                f"[tool.spice.commands] entry {raw_name!r} shadows a built-in "
                "spice command; pick another name"
            )
        registration = command_paths.get(path)
        if registration is not None:
            raise _mount_shadow_error(str(raw_name), registration)
        mounts[path] = _mount_argv(str(raw_name), raw_argv)
    return mounts


def _mount_shadow_error(name: str, registration: CommandPathRegistration) -> SpiceError:
    action = "spice " + " ".join(registration.path)
    if registration.source == "extension":
        return SpiceError(
            f"[tool.spice.commands] entry {name!r} shadows extension-provided "
            f"spice action {action!r} from {registration.provider!r}; "
            "pick another name"
        )
    return SpiceError(
        f"[tool.spice.commands] entry {name!r} shadows built-in "
        f"spice action {action!r}; pick another name"
    )


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
    env = dict(os.environ)  # env-policy: allow
    env[MOUNTED_COMMAND_ENV] = "1"
    env[VISIBLE_PROG_ENV] = mount.visible_prog
    # The mounted child deliberately enters candidate checkout code. Preserve
    # the parent interpreter as the independently installed runtime identity so
    # release evidence can prove what ordinary fleet commands actually import.
    env[RUNTIME_PYTHON_ENV] = sys.executable
    result = run_parent_lifetime_command(
        [*mount.argv, *args], cwd=mount.repo_root, env=env, check=False
    )
    return result.returncode


def mounted_command_names() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    return sorted(".".join(path) for path in mounted_commands(repo_root))
