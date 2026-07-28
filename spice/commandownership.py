"""Digest-bound execution ownership for mounted command plans."""

from __future__ import annotations

from collections.abc import Mapping, MutableMapping
from pathlib import Path
from typing import Any

from spice.commandplan import (
    CommandPlanDocument,
    assert_plan_digest,
    plan_document,
)
from spice.errors import SpiceError
from spice.process.tool import run_tool_command

MOUNTED_COMMAND_ENV = "SPICE_MOUNTED_COMMAND"  # env-policy: allow
MOUNTED_RUNTIME_PYTHON_ENV = "SPICE_RUNTIME_PYTHON"  # env-policy: allow
COMMAND_PLAN_EXECUTION_DIGEST_ENV = (
    "SPICE_COMMAND_PLAN_EXECUTION_DIGEST"  # env-policy: allow
)
SPICE_PLAN_EXECUTOR = "spice"
COMMAND_PLAN_EXECUTOR = "command"
PLAN_EXECUTORS = frozenset({SPICE_PLAN_EXECUTOR, COMMAND_PLAN_EXECUTOR})
LEGACY_COMMAND_OWNER_PARENT_VERSION = "0.30.1"
LEGACY_COMMAND_OWNER_CANDIDATE_VERSION = "0.30.2"


def command_plan_executor(document: CommandPlanDocument) -> str:
    """Return the one executor named by every operation in a plan."""
    executors = {
        _operation_executor(operation, order)
        for order, operation in enumerate(document.operations, start=1)
    }
    if len(executors) > 1:
        raise SpiceError(
            "mounted command plan mixes operation executors: "
            + ", ".join(sorted(executors))
        )
    return next(iter(executors), SPICE_PLAN_EXECUTOR)


def assert_command_owned_plan_digest(
    document: CommandPlanDocument,
    expected_digest: str | None,
) -> None:
    """Require explicit authority before a mounted command executes its plan."""
    if expected_digest is None:
        raise SpiceError(
            "command-owned mounted command plan requires "
            f"--apply={document.digest}; the command, not Spice, owns its effects"
        )
    assert_plan_digest(document, expected_digest)


def defer_command_owned_apply(
    payload: Mapping[str, Any],
    *,
    apply_requested: bool,
    environ: MutableMapping[str, str],
    candidate_version: str,
    legacy_parent_version: str | None = None,
) -> bool:
    """Separate a mounted command's planning pass from its authorized execution."""
    if not apply_requested or environ.get(MOUNTED_COMMAND_ENV) != "1":
        return False
    document = plan_document(payload)
    if command_plan_executor(document) != COMMAND_PLAN_EXECUTOR:
        return False
    if COMMAND_PLAN_EXECUTION_DIGEST_ENV not in environ:
        parent_version = legacy_parent_version or _mounted_parent_version(environ)
        if (
            parent_version == LEGACY_COMMAND_OWNER_PARENT_VERSION
            and candidate_version == LEGACY_COMMAND_OWNER_CANDIDATE_VERSION
        ):
            return False
        raise SpiceError(
            "mounted parent does not advertise command-plan ownership; "
            "the only supported forward bootstrap is parent "
            f"{LEGACY_COMMAND_OWNER_PARENT_VERSION} publishing candidate "
            f"{LEGACY_COMMAND_OWNER_CANDIDATE_VERSION}, observed parent "
            f"{parent_version!r} and candidate {candidate_version!r}"
        )
    execution_digest = environ.get(COMMAND_PLAN_EXECUTION_DIGEST_ENV)
    if not execution_digest:
        return True
    assert_plan_digest(document, execution_digest)
    del environ[COMMAND_PLAN_EXECUTION_DIGEST_ENV]
    return False


def _mounted_parent_version(environ: Mapping[str, str]) -> str:
    python = environ.get(MOUNTED_RUNTIME_PYTHON_ENV)
    if not python:
        raise SpiceError(
            "mounted parent omitted both command-plan ownership and its runtime "
            "Python identity; refusing command-owned effects"
        )
    result = run_tool_command(
        [
            python,
            "-I",
            "-c",
            "from importlib.metadata import version; print(version('spice-harness'))",
        ],
        policy="release",
        operation="identify the pre-ownership mounted parent version",
        cwd=Path("/"),
        capture_output=True,
        text=True,
        check=False,
    )
    version = result.stdout.strip()
    if result.returncode != 0 or not version:
        detail = result.stderr.strip() or f"exit {result.returncode}"
        raise SpiceError(
            f"could not identify the pre-ownership mounted parent version: {detail}"
        )
    return version


def _operation_executor(operation: Mapping[str, Any], order: int) -> str:
    executor = operation.get("executor", SPICE_PLAN_EXECUTOR)
    if not isinstance(executor, str) or executor not in PLAN_EXECUTORS:
        raise SpiceError(
            f"mounted command plan operation {order} has unsupported executor "
            f"{executor!r}; expected one of {', '.join(sorted(PLAN_EXECUTORS))}"
        )
    return executor
