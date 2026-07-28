"""Versioned command plans shared by native and mounted Spice verbs."""

from __future__ import annotations

import hashlib
import json
import stat
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.paths import atomic_write_text, git_common_dir, git_dir
from spice.process.git import run_git_command

COMMAND_PLAN_PROTOCOL = "spice.command-plan"
COMMAND_PLAN_SCHEMA_VERSION = 1
PLAN_DIGEST_HEX_LENGTH = 64
FILE_SCOPE = "worktree-file"
COMMON_GIT_CONFIG_SCOPE = "common-git-config"
WORKTREE_GIT_CONFIG_SCOPE = "worktree-git-config"
FILE_MODE_MAX = 0o7777
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


def apply_mounted_plan(document: CommandPlanDocument, repo_root: Path) -> list[str]:
    """Apply a mounted plan's initialization-vocabulary operations in order."""
    operations = tuple(
        _mounted_operation(operation, repo_root) for operation in document.operations
    )
    for operation in operations:
        _assert_observed_state(repo_root, operation)
    applied: list[str] = []
    for operation in operations:
        _apply_operation(repo_root, operation)
        applied.append(operation.outcome_label)
    return applied


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
