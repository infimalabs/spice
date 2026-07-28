"""Exact executable-config approval presentation for ``spice init``."""

from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from spice.commandplan import command_plan_payload
from spice.config.trust import (
    ExactRepositoryConfigApproval,
    record_planned_repository_config_approval,
    repository_trust_log_path,
    require_planned_repository_config_approval_current,
)


def initialization_command_plan_payload(
    *,
    repo_root: Path,
    mode: str,
    receipt_path: Path,
    operations: Sequence[Mapping[str, Any]],
    approval: ExactRepositoryConfigApproval | None,
) -> dict[str, Any]:
    """Build an init plan that binds any offered exact authority snapshot."""
    planned_operations = list(operations)
    if approval is not None:
        planned_operations.append(exact_approval_operation(repo_root, approval))
    return command_plan_payload(
        command="init",
        metadata={
            "repository": str(repo_root),
            "mode": mode,
            "receipt_path": str(receipt_path),
        },
        operations=planned_operations,
    )


def exact_approval_operation(
    repo_root: Path,
    approval: ExactRepositoryConfigApproval,
) -> dict[str, object]:
    """Bind the complete exact-approval snapshot into a command plan."""
    capability_digests = dict(approval.capability_digests)
    return {
        "kind": "repository-config-approval",
        "target": str(repository_trust_log_path(repo_root)),
        "scope": "common-git-state",
        "observed_before": {
            "aggregate_digest": approval.digest,
            "capability_digests": capability_digests,
        },
        "intended_after": {
            "authority": "exact",
            "aggregate_digest": approval.digest,
            "capability_digests": capability_digests,
        },
        "will_change": bool(capability_digests),
    }


def exact_approval_preview_row(
    repo_root: Path,
    approval: ExactRepositoryConfigApproval,
    *,
    order: int,
) -> str:
    """Render the exact shared authority an init apply would append."""
    capabilities = ",".join(
        f"{capability}:{digest}" for capability, digest in approval.capability_digests
    )
    return (
        f"{order}. repository-config-approval common-git-state "
        f"{repository_trust_log_path(repo_root)} digest={approval.digest} "
        f"capabilities={capabilities or '<none>'} state=authorize"
    )


def record_init_exact_approval(
    repo_root: Path,
    approval: ExactRepositoryConfigApproval,
) -> None:
    """Append the exact capability authority accepted through init."""
    record_planned_repository_config_approval(
        repo_root,
        approval,
        source="spice init --apply",
    )


def require_init_exact_approval_current(
    repo_root: Path,
    approval: ExactRepositoryConfigApproval,
) -> None:
    """Validate the exact approval snapshot before init mutates anything."""
    require_planned_repository_config_approval_current(repo_root, approval)
