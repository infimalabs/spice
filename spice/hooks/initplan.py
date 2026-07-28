"""Side-effect-free planning and ordered application for ``spice init``.

The plan is the receipt-shaped boundary between discovery and mutation.  It
captures the exact scoped state observed before initialization and the exact
state Spice intends to generate, including file modes and a deterministic
ownership digest suitable for a later durable receipt.
"""

from __future__ import annotations

import json
import os
import stat
from dataclasses import replace
from pathlib import Path

from spice.commandplan import command_plan_payload
from spice.config.trust import ExactRepositoryConfigApproval
from spice.hooks.configapproval import (
    record_init_exact_approval,
    require_init_exact_approval_current,
)
from spice.hooks.initmodel import (
    InitializationMode,
    InitializationPlan,
    InitializationReceipt,
    InitializationReceiptRecord,
    InitOperation,
    InitOperationKind,
    InitOperationScope,
    InitReceiptEvent,
    InitReceiptOperation,
    InitReceiptStatus,
)
from spice.hooks.initplanning import (
    GATE_HOOK_ARGS as GATE_HOOK_ARGS,
    HOOK_ARGS as HOOK_ARGS,
    HOOKS_DIRNAME as HOOKS_DIRNAME,
    STATE_GITIGNORE_CONTENT as STATE_GITIGNORE_CONTENT,
    _operation_payload,
    _plan_operation_payload,
    git_config_file_get as git_config_file_get,
    hook_shim_content as hook_shim_content,
    initialization_detail_rows as initialization_detail_rows,
    initialization_plan_payload as initialization_plan_payload,
    initialization_preview_rows as initialization_preview_rows,
    initialization_receipt_path as initialization_receipt_path,
    plan_initialization as plan_initialization,
)
from spice.errors import SpiceError
from spice.operatorstate import (
    INITIALIZATION_RECEIPT_PATH,
    OPERATOR_STATE_MIGRATION_SCHEMA_VERSION,
    OPERATOR_STATE_RELOCATION_RELEASE,
    operator_state_migration_marker,
    prepare_operator_state_path,
)
from spice.paths import (
    atomic_write_json,
    atomic_write_text,
    fsync_directory,
    git_dir,
)
from spice.process.git import run_git_command

INIT_RECEIPT_MODE = 0o600
WITHDRAWN_INIT_RECEIPT_FILENAME = "init-receipt.json"
WITHDRAWN_DEINIT_RECEIPT_FILENAME = "spice-deinit-receipt.json"
OWNERSHIP_DIGEST_BYTES = 32
RECEIPT_DIGEST_BYTES = 32
RECEIPT_LOG_SCHEMA_VERSION = 1
# This is a refusal/resource bound, not the source of regular-file append
# atomicity. POSIX O_APPEND supplies the indivisible seek-to-end plus write;
# one unbuffered os.write call per pre-encoded record preserves that guarantee.
RECEIPT_RECORD_MAX_BYTES = 64 * 1024
FILE_MODE_MAX = 0o7777


def initialization_receipt_payload(
    receipt: InitializationReceipt,
) -> dict[str, object]:
    return {
        "schema_version": receipt.schema_version,
        "plan_schema_version": receipt.plan_schema_version,
        "repository": str(receipt.repo_root),
        "mode": receipt.mode.value,
        "status": receipt.status.value,
        "approved_repository_config_digest": (
            receipt.approved_repository_config_digest
        ),
        "operations": [
            {
                **_operation_payload(receipt_operation.operation),
                "completed": receipt_operation.completed,
            }
            for receipt_operation in receipt.operations
        ],
    }


def initialization_receipt_digest(receipt: InitializationReceipt) -> str:
    """Hash active receipt operations through the plan's canonical vocabulary."""
    return initialization_plan_digest(
        InitializationPlan(
            repo_root=receipt.repo_root,
            mode=receipt.mode,
            schema_version=receipt.plan_schema_version,
            operations=tuple(
                item.operation for item in receipt.operations if item.completed
            ),
        )
    )


