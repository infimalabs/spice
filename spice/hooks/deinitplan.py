"""Ownership-safe, resumable reversal of ``spice init`` receipts."""

from __future__ import annotations

import errno
import hashlib
import json
import os
import stat
from dataclasses import dataclass, replace
from enum import StrEnum
from pathlib import Path
from typing import Literal, TypedDict

from spice.errors import SpiceError
from spice.hooks.initplan import (
    INIT_RECEIPT_MODE,
    DEINIT_RECEIPT_FILENAME,
    InitOperation,
    InitOperationKind,
    InitOperationScope,
    InitReceiptStatus,
    InitializationReceipt,
    initialization_receipt_from_payload,
    initialization_receipt_digest,
    initialization_receipt_path,
    initialization_receipt_payload,
    git_config_file_get,
    load_initialization_receipt,
    write_initialization_receipt,
)
from spice.paths import atomic_write_text, git_dir
from spice.process.git import run_git_command

DEINIT_SCHEMA_VERSION = 1
RECOVERY_DIRNAME = "spice-deinit-recovery"
RECOVERY_DIGEST_LENGTH = 16
HASH_CHUNK_BYTES = 1024 * 1024


class DeinitReceiptStatus(StrEnum):
    REVERSING = "reversing"
    COMPLETE = "complete"


class DeinitOutcome(StrEnum):
    RESTORED = "restored"
    ALREADY_RESTORED = "already-restored"
    RETAINED_DIVERGED = "retained-diverged"
    RETAINED_SHARED = "retained-shared"
    PRESERVED_UNMANAGED = "preserved-unmanaged"


RETAINED_OUTCOMES = frozenset(
    {DeinitOutcome.RETAINED_DIVERGED, DeinitOutcome.RETAINED_SHARED}
)


@dataclass(frozen=True)
class DeinitOperationState:
    initialization_index: int
    completed: bool = False
    outcome: DeinitOutcome | None = None
    observed_kind: str | None = None
    observed_value: str | None = None
    observed_mode: int | None = None
    observed_sha256: str | None = None
    shared_owner: str | None = None


@dataclass(frozen=True)
class DeinitializationReceipt:
    repo_root: Path
    initialization: InitializationReceipt
    status: DeinitReceiptStatus
    operations: tuple[DeinitOperationState, ...]
    schema_version: int = DEINIT_SCHEMA_VERSION


@dataclass(frozen=True)
class FileObservation:
    kind: str
    content: bytes | None
    mode: int | None
    sha256: str | None


@dataclass(frozen=True)
class DeinitializationPlan:
    """One side-effect-free, ordered prediction of receipt reversal."""

    repo_root: Path
    reversal: DeinitializationReceipt | None
    receipt_digest: str | None
    operations: tuple[DeinitOperationState, ...]
    schema_version: int = DEINIT_SCHEMA_VERSION


class DeinitializationReportOperation(TypedDict):
    order: int
    kind: str
    target: str
    scope: str
    outcome: str | None
    observed_kind: str | None
    observed_value: str | None
    observed_mode: int | None
    observed_sha256: str | None
    shared_owner: str | None
    recovery_handle: str | None


class DeinitializationReport(TypedDict):
    schema_version: int
    repository: str
    status: Literal["complete", "not-initialized"]
    operations: list[DeinitializationReportOperation]
    residues: list[DeinitializationReportOperation]
    recovery_handle: str | None


def deinitialization_receipt_path(repo_root: Path) -> Path:
    return git_dir(repo_root.expanduser().resolve()) / DEINIT_RECEIPT_FILENAME


def load_deinitialization_receipt(
    repo_root: Path,
) -> DeinitializationReceipt | None:
    path = deinitialization_receipt_path(repo_root)
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, json.JSONDecodeError) as exc:
        raise SpiceError(
            f"could not read deinitialization receipt {path}: {exc}"
        ) from exc
    try:
        return _deinitialization_receipt_from_payload(payload)
    except (KeyError, TypeError, ValueError) as exc:
        raise SpiceError(f"invalid deinitialization receipt {path}: {exc}") from exc


