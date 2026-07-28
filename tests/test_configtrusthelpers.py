"""Test setup for repositories whose executable configuration is trusted."""

from pathlib import Path

from spice.config.trust import plan_exact_repository_config_approval
from spice.hooks.initplan import (
    InitializationMode,
    apply_initialization_plan,
    plan_initialization,
)


def approve_repository_config(repo: Path) -> None:
    """Apply fixture gates and append the repository-config approval fact."""
    resolved = repo.expanduser().resolve()
    plan = plan_initialization(
        resolved,
        InitializationMode.GATES_ONLY,
        include_agent_skill=False,
    )
    apply_initialization_plan(
        plan,
        repository_config_approval=plan_exact_repository_config_approval(resolved),
    )
