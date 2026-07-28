"""Digest-bound execution ownership for mounted command plans."""

from __future__ import annotations

from collections.abc import Mapping, MutableMapping
from typing import Any

from spice.commandplan import (
    CommandPlanDocument,
    assert_plan_digest,
    plan_document,
)
from spice.errors import SpiceError

MOUNTED_COMMAND_ENV = "SPICE_MOUNTED_COMMAND"  # env-policy: allow
COMMAND_PLAN_EXECUTION_DIGEST_ENV = (
    "SPICE_COMMAND_PLAN_EXECUTION_DIGEST"  # env-policy: allow
)
SPICE_PLAN_EXECUTOR = "spice"
COMMAND_PLAN_EXECUTOR = "command"
PLAN_EXECUTORS = frozenset({SPICE_PLAN_EXECUTOR, COMMAND_PLAN_EXECUTOR})


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
) -> bool:
    """Separate a mounted command's planning pass from its authorized execution."""
    if not apply_requested or environ.get(MOUNTED_COMMAND_ENV) != "1":
        return False
    document = plan_document(payload)
    if command_plan_executor(document) != COMMAND_PLAN_EXECUTOR:
        return False
    if COMMAND_PLAN_EXECUTION_DIGEST_ENV not in environ:
        # A pre-ownership parent cannot understand the command executor. Keep
        # the former single command-owned apply path so the first compatible
        # release can bootstrap the new parent rather than deadlock publication.
        return False
    execution_digest = environ.get(COMMAND_PLAN_EXECUTION_DIGEST_ENV)
    if not execution_digest:
        return True
    assert_plan_digest(document, execution_digest)
    del environ[COMMAND_PLAN_EXECUTION_DIGEST_ENV]
    return False


def _operation_executor(operation: Mapping[str, Any], order: int) -> str:
    executor = operation.get("executor", SPICE_PLAN_EXECUTOR)
    if not isinstance(executor, str) or executor not in PLAN_EXECUTORS:
        raise SpiceError(
            f"mounted command plan operation {order} has unsupported executor "
            f"{executor!r}; expected one of {', '.join(sorted(PLAN_EXECUTORS))}"
        )
    return executor
