"""Shared execution and hashing primitives for release rehearsals."""

from __future__ import annotations

import hashlib
import os
import shlex
import subprocess
import sys
from pathlib import Path

from evidence import FailureArtifactStore, redact_text

HASH_CHUNK_BYTES = 1024 * 1024


class RehearsalError(RuntimeError):
    """The release proof could not establish one required invariant."""


def run(
    command: list[str] | tuple[str, ...],
    *,
    cwd: Path,
    capture: bool = False,
    env: dict[str, str] | None = None,
    failures: FailureArtifactStore | None = None,
    gate: str = "command",
) -> subprocess.CompletedProcess[str]:
    argv = [str(part) for part in command]
    print(f"+ {shlex.join(argv)}", flush=True)
    completed = subprocess.run(
        argv,
        check=False,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        environment = (
            env if env is not None else dict(os.environ)  # env-policy: allow
        )
        diagnostic = (
            failures.record(
                gate,
                argv,
                completed.returncode,
                completed.stdout,
                completed.stderr,
                environment=environment,
            )
            if failures is not None
            else None
        )
        safe_stdout = redact_text(completed.stdout, environment).strip()
        safe_stderr = redact_text(completed.stderr, environment).strip()
        if safe_stdout:
            print(safe_stdout, file=sys.stderr)
        if safe_stderr:
            print(safe_stderr, file=sys.stderr)
        suffix = f"; diagnostic={diagnostic}" if diagnostic is not None else ""
        raise RehearsalError(
            f"{gate} failed with exit code {completed.returncode}{suffix}"
        )
    if not capture and completed.stdout:
        print(completed.stdout, end="")
    if completed.stderr:
        print(completed.stderr, end="", file=sys.stderr)
    return completed


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(HASH_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def isolated_environment() -> dict[str, str]:
    environment = dict(os.environ)  # env-policy: allow
    for name in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV"):
        environment.pop(name, None)
    return environment
