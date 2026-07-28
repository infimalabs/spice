"""Operator approval for executable tracked repository configuration."""

from __future__ import annotations

import hashlib
import hmac
import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.config.layers import (
    REPOSITORY_SOURCE,
    LayeredConfig,
    load_config,
)
from spice.errors import SpiceError
from spice.paths import git_dir

EXECUTABLE_REPOSITORY_CONFIG_PATHS = (
    ("commands",),
    ("wrappers",),
    ("policy", "pre_commit"),
    ("policy", "pre_commit_success"),
    ("policy", "pre_commit_builtins"),
    ("say", "command"),
    ("judge", "bin"),
    ("rtk", "executable"),
    ("policy", "suite_seam", "run"),
    ("policy", "reachability_providers"),
    ("policy", "python_typecheck_interpreter"),
)


@dataclass(frozen=True)
class RepositoryConfigApproval:
    """The current executable digest and its worktree-local approval state."""

    digest: str
    approved_digest: str | None
    approved: bool


def repository_executable_config_digest(repo_root: Path) -> str:
    """Digest the complete executable surface in the repository layer."""
    loaded = load_config(repo_root.expanduser().resolve())
    surface = _repository_executable_surface(loaded)
    encoded = json.dumps(
        surface,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def repository_config_approval(repo_root: Path) -> RepositoryConfigApproval:
    """Return whether this worktree approved the repository's current digest."""
    resolved_root = repo_root.expanduser().resolve()
    digest = repository_executable_config_digest(resolved_root)
    from spice.hooks.initplan import InitReceiptStatus, load_initialization_receipt

    receipt = load_initialization_receipt(resolved_root)
    approved_digest = (
        receipt.approved_repository_config_digest
        if receipt is not None
        and receipt.repo_root == resolved_root
        and receipt.status is InitReceiptStatus.COMPLETE
        else None
    )
    return RepositoryConfigApproval(
        digest=digest,
        approved_digest=approved_digest,
        approved=(
            approved_digest is not None and hmac.compare_digest(approved_digest, digest)
        ),
    )


def require_repository_config_approval(
    repo_root: Path,
    config_path: Sequence[str],
    *,
    command: str,
) -> None:
    """Refuse one repository-sourced executable until its digest is approved."""
    resolved_root = repo_root.expanduser().resolve()
    path = tuple(config_path)
    loaded = load_config(resolved_root)
    source = loaded.source_for(path)
    if source is None or source.name != REPOSITORY_SOURCE:
        return
    try:
        git_dir(resolved_root)
    except SpiceError as exc:
        if str(exc) == "not inside a git worktree":
            return
        raise

    approval = repository_config_approval(resolved_root)
    if approval.approved:
        return
    state = (
        "has no operator approval"
        if approval.approved_digest is None
        else "changed since operator approval "
        f"(approved={approval.approved_digest} current={approval.digest})"
    )
    dotted = ".".join(path)
    raise SpiceError(
        "repository executable configuration "
        f"{dotted} from {source.path} {state}; refusing command {command}; "
        f"run `spice init --apply` in {resolved_root} to approve digest "
        f"{approval.digest}"
    )


def _repository_executable_surface(loaded: LayeredConfig) -> dict[str, Any]:
    repository = loaded.layer(REPOSITORY_SOURCE).values
    surface: dict[str, Any] = {}
    for path in EXECUTABLE_REPOSITORY_CONFIG_PATHS:
        value = _mapping_value(repository, path)
        if value is not _MISSING:
            surface[".".join(path)] = _plain_value(value)
    return surface


_MISSING = object()


def _mapping_value(values: Mapping[str, Any], path: Sequence[str]) -> object:
    current: object = values
    for part in path:
        if not isinstance(current, Mapping) or part not in current:
            return _MISSING
        current = current[part]
    return current


def _plain_value(value: object) -> object:
    if isinstance(value, Mapping):
        return {str(key): _plain_value(item) for key, item in value.items()}
    if isinstance(value, tuple):
        return [_plain_value(item) for item in value]
    if isinstance(value, list):
        return [_plain_value(item) for item in value]
    return value