def initialization_plan_digest(plan: InitializationPlan) -> str:
    """Hash a plan through the same normalized operation sequence as its receipt."""
    return _operation_sequence_digest(plan.operations)


def _operation_sequence_digest(
    operations: tuple[InitOperation, ...],
) -> str:
    payload = command_plan_payload(
        command="init",
        operations=[
            {
                **_plan_operation_payload(operation),
                "will_change": operation.will_change,
            }
            for operation in operations
        ],
    )
    return str(payload["plan_digest"])


def initialization_receipt_record_payload(
    record: InitializationReceiptRecord,
) -> dict[str, object]:
    """Return one total JSONL record over the normalized plan operation."""
    return {
        "schema_version": record.schema_version,
        "plan_schema_version": record.plan_schema_version,
        "repository": str(record.repo_root),
        "mode": record.mode.value,
        "event": record.event.value,
        "operation_index": record.operation_index,
        "operation_count": record.operation_count,
        **_operation_payload(record.operation),
        "outcome": record.outcome,
        "observed_kind": record.observed_kind,
        "observed_value": record.observed_value,
        "observed_mode": record.observed_mode,
        "observed_sha256": record.observed_sha256,
        "shared_owner": record.shared_owner,
        "approved_repository_config_digest": (record.approved_repository_config_digest),
    }