def write_deinitialization_receipt(receipt: DeinitializationReceipt) -> None:
    path = deinitialization_receipt_path(receipt.repo_root)
    content = (
        json.dumps(
            deinitialization_receipt_payload(receipt),
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    atomic_write_text(path, content, write_if_changed=True)
    path.chmod(INIT_RECEIPT_MODE)


def deinitialization_receipt_payload(
    receipt: DeinitializationReceipt,
) -> dict[str, object]:
    return {
        "schema_version": receipt.schema_version,
        "repository": str(receipt.repo_root),
        "status": receipt.status.value,
        "initialization_receipt": initialization_receipt_payload(
            receipt.initialization
        ),
        "operations": [
            {
                "initialization_index": state.initialization_index,
                "completed": state.completed,
                "outcome": state.outcome.value if state.outcome is not None else None,
                "observed_kind": state.observed_kind,
                "observed_value": state.observed_value,
                "observed_mode": state.observed_mode,
                "observed_sha256": state.observed_sha256,
                "shared_owner": state.shared_owner,
            }
            for state in receipt.operations
        ],
    }


def plan_deinitialization(repo_root: Path) -> DeinitializationPlan:
    """Predict one complete reversal without creating receipts or mutating state."""
    resolved_root = repo_root.expanduser().resolve()
    reversal = load_deinitialization_receipt(resolved_root)
    if reversal is None:
        initialization = load_initialization_receipt(resolved_root)
        if initialization is None:
            return DeinitializationPlan(resolved_root, None, None, ())
        if initialization.repo_root != resolved_root:
            raise SpiceError(
                "initialization receipt belongs to a different repository: "
                f"{initialization.repo_root}"
            )
        receipt_digest = initialization_receipt_digest(initialization)
        reversal = _new_deinitialization_receipt(initialization)
    else:
        receipt_digest = initialization_receipt_digest(reversal.initialization)

    if reversal.repo_root != resolved_root:
        raise SpiceError(
            "deinitialization receipt belongs to a different repository: "
            f"{reversal.repo_root}"
        )
    states = list(reversal.operations)
    simulated = reversal
    for position, state in enumerate(states):
        if state.completed:
            continue
        operation = reversal.initialization.operations[
            state.initialization_index
        ].operation
        states[position] = _predict_reverse_operation(
            resolved_root,
            operation,
            state.initialization_index,
            simulated,
        )
        simulated = replace(simulated, operations=tuple(states))
    return DeinitializationPlan(
        resolved_root,
        reversal,
        receipt_digest,
        tuple(states),
    )


def deinitialization_plan_payload(plan: DeinitializationPlan) -> dict[str, object]:
    """Return the ordered machine representation of a reversal preview."""
    operations = []
    if plan.reversal is not None:
        for order, state in enumerate(plan.operations, start=1):
            operation = plan.reversal.initialization.operations[
                state.initialization_index
            ].operation
            operations.append(
                {
                    "order": order,
                    "kind": operation.kind.value,
                    "target": operation.target,
                    "scope": operation.scope.value,
                    "predicted_outcome": (
                        state.outcome.value if state.outcome is not None else None
                    ),
                    "observed_kind": state.observed_kind,
                    "observed_value": state.observed_value,
                    "observed_mode": state.observed_mode,
                    "observed_sha256": state.observed_sha256,
                    "shared_owner": state.shared_owner,
                }
            )
    return {
        "schema_version": plan.schema_version,
        "repository": str(plan.repo_root),
        "status": "preview" if plan.reversal is not None else "not-initialized",
        "receipt_digest": plan.receipt_digest,
        "operations": operations,
    }


def deinitialization_plan_rows(plan: DeinitializationPlan) -> list[str]:
    """Render a reversal preview in its exact receipt order."""
    status = "preview" if plan.reversal is not None else "not-initialized"
    rows = [
        f"unapply-plan schema={plan.schema_version} "
        f"status={status} repository={plan.repo_root}"
    ]
    if plan.reversal is None:
        rows.append("preview: no changes applied; pass --apply to execute")
        return rows
    rows.append(f"receipt-digest={plan.receipt_digest}")
    for order, state in enumerate(plan.operations, start=1):
        operation = plan.reversal.initialization.operations[
            state.initialization_index
        ].operation
        outcome = state.outcome.value if state.outcome is not None else "pending"
        rows.append(f"{order}. {outcome} {operation.kind.value} {operation.target}")
    rows.append("preview: no changes applied; pass --apply to execute")
    return rows


def apply_deinitialization_plan(
    plan: DeinitializationPlan,
) -> DeinitializationReport:
    """Apply a reversal plan, preserving the receipt's resumable write boundary."""
    current_reversal = load_deinitialization_receipt(plan.repo_root)
    current_initialization = (
        current_reversal.initialization
        if current_reversal is not None
        else load_initialization_receipt(plan.repo_root)
    )
    current_digest = (
        initialization_receipt_digest(current_initialization)
        if current_initialization is not None
        else None
    )
    if current_digest != plan.receipt_digest:
        raise SpiceError(
            "initialization receipt changed after unapply planning: "
            f"expected {plan.receipt_digest or '<none>'}; "
            f"observed {current_digest or '<none>'}"
        )
    if plan.reversal is None:
        return _not_initialized_report(plan.repo_root)
    reversal = current_reversal
    if reversal is None:
        initialization = load_initialization_receipt(plan.repo_root)
        if initialization is None:
            return _not_initialized_report(plan.repo_root)
        reversal = _new_deinitialization_receipt(initialization)
        write_deinitialization_receipt(reversal)
        write_initialization_receipt(reversal.initialization)
    if reversal.repo_root != plan.repo_root:
        raise SpiceError(
            "deinitialization receipt belongs to a different repository: "
            f"{reversal.repo_root}"
        )
    if reversal.status is DeinitReceiptStatus.COMPLETE:
        return _finalize_deinitialization(reversal)

    states = list(reversal.operations)
    for position, state in enumerate(states):
        if state.completed:
            continue
        operation = reversal.initialization.operations[
            state.initialization_index
        ].operation
        states[position] = _reverse_operation(
            plan.repo_root,
            operation,
            state.initialization_index,
            reversal,
        )
        reversal = replace(reversal, operations=tuple(states))
        write_deinitialization_receipt(reversal)

    reversal = replace(reversal, status=DeinitReceiptStatus.COMPLETE)
    write_deinitialization_receipt(reversal)
    return _finalize_deinitialization(reversal)


def _predict_reverse_operation(
    repo_root: Path,
    operation: InitOperation,
    initialization_index: int,
    receipt: DeinitializationReceipt,
) -> DeinitOperationState:
    if operation.kind is InitOperationKind.FILE:
        return _predict_file_operation(repo_root, operation, initialization_index)
    return _predict_config_operation(
        repo_root, operation, initialization_index, receipt
    )


def _predict_file_operation(
    repo_root: Path,
    operation: InitOperation,
    initialization_index: int,
) -> DeinitOperationState:
    observed = _observe_file(repo_root / operation.target)
    if not operation.managed:
        outcome = DeinitOutcome.PRESERVED_UNMANAGED
    elif _file_matches(observed, operation.previous_value, operation.previous_mode):
        outcome = DeinitOutcome.ALREADY_RESTORED
    elif not _file_matches(
        observed, operation.generated_value, operation.generated_mode
    ):
        outcome = DeinitOutcome.RETAINED_DIVERGED
    else:
        outcome = DeinitOutcome.RESTORED
    return _file_outcome(initialization_index, outcome, observed)


def _predict_config_operation(
    repo_root: Path,
    operation: InitOperation,
    initialization_index: int,
    receipt: DeinitializationReceipt,
) -> DeinitOperationState:
    observed = git_config_file_get(operation.scope_path, operation.target)
    shared_owner = None
    if not operation.managed:
        outcome = DeinitOutcome.PRESERVED_UNMANAGED
    elif observed == operation.previous_value:
        outcome = DeinitOutcome.ALREADY_RESTORED
    elif observed != operation.generated_value:
        outcome = DeinitOutcome.RETAINED_DIVERGED
    elif operation.scope is InitOperationScope.COMMON_GIT_CONFIG:
        owners = _other_common_owners(repo_root, operation)
        if owners:
            outcome = DeinitOutcome.RETAINED_SHARED
            shared_owner = str(owners[0][0])
        elif _retained_worktree_config_requires_common_setting(receipt):
            outcome = DeinitOutcome.RETAINED_SHARED
            shared_owner = str(repo_root)
        else:
            outcome = DeinitOutcome.RESTORED
    else:
        outcome = DeinitOutcome.RESTORED
    return _config_outcome(
        initialization_index,
        outcome,
        observed,
        shared_owner=shared_owner,
    )


def deinitialization_report_rows(report: DeinitializationReport) -> list[str]:
    rows = [f"unapply status={report['status']} repository={report['repository']}"]
    for item in report["operations"]:
        row = f"{item['outcome']} {item['kind']} {item['target']}"
        recovery = item["recovery_handle"]
        if recovery is not None:
            row += f" recovery={recovery}"
        rows.append(row)
    recovery_handle = report["recovery_handle"]
    if isinstance(recovery_handle, str):
        rows.append(f"recovery={recovery_handle}")
    return rows


def _new_deinitialization_receipt(
    initialization: InitializationReceipt,
) -> DeinitializationReceipt:
    owned = replace(initialization, status=InitReceiptStatus.DEINITIALIZING)
    return DeinitializationReceipt(
        repo_root=initialization.repo_root,
        initialization=owned,
        status=DeinitReceiptStatus.REVERSING,
        operations=tuple(
            DeinitOperationState(initialization_index=index)
            for index in reversed(range(len(initialization.operations)))
        ),
    )


def _reverse_operation(
    repo_root: Path,
    operation: InitOperation,
    initialization_index: int,
    receipt: DeinitializationReceipt,
) -> DeinitOperationState:
    if operation.kind is InitOperationKind.FILE:
        return _reverse_file_operation(repo_root, operation, initialization_index)
    return _reverse_config_operation(
        repo_root,
        operation,
        initialization_index,
        receipt,
    )


def _reverse_file_operation(
    repo_root: Path,
    operation: InitOperation,
    initialization_index: int,
) -> DeinitOperationState:
    observed = _observe_file(repo_root / operation.target)
    if not operation.managed:
        return _file_outcome(
            initialization_index, DeinitOutcome.PRESERVED_UNMANAGED, observed
        )
    if _file_matches(observed, operation.previous_value, operation.previous_mode):
        return _file_outcome(
            initialization_index, DeinitOutcome.ALREADY_RESTORED, observed
        )
    if not _file_matches(observed, operation.generated_value, operation.generated_mode):
        return _file_outcome(
            initialization_index, DeinitOutcome.RETAINED_DIVERGED, observed
        )

    target = repo_root / operation.target
    if operation.previous_value is None:
        target.unlink()
    else:
        atomic_write_text(target, operation.previous_value, write_if_changed=True)
        if operation.previous_mode is not None:
            target.chmod(operation.previous_mode)
    return _file_outcome(initialization_index, DeinitOutcome.RESTORED, observed)


def _reverse_config_operation(
    repo_root: Path,
    operation: InitOperation,
    initialization_index: int,
    receipt: DeinitializationReceipt,
) -> DeinitOperationState:
    observed = git_config_file_get(operation.scope_path, operation.target)
    if not operation.managed:
        return _config_outcome(
            initialization_index, DeinitOutcome.PRESERVED_UNMANAGED, observed
        )
    if observed == operation.previous_value:
        return _config_outcome(
            initialization_index, DeinitOutcome.ALREADY_RESTORED, observed
        )
    if observed != operation.generated_value:
        return _config_outcome(
            initialization_index, DeinitOutcome.RETAINED_DIVERGED, observed
        )
    if operation.scope is InitOperationScope.COMMON_GIT_CONFIG:
        shared_owner = _transfer_common_ownership(repo_root, operation)
        if shared_owner is not None:
            return _config_outcome(
                initialization_index,
                DeinitOutcome.RETAINED_SHARED,
                observed,
                shared_owner=str(shared_owner),
            )
        if _retained_worktree_config_requires_common_setting(receipt):
            return _config_outcome(
                initialization_index,
                DeinitOutcome.RETAINED_SHARED,
                observed,
                shared_owner=str(repo_root),
            )
    _restore_git_config(operation)
    return _config_outcome(initialization_index, DeinitOutcome.RESTORED, observed)


def _retained_worktree_config_requires_common_setting(
    receipt: DeinitializationReceipt,
) -> bool:
    for state in receipt.operations:
        if state.outcome not in RETAINED_OUTCOMES:
            continue
        operation = receipt.initialization.operations[
            state.initialization_index
        ].operation
        if operation.scope is InitOperationScope.WORKTREE_GIT_CONFIG:
            return True
    return False


def _observe_file(path: Path) -> FileObservation:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return FileObservation("absent", None, None, None)
    mode = stat.S_IMODE(metadata.st_mode)
    if stat.S_ISLNK(metadata.st_mode):
        target = os.readlink(path).encode("utf-8", errors="surrogateescape")
        return FileObservation(
            "symlink", None, mode, hashlib.sha256(target).hexdigest()
        )
    if not stat.S_ISREG(metadata.st_mode):
        kind = "directory" if stat.S_ISDIR(metadata.st_mode) else "other"
        return FileObservation(kind, None, mode, None)
    digest = hashlib.sha256()
    content = bytearray()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(HASH_CHUNK_BYTES), b""):
            content.extend(chunk)
            digest.update(chunk)
    return FileObservation("file", bytes(content), mode, digest.hexdigest())


def _file_matches(
    observed: FileObservation,
    expected_value: str | None,
    expected_mode: int | None,
) -> bool:
    if expected_value is None:
        return (observed.kind, observed.mode) == ("absent", None)
    return (
        observed.kind,
        observed.content,
        observed.mode,
    ) == ("file", expected_value.encode("utf-8"), expected_mode)


def _file_outcome(
    initialization_index: int,
    outcome: DeinitOutcome,
    observed: FileObservation,
) -> DeinitOperationState:
    return DeinitOperationState(
        initialization_index=initialization_index,
        completed=True,
        outcome=outcome,
        observed_kind=observed.kind,
        observed_mode=observed.mode,
        observed_sha256=observed.sha256,
    )


def _config_outcome(
    initialization_index: int,
    outcome: DeinitOutcome,
    observed: str | None,
    *,
    shared_owner: str | None = None,
) -> DeinitOperationState:
    return DeinitOperationState(
        initialization_index=initialization_index,
        completed=True,
        outcome=outcome,
        observed_kind="git-config",
        observed_value=observed,
        shared_owner=shared_owner,
    )


def _transfer_common_ownership(
    repo_root: Path, operation: InitOperation
) -> Path | None:
    for owner_root, receipt, position in _other_common_owners(repo_root, operation):
        receiver = receipt.operations[position]
        transferred = replace(
            receiver.operation,
            previous_value=operation.previous_value,
            previous_mode=operation.previous_mode,
            introduced=operation.introduced,
            previous_effective_value=operation.previous_effective_value,
        )
        operations = list(receipt.operations)
        operations[position] = replace(receiver, operation=transferred)
        write_initialization_receipt(replace(receipt, operations=tuple(operations)))
        return owner_root
    return None


def _other_common_owners(
    repo_root: Path, operation: InitOperation
) -> list[tuple[Path, InitializationReceipt, int]]:
    owners: list[tuple[Path, InitializationReceipt, int]] = []
    for owner_root in _worktree_roots(repo_root):
        if owner_root == repo_root:
            continue
        receipt = load_initialization_receipt(owner_root)
        if receipt is None or receipt.status is InitReceiptStatus.DEINITIALIZING:
            continue
        for position, receipt_operation in enumerate(receipt.operations):
            candidate = receipt_operation.operation
            if (
                candidate.kind,
                candidate.scope,
                candidate.scope_path,
                candidate.target,
                candidate.generated_value,
                candidate.managed,
            ) == (
                operation.kind,
                operation.scope,
                operation.scope_path,
                operation.target,
                operation.generated_value,
                True,
            ):
                owners.append((owner_root, receipt, position))
                break
    return sorted(owners, key=lambda item: str(item[0]))


def _worktree_roots(repo_root: Path) -> tuple[Path, ...]:
    result = run_git_command(
        ["git", "-C", str(repo_root), "worktree", "list", "--porcelain", "-z"],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        suffix = f": {detail}" if detail else ""
        raise SpiceError(f"could not enumerate Git worktrees for {repo_root}{suffix}")
    return tuple(
        Path(field.removeprefix("worktree ")).expanduser().resolve()
        for field in result.stdout.split("\0")
        if field.startswith("worktree ")
    )


def _restore_git_config(operation: InitOperation) -> None:
    if operation.previous_value is None:
        arguments = ["--unset-all", operation.target]
    else:
        arguments = ["--replace-all", operation.target, operation.previous_value]
    result = run_git_command(
        ["git", "config", "--file", str(operation.scope_path), *arguments],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode == 0:
        return
    detail = (result.stderr or result.stdout).strip()
    suffix = f": {detail}" if detail else ""
    raise SpiceError(
        f"could not restore initialization Git config {operation.target} "
        f"at {operation.scope_path}{suffix}"
    )


def _finalize_deinitialization(
    receipt: DeinitializationReceipt,
) -> DeinitializationReport:
    retained = tuple(
        state for state in receipt.operations if state.outcome in RETAINED_OUTCOMES
    )
    recovery_path = _recovery_path(receipt) if retained else None
    report = _completed_report(receipt, recovery_path)
    if recovery_path is not None:
        _write_recovery_report(recovery_path, report)
    _cleanup_introduced_containers(receipt)
    _unlink(initialization_receipt_path(receipt.repo_root))
    _cleanup_introduced_containers(receipt)
    _unlink(deinitialization_receipt_path(receipt.repo_root))
    return report


def _cleanup_introduced_containers(receipt: DeinitializationReceipt) -> None:
    operations = tuple(item.operation for item in receipt.initialization.operations)
    scope_paths = {
        operation.scope_path
        for operation in operations
        if operation.introduced_scope_path
    }
    for path in sorted(scope_paths, key=str):
        _unlink_empty_regular_file(path)

    relative_directories = {
        relative
        for operation in operations
        for relative in operation.introduced_parent_directories
    }
    for relative in sorted(
        relative_directories,
        key=lambda value: (len(Path(value).parts), value),
        reverse=True,
    ):
        _remove_empty_directory(receipt.repo_root / relative)


def _unlink_empty_regular_file(path: Path) -> None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return
    if stat.S_ISREG(metadata.st_mode) and metadata.st_size == 0:
        path.unlink()


def _remove_empty_directory(path: Path) -> None:
    try:
        path.rmdir()
    except FileNotFoundError:
        return
    except OSError as exc:
        if exc.errno in {errno.ENOTEMPTY, errno.EEXIST, errno.ENOTDIR}:
            return
        raise SpiceError(
            f"could not remove initialization directory {path}: {exc}"
        ) from exc


def _completed_report(
    receipt: DeinitializationReceipt,
    recovery_path: Path | None,
) -> DeinitializationReport:
    operations: list[DeinitializationReportOperation] = []
    residues: list[DeinitializationReportOperation] = []
    for order, state in enumerate(receipt.operations, start=1):
        operation = receipt.initialization.operations[
            state.initialization_index
        ].operation
        item: DeinitializationReportOperation = {
            "order": order,
            "kind": operation.kind.value,
            "target": operation.target,
            "scope": operation.scope.value,
            "outcome": state.outcome.value if state.outcome is not None else None,
            "observed_kind": state.observed_kind,
            "observed_value": state.observed_value,
            "observed_mode": state.observed_mode,
            "observed_sha256": state.observed_sha256,
            "shared_owner": state.shared_owner,
            "recovery_handle": None,
        }
        if state.outcome in RETAINED_OUTCOMES and recovery_path is not None:
            item["recovery_handle"] = f"{recovery_path}#/residues/{len(residues)}"
            residues.append(item)
        operations.append(item)
    return {
        "schema_version": DEINIT_SCHEMA_VERSION,
        "repository": str(receipt.repo_root),
        "status": "complete",
        "operations": operations,
        "residues": residues,
        "recovery_handle": str(recovery_path) if recovery_path is not None else None,
    }


def _not_initialized_report(repo_root: Path) -> DeinitializationReport:
    return {
        "schema_version": DEINIT_SCHEMA_VERSION,
        "repository": str(repo_root),
        "status": "not-initialized",
        "operations": [],
        "residues": [],
        "recovery_handle": None,
    }


def _recovery_path(receipt: DeinitializationReceipt) -> Path:
    encoded = json.dumps(
        deinitialization_receipt_payload(receipt),
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    digest = hashlib.sha256(encoded).hexdigest()[:RECOVERY_DIGEST_LENGTH]
    return git_dir(receipt.repo_root) / RECOVERY_DIRNAME / f"deinit-{digest}.json"


def _write_recovery_report(path: Path, report: DeinitializationReport) -> None:
    content = json.dumps(report, indent=2, sort_keys=True) + "\n"
    atomic_write_text(path, content, write_if_changed=True)
    path.chmod(INIT_RECEIPT_MODE)


def _unlink(path: Path) -> None:
    try:
        path.unlink(missing_ok=True)
    except OSError as exc:
        raise SpiceError(f"could not remove completed receipt {path}: {exc}") from exc


def _deinitialization_receipt_from_payload(
    payload: dict[str, object],
) -> DeinitializationReceipt:
    if payload["schema_version"] != DEINIT_SCHEMA_VERSION:
        raise ValueError(f"unsupported schema version {payload['schema_version']!r}")
    initialization_payload = payload["initialization_receipt"]
    if not isinstance(initialization_payload, dict):
        raise TypeError("initialization_receipt must be an object")
    initialization = initialization_receipt_from_payload(initialization_payload)
    operation_payloads = payload["operations"]
    if not isinstance(operation_payloads, list):
        raise TypeError("operations must be a list")
    operations = tuple(
        _deinit_operation_from_payload(item) for item in operation_payloads
    )
    expected = tuple(reversed(range(len(initialization.operations))))
    if tuple(state.initialization_index for state in operations) != expected:
        raise ValueError("deinitialization operations are not in exact reverse order")
    status = DeinitReceiptStatus(_required_string(payload["status"]))
    if status is DeinitReceiptStatus.COMPLETE and any(
        not state.completed for state in operations
    ):
        raise ValueError(
            "complete deinitialization receipt contains unfinished operations"
        )
    return DeinitializationReceipt(
        repo_root=Path(_required_string(payload["repository"])).expanduser().resolve(),
        initialization=initialization,
        status=status,
        operations=operations,
        schema_version=DEINIT_SCHEMA_VERSION,
    )


def _deinit_operation_from_payload(payload: object) -> DeinitOperationState:
    if not isinstance(payload, dict):
        raise TypeError("deinitialization operation must be an object")
    completed = payload.get("completed")
    if not isinstance(completed, bool):
        raise TypeError("deinitialization operation completion must be boolean")
    raw_outcome = payload.get("outcome")
    outcome = (
        None if raw_outcome is None else DeinitOutcome(_required_string(raw_outcome))
    )
    if completed != (outcome is not None):
        raise ValueError("deinitialization completion and outcome must agree")
    return DeinitOperationState(
        initialization_index=_required_int(payload["initialization_index"]),
        completed=completed,
        outcome=outcome,
        observed_kind=_optional_string(payload.get("observed_kind")),
        observed_value=_optional_string(payload.get("observed_value")),
        observed_mode=_optional_int(payload.get("observed_mode")),
        observed_sha256=_optional_string(payload.get("observed_sha256")),
        shared_owner=_optional_string(payload.get("shared_owner")),
    )


def _required_string(value: object) -> str:
    if isinstance(value, str):
        return value
    raise TypeError("value must be a string")


def _optional_string(value: object) -> str | None:
    if value is None or isinstance(value, str):
        return value
    raise TypeError("value must be a string or null")


def _required_int(value: object) -> int:
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    raise TypeError("value must be an integer")


def _optional_int(value: object) -> int | None:
    if value is None:
        return None
    return _required_int(value)
