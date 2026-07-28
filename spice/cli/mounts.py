"""Mounted commands: repo-owned command paths unified under the spice namespace."""

from __future__ import annotations

import os
import re
import shlex
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.commandplan import (
    apply_mounted_plan,
    assert_plan_digest,
    parse_command_plan_document,
)
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


@dataclass(frozen=True)
class MountedCommandResolution:
    commands: dict[tuple[str, ...], tuple[str, ...]]
    refusals: tuple[str, ...]


def mounted_commands(repo_root: Path) -> dict[tuple[str, ...], tuple[str, ...]]:
    """The accepted mount table; path collisions are contained per entry."""
    return resolve_mounted_commands(repo_root).commands


def resolve_mounted_commands(repo_root: Path) -> MountedCommandResolution:
    """Resolve accepted mounts and source-aware path-collision refusals."""
    try:
        return _resolve_mounted_commands(repo_root)
    except SpiceError as exc:
        raise contextualize_config_error(repo_root, exc, "commands") from exc


def _resolve_mounted_commands(repo_root: Path) -> MountedCommandResolution:
    mounts: dict[tuple[str, ...], tuple[str, ...]] = {}
    refusals: list[str] = []
    command_paths = command_path_registry()
    for raw_name, raw_argv in effective_commands(repo_root).items():
        path = mount_command_path(str(raw_name))
        if len(path) == 1 and path[0] in BUILTIN_COMMANDS:
            refusal = SpiceError(
                f"[commands] entry {raw_name!r} shadows a built-in "
                "spice command; pick another name"
            )
            refusals.append(
                str(
                    contextualize_config_error(
                        repo_root, refusal, "commands", str(raw_name)
                    )
                )
            )
            continue
        registration = command_paths.get(path)
        if registration is not None:
            refusals.append(
                str(
                    contextualize_config_error(
                        repo_root,
                        _mount_shadow_error(str(raw_name), registration),
                        "commands",
                        str(raw_name),
                    )
                )
            )
            continue
        mounts[path] = _mount_argv(str(raw_name), raw_argv)
    return MountedCommandResolution(commands=mounts, refusals=tuple(refusals))


def _mount_shadow_error(name: str, registration: CommandPathRegistration) -> SpiceError:
    action = "spice " + " ".join(registration.path)
    if registration.source == "extension":
        return SpiceError(
            f"[commands] entry {name!r} shadows extension-provided "
            f"spice action {action!r} from {registration.provider!r}; "
            "pick another name"
        )
    return SpiceError(
        f"[commands] entry {name!r} shadows built-in "
        f"spice action {action!r}; pick another name"
    )


def mount_command_path(raw_name: str) -> tuple[str, ...]:
    parts = tuple(raw_name.split("."))
    if not parts or any(not MOUNT_SEGMENT_RE.fullmatch(part) for part in parts):
        raise SpiceError(
            f"[commands] entry {raw_name!r} must be dot-separated "
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
            f"[commands] entry {name!r} must be a command string "
            "or a list of argv strings"
        )
    if not argv:
        raise SpiceError(f"[commands] entry {name!r} is empty")
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
    requested, digest = _mounted_apply_request(args)
    if not requested:
        result = run_parent_lifetime_command(
            [*mount.argv, *args],
            cwd=mount.repo_root,
            env=env,
            check=False,
        )
        return result.returncode
    result = run_parent_lifetime_command(
        [*mount.argv, *args],
        cwd=mount.repo_root,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    stdout = str(getattr(result, "stdout", "") or "")
    stderr = str(getattr(result, "stderr", "") or "")
    if stderr:
        sys.stderr.write(stderr)
    if result.returncode != 0:
        if stdout:
            sys.stdout.write(stdout)
        return result.returncode
    document = parse_command_plan_document(stdout)
    if document is None:
        if stdout:
            sys.stdout.write(stdout)
        return result.returncode
    assert_plan_digest(document, digest)
    applied = apply_mounted_plan(document, mount.repo_root)
    print(f"applied command-plan digest={document.digest} operations={len(applied)}")
    for order, label in enumerate(applied, start=1):
        print(f"{order}. {label}")
    return result.returncode


def _mounted_apply_request(args: list[str]) -> tuple[bool, str | None]:
    requests = [
        argument.partition("=")
        for argument in args[: args.index("--") if "--" in args else len(args)]
        if argument == "--apply" or argument.startswith("--apply=")
    ]
    if len(requests) > 1:
        raise SpiceError("mounted command accepts --apply at most once")
    if not requests:
        return False, None
    argument, separator, digest = requests[0]
    _ = argument
    return True, digest if separator else None


def mounted_command_names() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    return sorted(".".join(path) for path in mounted_commands(repo_root))
