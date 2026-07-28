"""Versioned command plans shared by native and mounted Spice verbs."""

from __future__ import annotations

import hashlib
import json
import os
import stat
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.paths import (
    atomic_write_json,
    atomic_write_text,
    fsync_directory,
    git_common_dir,
    git_dir,
    worktree_state_path,
)
from spice.process.git import run_git_command

COMMAND_PLAN_PROTOCOL = "spice.command-plan"
COMMAND_PLAN_SCHEMA_VERSION = 1
MOUNT_RECEIPT_PROTOCOL = "spice.command-receipt"
MOUNT_RECEIPT_SCHEMA_VERSION = 1
PLAN_DIGEST_HEX_LENGTH = 64
FILE_SCOPE = "worktree-file"
COMMON_GIT_CONFIG_SCOPE = "common-git-config"
WORKTREE_GIT_CONFIG_SCOPE = "worktree-git-config"
FILE_MODE_MAX = 0o7777
MOUNT_RECEIPT_MODE = 0o600
MOUNT_RECEIPT_RECORD_MAX_BYTES = 64 * 1024
MOUNT_RECEIPT_DIR = Path("command-receipts")
MOUNT_RECOVERY_DIR = Path("command-recovery")
MOUNT_RECOVERY_DIGEST_LENGTH = 16
MOUNT_OPERATION_KINDS = frozenset({"file", "git-config"})
_RESERVED_PAYLOAD_KEYS = frozenset(
    {"protocol", "schema_version", "command", "plan_digest", "operations"}
)


@dataclass(frozen=True)
class CommandPlanDocument:
    """One validated protocol document emitted by a native or mounted planner."""

    command: str
    operations: tuple[dict[str, Any], ...]
    digest: str
    payload: dict[str, Any]

    def reversed_payload(self) -> dict[str, Any]:
        """Reverse total operation states while preserving the plan protocol."""
        reversed_operations: list[dict[str, Any]] = []
        for operation in reversed(self.operations):
            before = _state(operation.get("observed_before"), "observed_before")
            after = _state(operation.get("intended_after"), "intended_after")
            reversed_operations.append(
                {
                    **{
                        key: value
                        for key, value in operation.items()
                        if key not in {"order", "observed_before", "intended_after"}
                    },
                    "observed_before": after,
                    "intended_after": before,
                }
            )
        return command_plan_payload(
            command=self.command,
            operations=reversed_operations,
            metadata={"direction": "unapply"},
        )


@dataclass(frozen=True)
class _MountedOperation:
    kind: str
    target: str
    scope: str
    before: dict[str, Any]
    after: dict[str, Any]
    managed: bool

    @property
    def label(self) -> str:
        return f"{self.kind}:{self.scope}:{self.target}"

    @property
    def outcome_label(self) -> str:
        if not self.managed:
            return f"preserved-unmanaged:{self.label}"
        if self.before == self.after:
            return f"ready:{self.label}"
        return self.label


class MountedReceiptEvent(StrEnum):
    APPLY = "apply"
    UNAPPLY = "unapply"


class MountedReversalOutcome(StrEnum):
    RESTORED = "restored"
    ALREADY_RESTORED = "already-restored"
    RETAINED_DIVERGED = "retained-diverged"
    RETAINED_SHARED = "retained-shared"
    PRESERVED_UNMANAGED = "preserved-unmanaged"


@dataclass(frozen=True)
class MountedReceiptRecord:
    """One durable mounted-plan completion fact in shared operation vocabulary."""

    repo_root: Path
    receipt_id: str
    command: str
    plan_digest: str
    event: MountedReceiptEvent
    operation_index: int
    operation_count: int
    operation: dict[str, Any]
    outcome: MountedReversalOutcome | None = None
    schema_version: int = MOUNT_RECEIPT_SCHEMA_VERSION


@dataclass(frozen=True)
class MountedPlanReceipt:
    """The active mounted ownership receipt replayed from its append-only log."""

    repo_root: Path
    receipt_id: str
    command: str
    plan_digest: str
    operation_count: int
    operations: tuple[dict[str, Any], ...]
    reversals: tuple[MountedReceiptRecord, ...]

    @property
    def complete(self) -> bool:
        return len(self.operations) == self.operation_count

    @property
    def reversing(self) -> bool:
        return bool(self.reversals)

    @property
    def digest(self) -> str:
        if not self.complete:
            raise SpiceError(
                f"mounted command receipt {self.receipt_id!r} is incomplete"
            )
        return command_plan_digest(self.operations)


@dataclass(frozen=True)
class MountedReversalState:
    operation_index: int
    operation: dict[str, Any]
    outcome: MountedReversalOutcome
    completed: bool


@dataclass(frozen=True)
class MountedReversalPlan:
    repo_root: Path
    receipt_id: str
    receipt_digest: str | None
    document: CommandPlanDocument
    states: tuple[MountedReversalState, ...]


