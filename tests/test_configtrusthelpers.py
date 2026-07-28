"""Test setup for repositories whose executable configuration is trusted."""

from pathlib import Path

from spice.config.trust import repository_executable_config_digest
from spice.hooks.initplan import (
    InitializationMode,
    InitializationReceipt,
    InitReceiptStatus,
    write_initialization_receipt,
)


def approve_repository_config(repo: Path) -> None:
    """Write the completed approval fact without installing fixture hooks."""
    resolved = repo.expanduser().resolve()
    write_initialization_receipt(
        InitializationReceipt(
            repo_root=resolved,
            mode=InitializationMode.GATES_ONLY,
            plan_schema_version=1,
            status=InitReceiptStatus.COMPLETE,
            operations=(),
            approved_repository_config_digest=(
                repository_executable_config_digest(resolved)
            ),
        )
    )