def encode_initialization_receipt_record(
    record: InitializationReceiptRecord,
) -> bytes:
    """Encode and bound one record before any append or associated mutation."""
    encoded = (
        json.dumps(
            initialization_receipt_record_payload(record),
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        + b"\n"
    )
    if len(encoded) > RECEIPT_RECORD_MAX_BYTES:
        raise SpiceError(
            "initialization receipt record exceeds encoded byte bound: "
            f"{len(encoded)} > {RECEIPT_RECORD_MAX_BYTES}"
        )
    return encoded


def append_initialization_receipt_record(
    record: InitializationReceiptRecord,
    *,
    encoded: bytes | None = None,
) -> None:
    """Append one pre-bounded record with one unbuffered O_APPEND write.

    POSIX regular-file ``O_APPEND`` makes positioning at end-of-file and the
    following write one indivisible step relative to other writers. That is the
    guarantee. The two conditions preserving it here are that the complete
    record is encoded and size-checked first, then emitted by exactly one
    unbuffered ``os.write`` call. ``RECEIPT_RECORD_MAX_BYTES`` is a refusal and
    resource margin; it is not borrowed atomicity from pipes or another file
    type.
    """
    payload = encode_initialization_receipt_record(record)
    if encoded is not None and encoded != payload:
        raise SpiceError(
            "pre-encoded initialization receipt record does not match its fact"
        )
    path = initialization_receipt_path(record.repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    existed = path.exists()
    flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND
    flags |= getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags, INIT_RECEIPT_MODE)
    except OSError as exc:
        raise SpiceError(
            f"could not open initialization receipt {path}: {exc}"
        ) from exc
    chmod_after_close = not hasattr(os, "fchmod")
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise SpiceError(f"initialization receipt is not a regular file: {path}")
        if not chmod_after_close:
            os.fchmod(descriptor, INIT_RECEIPT_MODE)
        written = os.write(descriptor, payload)
        if written != len(payload):
            raise SpiceError(
                "short initialization receipt append: "
                f"wrote {written} of {len(payload)} bytes"
            )
        os.fsync(descriptor)
    except OSError as exc:
        raise SpiceError(
            f"could not append initialization receipt {path}: {exc}"
        ) from exc
    finally:
        os.close(descriptor)
    if chmod_after_close:
        path.chmod(INIT_RECEIPT_MODE)
    if not existed:
        fsync_directory(path.parent)


def load_initialization_receipt_records(
    repo_root: Path,
) -> tuple[InitializationReceiptRecord, ...]:
    """Read and validate the complete append-only receipt log."""
    resolved_root = repo_root.expanduser().resolve()
    path = _prepare_initialization_receipt_log(resolved_root)
    try:
        content = path.read_bytes()
    except FileNotFoundError:
        return ()
    except OSError as exc:
        raise SpiceError(
            f"could not read initialization receipt {path}: {exc}"
        ) from exc
    if not content:
        return ()
    if not content.endswith(b"\n"):
        raise SpiceError(f"invalid initialization receipt {path}: unterminated record")
    records: list[InitializationReceiptRecord] = []
    for line_number, encoded in enumerate(content.splitlines(keepends=True), start=1):
        if len(encoded) > RECEIPT_RECORD_MAX_BYTES:
            raise SpiceError(
                f"invalid initialization receipt {path}: record {line_number} "
                f"exceeds {RECEIPT_RECORD_MAX_BYTES} bytes"
            )
        try:
            payload = json.loads(encoded)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise SpiceError(
                f"invalid initialization receipt {path}: record {line_number}: {exc}"
            ) from exc
        if not isinstance(payload, dict):
            raise SpiceError(
                f"invalid initialization receipt {path}: "
                f"record {line_number} must be an object"
            )
        try:
            record = initialization_receipt_record_from_payload(payload)
        except (KeyError, TypeError, ValueError) as exc:
            raise SpiceError(
                f"invalid initialization receipt {path}: record {line_number}: {exc}"
            ) from exc
        if record.repo_root != resolved_root:
            raise SpiceError(
                "initialization receipt belongs to a different repository: "
                f"{record.repo_root}"
            )
        records.append(record)
    return tuple(records)


def load_initialization_receipt(repo_root: Path) -> InitializationReceipt | None:
    """Replay the append-only log into the current active ownership receipt."""
    records = load_initialization_receipt_records(repo_root)
    if not records:
        return None
    active: dict[
        tuple[InitOperationKind, InitOperationScope, str],
        InitializationReceiptRecord,
    ] = {}
    expected_count = 0
    unapplying = False
    plan_schema_version = records[0].plan_schema_version
    mode = records[0].mode
    approved_repository_config_digest: str | None = None
    for record in records:
        if record.plan_schema_version != plan_schema_version:
            raise SpiceError("initialization receipt mixes plan schema versions")
        if record.mode is InitializationMode.FULL:
            mode = InitializationMode.FULL
        if record.approved_repository_config_digest is not None:
            approved_repository_config_digest = record.approved_repository_config_digest
        if record.event is InitReceiptEvent.UNAPPLY:
            unapplying = True
            continue
        if unapplying:
            raise SpiceError(
                "initialization receipt contains an apply, transfer, or approval record "
                "after reversal began"
            )
        key = _operation_key(record.operation)
        if record.event is InitReceiptEvent.APPROVAL:
            if key not in active:
                raise SpiceError(
                    "initialization receipt approves configuration against an "
                    f"unknown operation {record.operation.target!r}"
                )
            prior = active[key]
            if (
                record.operation != prior.operation
                or record.operation_index != prior.operation_index
                or record.operation_count != prior.operation_count
            ):
                raise SpiceError(
                    "initialization receipt approval changes its operation context"
                )
            continue
        if record.event is InitReceiptEvent.TRANSFER and key not in active:
            raise SpiceError(
                "initialization receipt transfers unknown operation "
                f"{record.operation.target!r}"
            )
        active[key] = record
        expected_count = max(expected_count, record.operation_count)
    ordered = tuple(sorted(active.values(), key=lambda item: item.operation_index))
    indices = tuple(record.operation_index for record in ordered)
    if len(set(indices)) != len(indices):
        raise SpiceError("initialization receipt has duplicate operation positions")
    complete = len(ordered) == expected_count and indices == tuple(
        range(expected_count)
    )
    return InitializationReceipt(
        repo_root=records[0].repo_root,
        mode=mode,
        plan_schema_version=plan_schema_version,
        status=(
            InitReceiptStatus.DEINITIALIZING
            if unapplying
            else InitReceiptStatus.COMPLETE
            if complete
            else InitReceiptStatus.APPLYING
        ),
        operations=tuple(
            InitReceiptOperation(operation=record.operation, completed=True)
            for record in ordered
        ),
        approved_repository_config_digest=approved_repository_config_digest,
    )


def initialization_receipt_record_from_payload(
    payload: dict[str, object],
) -> InitializationReceiptRecord:
    schema_version = _required_int(payload["schema_version"])
    if schema_version != RECEIPT_LOG_SCHEMA_VERSION:
        raise ValueError(f"unsupported receipt record schema {schema_version!r}")
    plan_schema_version = _required_int(payload["plan_schema_version"])
    if plan_schema_version != 1:
        raise ValueError(f"unsupported plan schema version {plan_schema_version!r}")
    event = InitReceiptEvent(_required_string(payload["event"]))
    operation_index = _required_int(payload["operation_index"])
    operation_count = _required_int(payload["operation_count"])
    if operation_index < 0 or operation_count <= operation_index:
        raise ValueError("receipt operation position is outside its operation count")
    outcome = _optional_string(payload.get("outcome"))
    if (event is InitReceiptEvent.UNAPPLY) != (outcome is not None):
        raise ValueError("only unapply records carry a reversal outcome")
    approved_digest = _optional_ownership_digest(
        payload.get("approved_repository_config_digest")
    )
    if event is InitReceiptEvent.APPROVAL and approved_digest is None:
        raise ValueError("approval record is missing its repository config digest")
    return InitializationReceiptRecord(
        repo_root=Path(_required_string(payload["repository"])).expanduser().resolve(),
        mode=InitializationMode(_required_string(payload["mode"])),
        plan_schema_version=plan_schema_version,
        event=event,
        operation_index=operation_index,
        operation_count=operation_count,
        operation=_operation_from_payload(payload),
        outcome=outcome,
        observed_kind=_optional_string(payload.get("observed_kind")),
        observed_value=_optional_string(payload.get("observed_value")),
        observed_mode=_optional_mode(payload.get("observed_mode")),
        observed_sha256=_optional_string(payload.get("observed_sha256")),
        shared_owner=_optional_string(payload.get("shared_owner")),
        approved_repository_config_digest=approved_digest,
        schema_version=schema_version,
    )


def apply_initialization_plan(
    plan: InitializationPlan,
    *,
    repository_config_approval: ExactRepositoryConfigApproval | None = None,
) -> InitializationReceipt:
    """Apply one plan, appending exactly one receipt record per completion."""
    if repository_config_approval is not None:
        require_init_exact_approval_current(
            plan.repo_root,
            repository_config_approval,
        )
    if (git_dir(plan.repo_root) / WITHDRAWN_DEINIT_RECEIPT_FILENAME).is_file():
        raise SpiceError(
            "run `spice init --unapply --apply` to resume the interrupted reversal; "
            "initialization cannot run while its receipt is active"
        )
    existing = load_initialization_receipt(plan.repo_root)
    if existing is not None and existing.status is InitReceiptStatus.DEINITIALIZING:
        raise SpiceError(
            "run `spice init --unapply --apply` to resume the interrupted reversal; "
            "initialization cannot run while its receipt is active"
        )
    approved_digest = (
        existing.approved_repository_config_digest if existing is not None else None
    )
    if repository_config_approval is not None:
        approved_digest = repository_config_approval.digest
    approval_changed = (
        existing is not None
        and approved_digest != existing.approved_repository_config_digest
    )
    receipt = _receipt_for_plan(
        plan,
        existing,
        approved_repository_config_digest=approved_digest,
    )
    completed_candidate = _completed_plan_receipt(plan, receipt)
    if (
        existing is not None
        and existing == completed_candidate
        and all(not operation.will_change for operation in plan.operations)
    ):
        if repository_config_approval is not None:
            record_init_exact_approval(
                plan.repo_root,
                repository_config_approval,
            )
        return existing

    receipt, appended_completion = _apply_initialization_operations(
        plan,
        receipt,
        approved_digest,
    )
    if approval_changed and not appended_completion:
        _append_initialization_approval(receipt, approved_digest)

    status = (
        InitReceiptStatus.COMPLETE
        if all(operation.completed for operation in receipt.operations)
        else InitReceiptStatus.APPLYING
    )
    completed = replace(receipt, status=status)
    if repository_config_approval is not None and status is InitReceiptStatus.COMPLETE:
        record_init_exact_approval(
            plan.repo_root,
            repository_config_approval,
        )
    return completed


def _completed_plan_receipt(
    plan: InitializationPlan,
    receipt: InitializationReceipt,
) -> InitializationReceipt:
    """Project one receipt through every operation named by the current plan."""
    plan_keys = tuple(_operation_key(operation) for operation in plan.operations)
    if len(set(plan_keys)) != len(plan_keys):
        raise SpiceError("initialization plan contains duplicate operation identities")
    candidate_operations = tuple(
        replace(receipt_operation, completed=True)
        if _operation_key(receipt_operation.operation) in plan_keys
        else receipt_operation
        for receipt_operation in receipt.operations
    )
    status = (
        InitReceiptStatus.COMPLETE
        if all(operation.completed for operation in candidate_operations)
        else InitReceiptStatus.APPLYING
    )
    return replace(receipt, status=status, operations=candidate_operations)


def _apply_initialization_operations(
    plan: InitializationPlan,
    receipt: InitializationReceipt,
    approved_digest: str | None,
) -> tuple[InitializationReceipt, bool]:
    """Apply incomplete plan operations and report whether a record was appended."""
    operations = list(receipt.operations)
    positions = {
        _operation_key(receipt_operation.operation): index
        for index, receipt_operation in enumerate(operations)
    }
    appended_completion = False
    for operation in plan.operations:
        position = positions[_operation_key(operation)]
        if operations[position].completed:
            continue
        receipt_operation = operations[position].operation
        record = InitializationReceiptRecord(
            repo_root=receipt.repo_root,
            mode=receipt.mode,
            plan_schema_version=receipt.plan_schema_version,
            event=InitReceiptEvent.APPLY,
            operation_index=position,
            operation_count=len(operations),
            operation=receipt_operation,
            approved_repository_config_digest=approved_digest,
        )
        encoded = encode_initialization_receipt_record(record)
        if operation.will_change:
            if operation.kind is InitOperationKind.FILE:
                _apply_file_operation(plan.repo_root, operation)
            else:
                _apply_config_operation(plan.repo_root, operation)
        append_initialization_receipt_record(record, encoded=encoded)
        operations[position] = replace(operations[position], completed=True)
        receipt = replace(receipt, operations=tuple(operations))
        appended_completion = True
    return receipt, appended_completion


def _append_initialization_approval(
    receipt: InitializationReceipt,
    approved_digest: str | None,
) -> None:
    """Append a standalone approval fact against an existing receipt operation."""
    if not receipt.operations:
        raise SpiceError(
            "cannot record repository configuration approval without an "
            "initialization operation"
        )
    approval_record = InitializationReceiptRecord(
        repo_root=receipt.repo_root,
        mode=receipt.mode,
        plan_schema_version=receipt.plan_schema_version,
        event=InitReceiptEvent.APPROVAL,
        operation_index=0,
        operation_count=len(receipt.operations),
        operation=receipt.operations[0].operation,
        approved_repository_config_digest=approved_digest,
    )
    encoded = encode_initialization_receipt_record(approval_record)
    append_initialization_receipt_record(approval_record, encoded=encoded)


def _optional_string(value: object) -> str | None:
    if value is None or isinstance(value, str):
        return value
    raise TypeError("value must be a string or null")


def _required_string(value: object) -> str:
    if isinstance(value, str):
        return value
    raise TypeError("value must be a string")


def _optional_mode(value: object) -> int | None:
    if value is None:
        return None
    if (
        isinstance(value, int)
        and not isinstance(value, bool)
        and 0 <= value <= FILE_MODE_MAX
    ):
        return value
    raise TypeError("file mode must be an integer from 0 through 07777 or null")


def _required_bool(value: object) -> bool:
    if isinstance(value, bool):
        return value
    raise TypeError("value must be boolean")


def _required_int(value: object) -> int:
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    raise TypeError("value must be an integer")


def _string_tuple(value: object) -> tuple[str, ...]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise TypeError("value must be a list of strings")
    return tuple(value)


def _required_ownership_digest(value: object) -> str:
    digest = _required_string(value)
    try:
        decoded = bytes.fromhex(digest)
    except ValueError as exc:
        raise TypeError("ownership digest must be hexadecimal SHA-256") from exc
    if len(decoded) != OWNERSHIP_DIGEST_BYTES:
        raise TypeError("ownership digest must be hexadecimal SHA-256")
    return digest


def _optional_ownership_digest(value: object) -> str | None:
    if value is None:
        return None
    return _required_ownership_digest(value)


def _operation_from_payload(payload: dict[str, object]) -> InitOperation:
    return InitOperation(
        kind=InitOperationKind(_required_string(payload["kind"])),
        target=_required_string(payload["target"]),
        scope=InitOperationScope(_required_string(payload["scope"])),
        scope_path=Path(_required_string(payload["scope_path"])),
        previous_value=_optional_string(payload.get("previous_value")),
        generated_value=_required_string(payload["generated_value"]),
        previous_mode=_optional_mode(payload.get("previous_mode")),
        generated_mode=_optional_mode(payload.get("generated_mode")),
        ownership_digest=_required_ownership_digest(payload["ownership_digest"]),
        initialization_mode=InitializationMode(
            _required_string(payload["initialization_mode"])
        ),
        introduced=_required_bool(payload["introduced"]),
        managed=_required_bool(payload["managed"]),
        previous_effective_value=_optional_string(
            payload.get("previous_effective_value")
        ),
        introduced_parent_directories=_string_tuple(
            payload.get("introduced_parent_directories", [])
        ),
        introduced_scope_path=_required_bool(
            payload.get("introduced_scope_path", False)
        ),
    )


def initialization_receipt_from_payload(
    payload: dict[str, object],
) -> InitializationReceipt:
    if payload["schema_version"] != 1:
        raise ValueError(f"unsupported schema version {payload['schema_version']!r}")
    plan_schema_version = _required_int(payload["plan_schema_version"])
    if plan_schema_version != 1:
        raise ValueError(f"unsupported plan schema version {plan_schema_version!r}")
    operation_payloads = payload["operations"]
    if not isinstance(operation_payloads, list):
        raise TypeError("operations must be a list")
    operations: list[InitReceiptOperation] = []
    operation_keys: set[tuple[InitOperationKind, InitOperationScope, str]] = set()
    for item in operation_payloads:
        if not isinstance(item, dict):
            raise TypeError("receipt operation must be an object")
        completed = item.get("completed")
        if not isinstance(completed, bool):
            raise TypeError("receipt operation completion must be boolean")
        receipt_operation = InitReceiptOperation(
            operation=_operation_from_payload(item),
            completed=completed,
        )
        key = _operation_key(receipt_operation.operation)
        if key in operation_keys:
            raise ValueError(f"duplicate receipt operation identity {key!r}")
        operation_keys.add(key)
        operations.append(receipt_operation)
    status = InitReceiptStatus(_required_string(payload["status"]))
    if status is InitReceiptStatus.COMPLETE and any(
        not operation.completed for operation in operations
    ):
        raise ValueError("complete receipt contains unfinished operations")
    return InitializationReceipt(
        repo_root=Path(_required_string(payload["repository"])).expanduser().resolve(),
        mode=InitializationMode(_required_string(payload["mode"])),
        plan_schema_version=plan_schema_version,
        status=status,
        operations=tuple(operations),
        approved_repository_config_digest=_optional_ownership_digest(
            payload.get("approved_repository_config_digest")
        ),
        schema_version=_required_int(payload["schema_version"]),
    )


def _operation_key(
    operation: InitOperation,
) -> tuple[InitOperationKind, InitOperationScope, str]:
    return operation.kind, operation.scope, operation.target


def _receipt_for_plan(
    plan: InitializationPlan,
    existing: InitializationReceipt | None,
    *,
    approved_repository_config_digest: str | None,
) -> InitializationReceipt:
    if existing is None:
        return InitializationReceipt(
            repo_root=plan.repo_root,
            mode=plan.mode,
            plan_schema_version=plan.schema_version,
            status=InitReceiptStatus.APPLYING,
            operations=tuple(
                InitReceiptOperation(operation=operation, completed=False)
                for operation in plan.operations
            ),
            approved_repository_config_digest=approved_repository_config_digest,
        )
    if existing.repo_root != plan.repo_root:
        raise SpiceError(
            "initialization receipt belongs to a different repository: "
            f"{existing.repo_root}"
        )

    planned_by_key = {
        _operation_key(operation): operation for operation in plan.operations
    }
    merged: list[InitReceiptOperation] = []
    seen: set[tuple[InitOperationKind, InitOperationScope, str]] = set()
    for receipt_operation in existing.operations:
        key = _operation_key(receipt_operation.operation)
        planned = planned_by_key.get(key)
        if planned is None:
            merged.append(receipt_operation)
        else:
            merged.append(_merge_receipt_operation(receipt_operation, planned))
            seen.add(key)
    for operation in plan.operations:
        key = _operation_key(operation)
        if key in seen:
            continue
        merged.append(InitReceiptOperation(operation=operation, completed=False))

    mode = (
        InitializationMode.FULL
        if InitializationMode.FULL in {existing.mode, plan.mode}
        else InitializationMode.GATES_ONLY
    )
    return InitializationReceipt(
        repo_root=plan.repo_root,
        mode=mode,
        plan_schema_version=plan.schema_version,
        status=InitReceiptStatus.APPLYING,
        operations=tuple(merged),
        approved_repository_config_digest=approved_repository_config_digest,
    )


def _merge_receipt_operation(
    existing: InitReceiptOperation,
    planned: InitOperation,
) -> InitReceiptOperation:
    previous = existing.operation
    operation = replace(
        planned,
        previous_value=previous.previous_value,
        previous_mode=previous.previous_mode,
        initialization_mode=previous.initialization_mode,
        introduced=previous.introduced,
        previous_effective_value=previous.previous_effective_value,
        introduced_parent_directories=previous.introduced_parent_directories,
        introduced_scope_path=previous.introduced_scope_path,
    )
    completed = (
        existing.completed
        and previous.ownership_digest == planned.ownership_digest
        and not planned.will_change
    )
    return InitReceiptOperation(operation=operation, completed=completed)


def _prepare_initialization_receipt_log(repo_root: Path) -> Path:
    """Perform the v0.30.0 document-to-log migration once, then return JSONL."""
    canonical = initialization_receipt_path(repo_root)
    predecessor = canonical.with_name(WITHDRAWN_INIT_RECEIPT_FILENAME)
    marker = operator_state_migration_marker(repo_root, INITIALIZATION_RECEIPT_PATH)
    if predecessor.exists() or predecessor.is_symlink():
        if (
            marker.exists()
            or marker.is_symlink()
            or canonical.exists()
            or canonical.is_symlink()
        ):
            raise SpiceError(
                f"remove {predecessor}; initialization receipt document was "
                f"withdrawn in {OPERATOR_STATE_RELOCATION_RELEASE} and has "
                f"already been migrated; use the append-only log at {canonical}"
            )
        if predecessor.is_symlink() or not predecessor.is_file():
            raise SpiceError(
                f"remove {predecessor}; initialization receipt document was "
                f"withdrawn in {OPERATOR_STATE_RELOCATION_RELEASE} and is not "
                "a regular file"
            )
        _migrate_initialization_receipt_document(
            predecessor,
            canonical,
            expected_repo_root=repo_root,
        )
        fsync_directory(canonical.parent)
        try:
            atomic_write_json(
                marker,
                {
                    "schema_version": OPERATOR_STATE_MIGRATION_SCHEMA_VERSION,
                    "release": OPERATOR_STATE_RELOCATION_RELEASE,
                    "kind": INITIALIZATION_RECEIPT_PATH.key,
                    "withdrawn_path": str(predecessor),
                    "canonical_path": str(canonical),
                },
                write_if_changed=True,
            )
        except OSError as exc:
            raise SpiceError(
                "could not record initialization receipt document migration "
                f"from {predecessor} to {canonical}: {exc}"
            ) from exc

    return prepare_operator_state_path(
        repo_root,
        INITIALIZATION_RECEIPT_PATH,
        migrate=lambda source, target: _migrate_initialization_receipt_document(
            source,
            target,
            expected_repo_root=repo_root,
        ),
    )


def _migrate_initialization_receipt_document(
    source: Path,
    target: Path,
    *,
    expected_repo_root: Path,
) -> None:
    """Translate one retired mutable document into its completed record prefix."""
    try:
        payload = json.loads(source.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SpiceError(
            f"could not migrate initialization receipt document {source}: {exc}"
        ) from exc
    if not isinstance(payload, dict):
        raise SpiceError(
            f"could not migrate initialization receipt document {source}: "
            "top level must be an object"
        )
    try:
        receipt = initialization_receipt_from_payload(payload)
    except (KeyError, TypeError, ValueError) as exc:
        raise SpiceError(
            f"could not migrate initialization receipt document {source}: {exc}"
        ) from exc
    if receipt.repo_root != expected_repo_root:
        raise SpiceError(
            f"could not migrate initialization receipt document {source}: "
            f"receipt belongs to {receipt.repo_root}, not {expected_repo_root}"
        )
    completed_positions = tuple(
        position for position, item in enumerate(receipt.operations) if item.completed
    )
    if completed_positions != tuple(range(len(completed_positions))):
        raise SpiceError(
            f"could not migrate initialization receipt document {source}: "
            "completed operations are not an authoritative prefix"
        )
    encoded_records = tuple(
        encode_initialization_receipt_record(
            InitializationReceiptRecord(
                repo_root=receipt.repo_root,
                mode=receipt.mode,
                plan_schema_version=receipt.plan_schema_version,
                event=InitReceiptEvent.APPLY,
                operation_index=position,
                operation_count=len(receipt.operations),
                operation=item.operation,
                approved_repository_config_digest=(
                    receipt.approved_repository_config_digest
                ),
            )
        )
        for position, item in enumerate(receipt.operations)
        if item.completed
    )
    atomic_write_text(
        target,
        b"".join(encoded_records).decode("utf-8"),
        write_if_changed=False,
    )
    target.chmod(INIT_RECEIPT_MODE)
    source.unlink()


def _apply_file_operation(repo_root: Path, operation: InitOperation) -> None:
    target = repo_root / operation.target
    atomic_write_text(target, operation.generated_value, write_if_changed=True)
    if operation.generated_mode is not None:
        target.chmod(operation.generated_mode)


def _apply_config_operation(repo_root: Path, operation: InitOperation) -> None:
    if operation.scope is InitOperationScope.COMMON_GIT_CONFIG:
        args = ["config", operation.target, operation.generated_value]
    else:
        args = [
            "config",
            "--worktree",
            operation.target,
            operation.generated_value,
        ]
    result = run_git_command(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode == 0:
        return
    detail = (result.stderr or result.stdout).strip()
    suffix = f": {detail}" if detail else ""
    raise SpiceError(
        f"could not apply initialization Git config {operation.target} "
        f"for {repo_root}{suffix}"
    )
