"""Immutable operation and receipt models for repository initialization."""

from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path


class InitializationMode(StrEnum):
    """The operator-visible initialization surface that produced a plan."""

    FULL = "full"
    GATES_ONLY = "gates-only"


class InitOperationKind(StrEnum):
    FILE = "file"
    GIT_CONFIG = "git-config"


class InitOperationScope(StrEnum):
    WORKTREE_FILE = "worktree-file"
    COMMON_GIT_CONFIG = "common-git-config"
    WORKTREE_GIT_CONFIG = "worktree-git-config"


class InitReceiptStatus(StrEnum):
    APPLYING = "applying"
    COMPLETE = "complete"
    DEINITIALIZING = "deinitializing"


class InitReceiptEvent(StrEnum):
    """One durable fact in the initialization ownership log."""

    APPLY = "apply"
    UNAPPLY = "unapply"
    TRANSFER = "transfer"
    APPROVAL = "approval"


@dataclass(frozen=True)
class InitOperation:
    """One fully resolved initialization mutation and its prior provenance."""

    kind: InitOperationKind
    target: str
    scope: InitOperationScope
    scope_path: Path
    previous_value: str | None
    generated_value: str
    previous_mode: int | None
    generated_mode: int | None
    ownership_digest: str
    initialization_mode: InitializationMode
    introduced: bool
    managed: bool = True
    previous_effective_value: str | None = None
    introduced_parent_directories: tuple[str, ...] = ()
    introduced_scope_path: bool = False

    @property
    def will_change(self) -> bool:
        if not self.managed:
            return False
        return (
            self.previous_value != self.generated_value
            or self.previous_mode != self.generated_mode
        )


@dataclass(frozen=True)
class InitializationPlan:
    """A stable, ordered model consumed unchanged by preview and apply."""

    repo_root: Path
    mode: InitializationMode
    operations: tuple[InitOperation, ...]
    schema_version: int = 1


@dataclass(frozen=True)
class InitReceiptOperation:
    """One planned operation plus its durably acknowledged apply state."""

    operation: InitOperation
    completed: bool


@dataclass(frozen=True)
class InitializationReceipt:
    """Machine-local provenance for resumable and reversible initialization."""

    repo_root: Path
    mode: InitializationMode
    plan_schema_version: int
    status: InitReceiptStatus
    operations: tuple[InitReceiptOperation, ...]
    approved_repository_config_digest: str | None = None
    schema_version: int = 1


@dataclass(frozen=True)
class InitializationReceiptRecord:
    """One complete append-only fact over the shared plan operation vocabulary."""

    repo_root: Path
    mode: InitializationMode
    plan_schema_version: int
    event: InitReceiptEvent
    operation_index: int
    operation_count: int
    operation: InitOperation
    outcome: str | None = None
    observed_kind: str | None = None
    observed_value: str | None = None
    observed_mode: int | None = None
    observed_sha256: str | None = None
    shared_owner: str | None = None
    approved_repository_config_digest: str | None = None
    schema_version: int = 1