@dataclass(frozen=True)
class MountedReversalReport:
    receipt_id: str
    receipt_digest: str | None
    outcomes: tuple[str, ...]
    recovery_path: Path | None = None


def command_plan_payload(
    *,
    command: str,
    operations: Sequence[Mapping[str, Any]],
    metadata: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build the canonical plan document and its ordered-operation digest."""
    normalized = _normalize_operations(operations)
    payload: dict[str, Any] = {
        "protocol": COMMAND_PLAN_PROTOCOL,
        "schema_version": COMMAND_PLAN_SCHEMA_VERSION,
        "command": _required_text(command, "command"),
        "plan_digest": command_plan_digest(normalized),
        "operations": normalized,
    }
    for key, value in (metadata or {}).items():
        name = str(key)
        if name in _RESERVED_PAYLOAD_KEYS:
            raise SpiceError(f"command plan metadata cannot replace {name!r}")
        payload[name] = _json_value(value, f"metadata.{name}")
    return payload


def command_plan_digest(operations: Sequence[Mapping[str, Any]]) -> str:
    """Hash the version and complete ordered normalized operation list."""
    normalized = _normalize_operations(operations)
    encoded = json.dumps(
        {
            "schema_version": COMMAND_PLAN_SCHEMA_VERSION,
            "operations": normalized,
        },
        allow_nan=False,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def parse_command_plan_document(text: str) -> CommandPlanDocument | None:
    """Recognize a valid plan document; unrelated output is not a plan."""
    try:
        raw = json.loads(text)
    except json.JSONDecodeError:
        return None
    if not isinstance(raw, dict) or raw.get("protocol") != COMMAND_PLAN_PROTOCOL:
        return None
    version = raw.get("schema_version")
    if (
        not isinstance(version, int)
        or isinstance(version, bool)
        or version != COMMAND_PLAN_SCHEMA_VERSION
    ):
        raise SpiceError(
            "mounted command emitted unsupported command plan "
            f"schema_version={version!r}; expected {COMMAND_PLAN_SCHEMA_VERSION}"
        )
    command = _required_text(raw.get("command"), "command plan command")
    operations_raw = raw.get("operations")
    if not isinstance(operations_raw, list):
        raise SpiceError("mounted command plan operations must be an ordered list")
    for expected_order, operation in enumerate(operations_raw, start=1):
        if not isinstance(operation, Mapping):
            raise SpiceError(
                f"mounted command plan operation {expected_order} must be an object"
            )
        if operation.get("order") != expected_order or isinstance(
            operation.get("order"), bool
        ):
            raise SpiceError(
                "mounted command plan operation order must be consecutive: "
                f"expected {expected_order}, got {operation.get('order')!r}"
            )
    operations = _normalize_operations(operations_raw)
    observed = _required_digest(raw.get("plan_digest"), "command plan digest")
    expected = command_plan_digest(operations)
    if observed != expected:
        raise SpiceError(
            "mounted command emitted invalid command plan digest: "
            f"document={observed} computed={expected}"
        )
    payload = {str(key): _json_value(value, str(key)) for key, value in raw.items()}
    payload["operations"] = operations
    return CommandPlanDocument(command, tuple(operations), observed, payload)


def plan_document(payload: Mapping[str, Any]) -> CommandPlanDocument:
    """Validate an in-process native plan payload through the wire parser."""
    encoded = json.dumps(
        payload,
        allow_nan=False,
        ensure_ascii=False,
        separators=(",", ":"),
    )
    document = parse_command_plan_document(encoded)
    if document is None:
        raise SpiceError("native command plan payload is missing the plan protocol")
    return document


def assert_plan_digest(
    document: CommandPlanDocument | Mapping[str, Any],
    expected_digest: str | None,
) -> None:
    """Refuse a stale digest while naming the current ordered operations."""
    if expected_digest is None:
        return
    expected = _required_digest(expected_digest, "--apply plan digest")
    plan = (
        document
        if isinstance(document, CommandPlanDocument)
        else plan_document(document)
    )
    if expected == plan.digest:
        return
    current = ", ".join(_operation_label(operation) for operation in plan.operations)
    raise SpiceError(
        "stale command plan digest: "
        f"expected={expected} observed={plan.digest}; "
        f"current operations changed: {current or '<none>'}"
    )


def assert_mounted_plan_digest(
    document: CommandPlanDocument,
    repo_root: Path,
    expected_digest: str | None,
) -> None:
    """Validate mounted operations and require a digest for destructive plans."""
    operations = tuple(
        _mounted_operation(operation, repo_root) for operation in document.operations
    )
    if expected_digest is None:
        destructive = [
            f"{order}:{operation.label}"
            for order, operation in enumerate(operations, start=1)
            if operation.managed
            and operation.before != operation.after
            and _state_exists(operation.before)
        ]
        if destructive:
            raise SpiceError(
                "destructive mounted command plan requires "
                f"--apply={document.digest}; existing state would change: "
                + ", ".join(destructive)
            )
    assert_plan_digest(document, expected_digest)


def mounted_command_receipt_path(repo_root: Path, receipt_id: str) -> Path:
    """Select one mount's private receipt without exposing a caller path."""
    identity = _required_text(receipt_id, "mounted receipt identity")
    digest = hashlib.sha256(identity.encode("utf-8")).hexdigest()
    return worktree_state_path(
        repo_root.expanduser().resolve(),
        MOUNT_RECEIPT_DIR / f"{digest}.jsonl",
    )


def mounted_receipt_record_payload(
    record: MountedReceiptRecord,
) -> dict[str, Any]:
    return {
        "protocol": MOUNT_RECEIPT_PROTOCOL,
        "schema_version": record.schema_version,
        "plan_schema_version": COMMAND_PLAN_SCHEMA_VERSION,
        "repository": str(record.repo_root),
        "receipt_id": record.receipt_id,
        "command": record.command,
        "plan_digest": record.plan_digest,
        "event": record.event.value,
        "operation_index": record.operation_index,
        "operation_count": record.operation_count,
        "operation": record.operation,
        "outcome": record.outcome.value if record.outcome is not None else None,
    }


def encode_mounted_receipt_record(record: MountedReceiptRecord) -> bytes:
    """Encode and bound one mounted receipt fact before its associated write."""
    encoded = (
        json.dumps(
            mounted_receipt_record_payload(record),
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        + b"\n"
    )
    if len(encoded) > MOUNT_RECEIPT_RECORD_MAX_BYTES:
        raise SpiceError(
            "mounted command receipt record exceeds encoded byte bound: "
            f"{len(encoded)} > {MOUNT_RECEIPT_RECORD_MAX_BYTES}"
        )
    return encoded


def append_mounted_receipt_record(
    record: MountedReceiptRecord,
    *,
    encoded: bytes | None = None,
) -> None:
    """Append one complete bounded fact with one unbuffered O_APPEND write."""
    payload = encode_mounted_receipt_record(record)
    if encoded is not None and encoded != payload:
        raise SpiceError(
            "pre-encoded mounted command receipt record does not match its fact"
        )
    path = mounted_command_receipt_path(record.repo_root, record.receipt_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    existed = path.exists()
    flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND
    flags |= getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags, MOUNT_RECEIPT_MODE)
    except OSError as exc:
        raise SpiceError(
            f"could not open mounted command receipt {path}: {exc}"
        ) from exc
    chmod_after_close = not hasattr(os, "fchmod")
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise SpiceError(f"mounted command receipt is not a regular file: {path}")
        if not chmod_after_close:
            os.fchmod(descriptor, MOUNT_RECEIPT_MODE)
        written = os.write(descriptor, payload)
        if written != len(payload):
            raise SpiceError(
                "short mounted command receipt append: "
                f"wrote {written} of {len(payload)} bytes"
            )
        os.fsync(descriptor)
    except OSError as exc:
        raise SpiceError(
            f"could not append mounted command receipt {path}: {exc}"
        ) from exc
    finally:
        os.close(descriptor)
    if chmod_after_close:
        path.chmod(MOUNT_RECEIPT_MODE)
    if not existed:
        fsync_directory(path.parent)


def load_mounted_plan_receipt(
    repo_root: Path,
    receipt_id: str,
) -> MountedPlanReceipt | None:
    """Replay one mounted command's append-only apply and reversal facts."""
    resolved_root = repo_root.expanduser().resolve()
    path = mounted_command_receipt_path(resolved_root, receipt_id)
    try:
        content = path.read_bytes()
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise SpiceError(
            f"could not read mounted command receipt {path}: {exc}"
        ) from exc
    if not content:
        raise SpiceError(f"invalid mounted command receipt {path}: empty log")
    if not content.endswith(b"\n"):
        raise SpiceError(f"invalid mounted command receipt {path}: unterminated record")

    records: list[MountedReceiptRecord] = []
    for line_number, encoded in enumerate(content.splitlines(keepends=True), start=1):
        if len(encoded) > MOUNT_RECEIPT_RECORD_MAX_BYTES:
            raise SpiceError(
                f"invalid mounted command receipt {path}: record {line_number} "
                f"exceeds {MOUNT_RECEIPT_RECORD_MAX_BYTES} bytes"
            )
        try:
            raw = json.loads(encoded)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise SpiceError(
                f"invalid mounted command receipt {path}: record {line_number}: {exc}"
            ) from exc
        try:
            record = _mounted_receipt_record(raw, resolved_root, receipt_id)
        except (KeyError, TypeError, ValueError, SpiceError) as exc:
            raise SpiceError(
                f"invalid mounted command receipt {path}: record {line_number}: {exc}"
            ) from exc
        records.append(record)
    return _replay_mounted_receipt(records)


def apply_receipted_mounted_plan(
    document: CommandPlanDocument,
    repo_root: Path,
    receipt_id: str,
) -> list[str]:
    """Apply and durably acknowledge each mounted operation in plan order."""
    resolved_root = repo_root.expanduser().resolve()
    receipt = load_mounted_plan_receipt(resolved_root, receipt_id)
    if receipt is not None:
        if receipt.reversing:
            raise SpiceError(
                f"mounted command {receipt_id!r} has an interrupted unapply; "
                "resume with --unapply --apply"
            )
        pending_raw = _receipt_pending_operations(receipt, document)
        command = receipt.command
        plan_digest = receipt.plan_digest
        operation_count = receipt.operation_count
        completed = len(receipt.operations)
    else:
        pending_raw = document.operations
        command = document.command
        plan_digest = document.digest
        operation_count = len(document.operations)
        completed = 0
    mounted = tuple(
        _mounted_operation(operation, resolved_root) for operation in pending_raw
    )
    for operation in mounted:
        _assert_observed_state(resolved_root, operation)

    applied: list[str] = []
    for offset, operation in enumerate(mounted):
        index = completed + offset
        record = MountedReceiptRecord(
            repo_root=resolved_root,
            receipt_id=receipt_id,
            command=command,
            plan_digest=plan_digest,
            event=MountedReceiptEvent.APPLY,
            operation_index=index,
            operation_count=operation_count,
            operation=pending_raw[offset],
        )
        encoded = encode_mounted_receipt_record(record)
        _apply_operation(resolved_root, operation)
        append_mounted_receipt_record(record, encoded=encoded)
        applied.append(operation.outcome_label)
    return applied


def plan_mounted_reversal(
    repo_root: Path,
    receipt_id: str,
    expected_receipt_digest: str | None = None,
) -> MountedReversalPlan:
    """Build one receipt-selected reverse plan without consulting the child."""
    resolved_root = repo_root.expanduser().resolve()
    receipt = load_mounted_plan_receipt(resolved_root, receipt_id)
    if receipt is None:
        document = plan_document(
            command_plan_payload(
                command=f"{receipt_id} --unapply",
                operations=[],
                metadata={
                    "direction": "unapply",
                    "status": "not-initialized",
                    "receipt_digest": None,
                },
            )
        )
        if expected_receipt_digest is not None:
            raise SpiceError(
                f"mounted command receipt digest mismatch: "
                f"expected {expected_receipt_digest}; observed <none>"
            )
        return MountedReversalPlan(
            resolved_root,
            receipt_id,
            None,
            document,
            (),
        )
    if not receipt.complete:
        raise SpiceError(
            f"mounted command {receipt_id!r} has an interrupted apply; "
            "resume with --apply before unapplying"
        )
    receipt_digest = receipt.digest
    if (
        expected_receipt_digest is not None
        and expected_receipt_digest != receipt_digest
    ):
        raise SpiceError(
            "mounted command receipt digest mismatch: "
            f"expected {expected_receipt_digest}; observed {receipt_digest}"
        )

    completed = {record.operation_index: record for record in receipt.reversals}
    states: list[MountedReversalState] = []
    for operation_index in reversed(range(receipt.operation_count)):
        prior = completed.get(operation_index)
        if prior is not None:
            assert prior.outcome is not None
            states.append(
                MountedReversalState(
                    operation_index,
                    prior.operation,
                    prior.outcome,
                    True,
                )
            )
            continue
        states.append(
            _predict_mounted_reversal(
                resolved_root,
                operation_index,
                receipt.operations[operation_index],
            )
        )
    document = plan_document(
        command_plan_payload(
            command=f"{receipt_id} --unapply",
            operations=[state.operation for state in states],
            metadata={
                "direction": "unapply",
                "status": "preview",
                "receipt_digest": receipt_digest,
            },
        )
    )
    normalized_states = tuple(
        MountedReversalState(
            state.operation_index,
            document.operations[position],
            state.outcome,
            state.completed,
        )
        for position, state in enumerate(states)
    )
    return MountedReversalPlan(
        resolved_root,
        receipt_id,
        receipt_digest,
        document,
        normalized_states,
    )


def apply_mounted_reversal(
    plan: MountedReversalPlan,
    expected_plan_digest: str | None,
) -> MountedReversalReport:
    """Apply a mounted reverse plan and resume from its durable outcome prefix."""
    if plan.receipt_digest is None:
        assert_plan_digest(plan.document, expected_plan_digest)
        return MountedReversalReport(plan.receipt_id, None, ())
    current = plan_mounted_reversal(
        plan.repo_root,
        plan.receipt_id,
        plan.receipt_digest,
    )
    receipt = load_mounted_plan_receipt(current.repo_root, current.receipt_id)
    if receipt is None:
        raise SpiceError(
            f"mounted command receipt {current.receipt_id!r} disappeared before unapply"
        )
    assert_plan_digest(current.document, plan.document.digest)
    assert_mounted_plan_digest(
        current.document,
        current.repo_root,
        expected_plan_digest,
    )
    outcomes: list[str] = []
    for position, state in enumerate(current.states):
        label = f"{state.outcome.value}:{_operation_label(state.operation)}"
        outcomes.append(label)
        if state.completed:
            continue
        record = MountedReceiptRecord(
            repo_root=current.repo_root,
            receipt_id=current.receipt_id,
            command=receipt.command,
            plan_digest=receipt.plan_digest,
            event=MountedReceiptEvent.UNAPPLY,
            operation_index=state.operation_index,
            operation_count=len(current.states),
            operation=state.operation,
            outcome=state.outcome,
        )
        encoded = encode_mounted_receipt_record(record)
        if state.outcome is MountedReversalOutcome.RESTORED:
            operation = _mounted_operation(state.operation, current.repo_root)
            _assert_observed_state(current.repo_root, operation)
            _apply_operation(current.repo_root, operation)
        append_mounted_receipt_record(record, encoded=encoded)

    retained = any(
        state.outcome is MountedReversalOutcome.RETAINED_DIVERGED
        for state in current.states
    )
    recovery_path = _write_mounted_recovery(current) if retained else None
    _remove_mounted_receipt(current.repo_root, current.receipt_id)
    return MountedReversalReport(
        current.receipt_id,
        current.receipt_digest,
        tuple(outcomes),
        recovery_path,
    )


def _mounted_receipt_record(
    raw: object,
    repo_root: Path,
    receipt_id: str,
) -> MountedReceiptRecord:
    if not isinstance(raw, dict):
        raise TypeError("record must be an object")
    if raw.get("protocol") != MOUNT_RECEIPT_PROTOCOL:
        raise ValueError(f"unsupported protocol {raw.get('protocol')!r}")
    schema_version = _required_integer(raw.get("schema_version"), "schema_version")
    if schema_version != MOUNT_RECEIPT_SCHEMA_VERSION:
        raise ValueError(f"unsupported schema version {schema_version!r}")
    plan_schema = _required_integer(
        raw.get("plan_schema_version"),
        "plan_schema_version",
    )
    if plan_schema != COMMAND_PLAN_SCHEMA_VERSION:
        raise ValueError(f"unsupported plan schema version {plan_schema!r}")
    repository = (
        Path(_required_text(raw.get("repository"), "repository")).expanduser().resolve()
    )
    if repository != repo_root:
        raise ValueError(f"receipt belongs to a different repository: {repository}")
    observed_id = _required_text(raw.get("receipt_id"), "receipt_id")
    if observed_id != receipt_id:
        raise ValueError(
            f"receipt identity mismatch: expected {receipt_id!r}, "
            f"observed {observed_id!r}"
        )
    event = MountedReceiptEvent(_required_text(raw.get("event"), "event"))
    operation_index = _required_integer(
        raw.get("operation_index"),
        "operation_index",
    )
    operation_count = _required_integer(
        raw.get("operation_count"),
        "operation_count",
    )
    if operation_index < 0 or operation_index >= operation_count:
        raise ValueError("operation position is outside its operation count")
    raw_operation = raw.get("operation")
    if not isinstance(raw_operation, Mapping):
        raise TypeError("operation must be an object")
    operation = {
        str(key): _json_value(value, f"operation.{key}")
        for key, value in raw_operation.items()
    }
    _mounted_operation(operation, repo_root)
    raw_outcome = raw.get("outcome")
    outcome = (
        MountedReversalOutcome(_required_text(raw_outcome, "outcome"))
        if raw_outcome is not None
        else None
    )
    if (event is MountedReceiptEvent.UNAPPLY) != (outcome is not None):
        raise ValueError("only unapply records carry a reversal outcome")
    return MountedReceiptRecord(
        repo_root=repository,
        receipt_id=observed_id,
        command=_required_text(raw.get("command"), "command"),
        plan_digest=_required_digest(raw.get("plan_digest"), "plan_digest"),
        event=event,
        operation_index=operation_index,
        operation_count=operation_count,
        operation=operation,
        outcome=outcome,
        schema_version=schema_version,
    )


def _replay_mounted_receipt(
    records: Sequence[MountedReceiptRecord],
) -> MountedPlanReceipt:
    if not records:
        raise ValueError("mounted receipt has no records")
    first = records[0]
    operations: list[dict[str, Any]] = []
    reversals: list[MountedReceiptRecord] = []
    for record in records:
        if (
            record.repo_root != first.repo_root
            or record.receipt_id != first.receipt_id
            or record.command != first.command
            or record.plan_digest != first.plan_digest
            or record.operation_count != first.operation_count
        ):
            raise SpiceError("mounted command receipt mixes operation contexts")
        if record.event is MountedReceiptEvent.APPLY:
            if reversals:
                raise SpiceError(
                    "mounted command receipt applies after reversal started"
                )
            expected_index = len(operations)
            if record.operation_index != expected_index:
                raise SpiceError(
                    "mounted command receipt apply prefix is not consecutive: "
                    f"expected {expected_index}, observed {record.operation_index}"
                )
            if record.operation.get("order") != expected_index + 1:
                raise SpiceError(
                    "mounted command receipt apply operation order is not consecutive"
                )
            operations.append(record.operation)
            continue
        if len(operations) != first.operation_count:
            raise SpiceError(
                "mounted command receipt reverses an incomplete apply prefix"
            )
        expected_index = first.operation_count - len(reversals) - 1
        if record.operation_index != expected_index:
            raise SpiceError(
                "mounted command receipt reversal is not in strict reverse order: "
                f"expected {expected_index}, observed {record.operation_index}"
            )
        if record.operation.get("order") != len(reversals) + 1:
            raise SpiceError(
                "mounted command receipt reversal operation order is not consecutive"
            )
        reversals.append(record)
    receipt = MountedPlanReceipt(
        repo_root=first.repo_root,
        receipt_id=first.receipt_id,
        command=first.command,
        plan_digest=first.plan_digest,
        operation_count=first.operation_count,
        operations=tuple(operations),
        reversals=tuple(reversals),
    )
    if receipt.complete and receipt.digest != receipt.plan_digest:
        raise SpiceError(
            "mounted command receipt digest does not match its operation vocabulary: "
            f"recorded={receipt.plan_digest} computed={receipt.digest}"
        )
    return receipt


def _receipt_pending_operations(
    receipt: MountedPlanReceipt,
    document: CommandPlanDocument,
) -> tuple[dict[str, Any], ...]:
    if receipt.command != document.command:
        raise SpiceError(
            f"mounted command {receipt.receipt_id!r} plan changed while its "
            f"receipt is active; recorded={receipt.plan_digest} "
            f"observed={document.digest}"
        )
    completed = len(receipt.operations)
    if (
        receipt.plan_digest == document.digest
        and receipt.operation_count == len(document.operations)
        and receipt.operations == document.operations[:completed]
    ):
        return document.operations[completed:]

    completed_by_identity = {
        _operation_identity(operation): operation for operation in receipt.operations
    }
    pending: list[dict[str, Any]] = []
    for operation in document.operations:
        prior = completed_by_identity.get(_operation_identity(operation))
        if prior is None:
            pending.append(operation)
            continue
        observed = _state(operation.get("observed_before"), "observed_before")
        intended = _state(operation.get("intended_after"), "intended_after")
        completed_after = _state(prior.get("intended_after"), "intended_after")
        if observed != completed_after or intended != completed_after:
            raise SpiceError(
                f"mounted command {receipt.receipt_id!r} replanned a completed "
                f"operation incompatibly: {_operation_label(operation)}"
            )
    candidate = tuple(_normalize_operations([*receipt.operations, *pending]))
    if (
        len(candidate) != receipt.operation_count
        or command_plan_digest(candidate) != receipt.plan_digest
    ):
        raise SpiceError(
            f"mounted command {receipt.receipt_id!r} plan changed while its "
            f"receipt is active; recorded={receipt.plan_digest} "
            f"observed={document.digest}"
        )
    return candidate[completed:]


def _operation_identity(operation: Mapping[str, Any]) -> tuple[str, str, str]:
    return (
        _required_text(operation.get("kind"), "operation kind"),
        _required_text(operation.get("scope"), "operation scope"),
        _required_text(operation.get("target"), "operation target"),
    )


def _predict_mounted_reversal(
    repo_root: Path,
    operation_index: int,
    raw: Mapping[str, Any],
) -> MountedReversalState:
    original = _mounted_operation(raw, repo_root)
    observed = _observe_mounted_operation(repo_root, original)
    if not original.managed:
        outcome = MountedReversalOutcome.PRESERVED_UNMANAGED
        intended = observed
    elif observed == original.before:
        outcome = MountedReversalOutcome.ALREADY_RESTORED
        intended = observed
    elif observed != original.after:
        outcome = MountedReversalOutcome.RETAINED_DIVERGED
        intended = observed
    else:
        outcome = MountedReversalOutcome.RESTORED
        intended = original.before
    operation = {
        **{
            key: value
            for key, value in raw.items()
            if key
            not in {
                "order",
                "observed_before",
                "intended_after",
                "managed",
                "predicted_outcome",
            }
        },
        "observed_before": observed,
        "intended_after": intended,
        "managed": outcome is MountedReversalOutcome.RESTORED,
        "predicted_outcome": outcome.value,
    }
    normalized = _normalize_operations([operation])[0]
    return MountedReversalState(
        operation_index,
        normalized,
        outcome,
        False,
    )


def _observe_mounted_operation(
    repo_root: Path,
    operation: _MountedOperation,
) -> dict[str, Any]:
    return (
        _file_state(_mounted_file_path(repo_root, operation.target))
        if operation.kind == "file"
        else _git_config_state(repo_root, operation.scope, operation.target)
    )


def _write_mounted_recovery(plan: MountedReversalPlan) -> Path:
    identity = hashlib.sha256(plan.receipt_id.encode("utf-8")).hexdigest()
    path = worktree_state_path(
        plan.repo_root,
        MOUNT_RECOVERY_DIR
        / f"{identity}-{plan.document.digest[:MOUNT_RECOVERY_DIGEST_LENGTH]}.json",
    )
    atomic_write_json(
        path,
        {
            "protocol": MOUNT_RECEIPT_PROTOCOL,
            "schema_version": MOUNT_RECEIPT_SCHEMA_VERSION,
            "repository": str(plan.repo_root),
            "receipt_id": plan.receipt_id,
            "receipt_digest": plan.receipt_digest,
            "operations": [
                {
                    "operation_index": state.operation_index,
                    "outcome": state.outcome.value,
                    "operation": state.operation,
                }
                for state in plan.states
                if state.outcome is MountedReversalOutcome.RETAINED_DIVERGED
            ],
        },
    )
    path.chmod(MOUNT_RECEIPT_MODE)
    return path


def _remove_mounted_receipt(repo_root: Path, receipt_id: str) -> None:
    path = mounted_command_receipt_path(repo_root, receipt_id)
    try:
        path.unlink()
    except FileNotFoundError:
        return
    except OSError as exc:
        raise SpiceError(
            f"could not remove mounted command receipt {path}: {exc}"
        ) from exc
    fsync_directory(path.parent)
    try:
        path.parent.rmdir()
    except OSError:
        pass


def _required_integer(value: Any, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        raise TypeError(f"{label} must be an integer")
    return value


def _normalize_operations(
    operations: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    if isinstance(operations, (str, bytes)):
        raise SpiceError("command plan operations must be an ordered list")
    normalized: list[dict[str, Any]] = []
    for order, raw in enumerate(operations, start=1):
        if not isinstance(raw, Mapping):
            raise SpiceError(f"command plan operation {order} must be an object")
        operation = {
            str(key): _json_value(value, f"operations[{order}].{key}")
            for key, value in raw.items()
            if str(key) != "order"
        }
        operation["order"] = order
        for field in ("kind", "target", "scope"):
            operation[field] = _required_text(
                operation.get(field), f"command plan operation {order} {field}"
            )
        normalized.append({"order": operation.pop("order"), **operation})
    return normalized


def _json_value(value: Any, label: str) -> Any:
    try:
        return json.loads(json.dumps(value, allow_nan=False, ensure_ascii=False))
    except (TypeError, ValueError) as exc:
        raise SpiceError(f"{label} must be JSON-serializable: {exc}") from exc


def _required_text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise SpiceError(f"{label} must be a non-empty string")
    return value


def _required_digest(value: Any, label: str) -> str:
    digest = _required_text(value, label).lower()
    if len(digest) != PLAN_DIGEST_HEX_LENGTH:
        raise SpiceError(f"{label} must be a hexadecimal SHA-256 digest")
    try:
        bytes.fromhex(digest)
    except ValueError as exc:
        raise SpiceError(f"{label} must be a hexadecimal SHA-256 digest") from exc
    return digest


def _operation_label(operation: Mapping[str, Any]) -> str:
    return (
        f"{operation.get('order')}:{operation.get('kind')}:"
        f"{operation.get('scope')}:{operation.get('target')}"
    )


def _state(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise SpiceError(f"mounted command plan {label} must be an object")
    state = {
        str(key): _json_value(item, f"{label}.{key}") for key, item in value.items()
    }
    if set(state) != {"value", "mode"}:
        raise SpiceError(
            f"mounted command plan {label} must contain exactly value and mode"
        )
    raw_value = state["value"]
    if raw_value is not None and not isinstance(raw_value, str):
        raise SpiceError(f"mounted command plan {label}.value must be text or null")
    mode = state["mode"]
    if mode is not None and (
        not isinstance(mode, int)
        or isinstance(mode, bool)
        or not 0 <= mode <= FILE_MODE_MAX
    ):
        raise SpiceError(f"mounted command plan {label}.mode must be 0..07777 or null")
    return state


def _state_exists(state: Mapping[str, Any]) -> bool:
    return state["value"] is not None or state["mode"] is not None


def _mounted_operation(raw: Mapping[str, Any], repo_root: Path) -> _MountedOperation:
    kind = _required_text(raw.get("kind"), "mounted operation kind")
    if kind not in MOUNT_OPERATION_KINDS:
        raise SpiceError(
            f"mounted command plan operation kind {kind!r} is not applicable by Spice; "
            f"expected one of {', '.join(sorted(MOUNT_OPERATION_KINDS))}"
        )
    target = _required_text(raw.get("target"), "mounted operation target")
    scope = _required_text(raw.get("scope"), "mounted operation scope")
    before = _state(raw.get("observed_before"), "observed_before")
    after = _state(raw.get("intended_after"), "intended_after")
    managed = raw.get("managed", True)
    if not isinstance(managed, bool):
        raise SpiceError("mounted command plan operation managed must be boolean")
    if kind == "file":
        if scope != FILE_SCOPE:
            raise SpiceError(f"mounted file operation has invalid scope {scope!r}")
        _mounted_file_path(repo_root, target)
        for label, state in (("observed_before", before), ("intended_after", after)):
            if (state["value"] is None) != (state["mode"] is None):
                raise SpiceError(
                    f"mounted file {label} value and mode must both be present "
                    "or both be null"
                )
    else:
        if scope not in {COMMON_GIT_CONFIG_SCOPE, WORKTREE_GIT_CONFIG_SCOPE}:
            raise SpiceError(
                f"mounted git-config operation has invalid scope {scope!r}"
            )
        if before["mode"] is not None or after["mode"] is not None:
            raise SpiceError("mounted git-config operation modes must be null")
    return _MountedOperation(kind, target, scope, before, after, managed)


def _mounted_file_path(repo_root: Path, target: str) -> Path:
    relative = Path(target)
    if relative.is_absolute() or ".." in relative.parts:
        raise SpiceError(f"mounted file target escapes the repository: {target!r}")
    root = repo_root.expanduser().resolve()
    resolved = (root / relative).resolve()
    if not resolved.is_relative_to(root):
        raise SpiceError(f"mounted file target escapes the repository: {target!r}")
    return root / relative


def _assert_observed_state(repo_root: Path, operation: _MountedOperation) -> None:
    observed = (
        _file_state(_mounted_file_path(repo_root, operation.target))
        if operation.kind == "file"
        else _git_config_state(repo_root, operation.scope, operation.target)
    )
    if observed != operation.before:
        raise SpiceError(
            f"mounted command plan operation changed before apply: {operation.label}; "
            f"planned={operation.before!r} observed={observed!r}"
        )


def _file_state(path: Path) -> dict[str, Any]:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return {"value": None, "mode": None}
    if not stat.S_ISREG(metadata.st_mode):
        raise SpiceError(f"mounted file target is not a regular file: {path}")
    try:
        value = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise SpiceError(f"cannot observe mounted file target {path}: {exc}") from exc
    return {"value": value, "mode": stat.S_IMODE(metadata.st_mode)}


def _git_config_state(repo_root: Path, scope: str, key: str) -> dict[str, Any]:
    path = (
        git_common_dir(repo_root) / "config"
        if scope == COMMON_GIT_CONFIG_SCOPE
        else git_dir(repo_root) / "config.worktree"
    )
    result = run_git_command(
        ["git", "config", "--file", str(path), "--get", key],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode not in {0, 1}:
        detail = (result.stderr or result.stdout).strip()
        raise SpiceError(f"cannot observe mounted Git config {key}: {detail}")
    value = result.stdout or ""
    if value.endswith("\n"):
        value = value[:-1]
    return {"value": value if result.returncode == 0 else None, "mode": None}


def _apply_operation(repo_root: Path, operation: _MountedOperation) -> None:
    if not operation.managed or operation.before == operation.after:
        return
    if operation.kind == "file":
        _apply_file(_mounted_file_path(repo_root, operation.target), operation.after)
        return
    value = operation.after["value"]
    scope_flag = [] if operation.scope == COMMON_GIT_CONFIG_SCOPE else ["--worktree"]
    action = (
        ["--unset-all", operation.target]
        if value is None
        else [operation.target, value]
    )
    result = run_git_command(
        ["git", "-C", str(repo_root), "config", *scope_flag, *action],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise SpiceError(
            f"cannot apply mounted Git config {operation.target}: {detail}"
        )


def _apply_file(path: Path, state: Mapping[str, Any]) -> None:
    value = state["value"]
    if value is None:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        return
    atomic_write_text(path, str(value), write_if_changed=True)
    path.chmod(int(state["mode"]))
