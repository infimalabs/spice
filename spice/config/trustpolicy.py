"""Shared operator authority for executable repository configuration.

The authority log belongs under the common Git directory, never in the work
tree.  Linked worktrees therefore observe the same operator facts, while a
clone receives none of them.
"""

from __future__ import annotations

import json
import os
import stat
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from enum import StrEnum
from pathlib import Path
from types import MappingProxyType

from spice.errors import SpiceError
from spice.locking import bounded_exclusive_lock
from spice.paths import fsync_directory, shared_state_path
from spice.process.git import git_read, git_run

TRUST_LOG_PATH = Path("repository-config-trust.jsonl")
TRUST_LOCK_PATH = Path("repository-config-trust.lock")
TRUST_LOG_SCHEMA_VERSION = 1
TRUST_LOG_MODE = 0o600
TRUST_LOG_NONPRIVATE_MASK = 0o077
TRUST_RECORD_MAX_BYTES = 64 * 1024
TRUST_LOCK_TIMEOUT_SECONDS = 5.0
SHA256_DIGEST_BYTES = 32
GIT_OBJECT_ID_BYTES = frozenset({20, SHA256_DIGEST_BYTES})


class TrustEvent(StrEnum):
    """Append-only repository trust facts."""

    EXACT = "exact"
    GRANT = "grant"
    DERIVE = "derive"
    REVOKE = "revoke"


@dataclass(frozen=True)
class StandingTrustGrant:
    """One active operator-authored provenance delegation."""

    grant_id: str
    repository_url: str
    remote: str
    ref: str
    anchor_commit: str
    capabilities: tuple[str, ...]
    trusted_signers: tuple[str, ...]


@dataclass(frozen=True)
class CommitSignature:
    """Git's verification result for one commit."""

    verified: bool
    fingerprint: str
    signer: str
    detail: str


@dataclass(frozen=True)
class RepositoryTrustState:
    """The exact and delegated authority replayed from the shared log."""

    exact_approvals: Mapping[str, frozenset[str]]
    active_grant: StandingTrustGrant | None
    delegated_approvals: Mapping[str, frozenset[str]]
    record_count: int

    def approves(self, capability: str, digest: str) -> bool:
        return digest in self.exact_approvals.get(
            capability, ()
        ) or digest in self.delegated_approvals.get(capability, ())


def repository_trust_log_path(repo_root: Path) -> Path:
    """Return the repository-shared operator authority log."""
    return shared_state_path(repo_root.expanduser().resolve(), TRUST_LOG_PATH)


def _trust_lock(repo_root: Path, *, action: str):
    return bounded_exclusive_lock(
        shared_state_path(repo_root.expanduser().resolve(), TRUST_LOCK_PATH),
        timeout_seconds=TRUST_LOCK_TIMEOUT_SECONDS,
        action=action,
    )


def load_repository_trust_state(repo_root: Path) -> RepositoryTrustState:
    """Validate and replay every shared authority fact."""
    with _trust_lock(repo_root, action="read repository configuration authority"):
        return _load_repository_trust_state_unlocked(repo_root)


def _load_repository_trust_state_unlocked(
    repo_root: Path,
) -> RepositoryTrustState:
    records = _load_records(repo_root)
    exact: dict[str, set[str]] = {}
    delegated: dict[str, set[str]] = {}
    active: StandingTrustGrant | None = None
    for record in records:
        event = _trust_event(record)
        if event is TrustEvent.EXACT:
            _optional_object_id(record, "commit")
            if record.get("source") is not None:
                _required_text(record, "source")
            _merge_approvals(exact, _approval_mapping(record))
            continue
        if event is TrustEvent.GRANT:
            if active is not None:
                raise SpiceError(
                    "repository configuration trust log replaces an active "
                    "grant without revocation"
                )
            active = _grant_from_record(record)
            delegated = _mutable_approvals(_approval_mapping(record))
            continue
        if event is TrustEvent.REVOKE:
            _required_text(record, "reason")
            grant_id = (
                _required_digest(record, "grant_id")
                if record.get("grant_id") is not None
                else None
            )
            if grant_id is not None and (active is None or grant_id != active.grant_id):
                raise SpiceError(
                    "repository configuration trust log revokes an inactive "
                    f"grant: {grant_id}"
                )
            exact = {}
            active = None
            delegated = {}
            continue
        grant_id = _required_digest(record, "grant_id")
        if active is None or grant_id != active.grant_id:
            raise SpiceError(
                "repository configuration trust log references an inactive "
                f"grant: {grant_id}"
            )
        _required_object_id(record, "commit")
        approvals = _validate_derivation_record(record, active)
        _merge_approvals(delegated, approvals)
    return RepositoryTrustState(
        exact_approvals=_frozen_approvals(exact),
        active_grant=active,
        delegated_approvals=_frozen_approvals(delegated),
        record_count=len(records),
    )


def record_exact_approvals(
    repo_root: Path,
    approvals: Mapping[str, str],
    *,
    commit: str | None,
    source: str,
) -> None:
    """Append only exact capability digests not already approved exactly."""
    with _trust_lock(repo_root, action="record exact repository authority"):
        state = _load_repository_trust_state_unlocked(repo_root)
        missing = {
            capability: digest
            for capability, digest in sorted(approvals.items())
            if digest not in state.exact_approvals.get(capability, ())
        }
        if not missing:
            return
        record = {
            "schema_version": TRUST_LOG_SCHEMA_VERSION,
            "event": TrustEvent.EXACT.value,
            "recorded_at": _utc_now(),
            "commit": commit,
            "source": source,
            "approvals": missing,
        }
        _optional_object_id(record, "commit")
        _required_text(record, "source")
        _approval_mapping(record)
        _append_record(repo_root, record)


def record_standing_grant(
    repo_root: Path,
    grant: StandingTrustGrant,
    approvals: Mapping[str, str],
    *,
    expected_record_count: int,
) -> None:
    """Append one explicit standing grant and its exact anchor approvals."""
    record = {
        "schema_version": TRUST_LOG_SCHEMA_VERSION,
        "event": TrustEvent.GRANT.value,
        "recorded_at": _utc_now(),
        "grant_id": grant.grant_id,
        "repository_url": grant.repository_url,
        "remote": grant.remote,
        "ref": grant.ref,
        "anchor_commit": grant.anchor_commit,
        "capabilities": list(grant.capabilities),
        "trusted_signers": list(grant.trusted_signers),
        "approvals": dict(sorted(approvals.items())),
    }
    _grant_from_record(record)
    with _trust_lock(repo_root, action="record standing repository authority"):
        state = _load_repository_trust_state_unlocked(repo_root)
        if state.active_grant is not None:
            raise SpiceError(
                "repository configuration already has an active standing grant"
            )
        _require_record_count(state, expected_record_count)
        _append_record(repo_root, record)


def record_derived_approvals(
    repo_root: Path,
    grant: StandingTrustGrant,
    *,
    commit: str,
    approvals: Mapping[str, str],
    signatures: Sequence[tuple[str, CommitSignature]],
) -> None:
    """Append one auditable delegation from a standing grant."""
    if not approvals:
        return
    if any(not signature.verified for _commit, signature in signatures):
        raise SpiceError("cannot record unverified repository authority evidence")
    record = {
        "schema_version": TRUST_LOG_SCHEMA_VERSION,
        "event": TrustEvent.DERIVE.value,
        "recorded_at": _utc_now(),
        "grant_id": grant.grant_id,
        "commit": commit,
        "approvals": dict(sorted(approvals.items())),
        "signatures": [
            {
                "commit": signed_commit,
                "fingerprint": signature.fingerprint,
                "signer": signature.signer,
            }
            for signed_commit, signature in signatures
        ],
    }
    _required_object_id(record, "commit")
    _validate_derivation_record(record, grant)
    with _trust_lock(repo_root, action="record derived repository authority"):
        state = _load_repository_trust_state_unlocked(repo_root)
        if state.active_grant != grant:
            raise SpiceError(
                "repository configuration standing grant changed before "
                "derived authority could be recorded"
            )
        if all(state.approves(name, digest) for name, digest in approvals.items()):
            return
        _append_record(repo_root, record)


def record_standing_revocation(
    repo_root: Path,
    grant: StandingTrustGrant | None,
    *,
    reason: str,
    expected_record_count: int,
) -> None:
    """Revoke all exact and delegated authority without erasing audit history."""
    record = {
        "schema_version": TRUST_LOG_SCHEMA_VERSION,
        "event": TrustEvent.REVOKE.value,
        "recorded_at": _utc_now(),
        "grant_id": grant.grant_id if grant is not None else None,
        "reason": reason,
    }
    _required_text(record, "reason")
    with _trust_lock(repo_root, action="revoke repository authority"):
        state = _load_repository_trust_state_unlocked(repo_root)
        if grant is not None and state.active_grant != grant:
            raise SpiceError(
                "repository configuration standing grant changed before revocation"
            )
        _require_record_count(state, expected_record_count)
        _append_record(repo_root, record)


def commit_signature(repo_root: Path, commit: str) -> CommitSignature:
    """Verify one commit and return its stable signer identity."""
    verified = git_run(repo_root, "verify-commit", "--raw", commit)
    rendered = git_run(
        repo_root,
        "log",
        "-1",
        "--format=%G?%x00%GF%x00%GS",
        commit,
    )
    if rendered.returncode != 0:
        detail = (rendered.stderr or rendered.stdout).strip()
        return CommitSignature(False, "", "", detail or "signature unreadable")
    fields = rendered.stdout.rstrip("\n").split("\0")
    if len(fields) != 3:
        return CommitSignature(False, "", "", "signature identity is ambiguous")
    status, fingerprint, signer = fields
    detail = (verified.stderr or verified.stdout).strip()
    return CommitSignature(
        verified=verified.returncode == 0 and status in {"G", "U"},
        fingerprint=fingerprint.strip(),
        signer=signer.strip(),
        detail=detail or f"status={status}",
    )


def current_commit(repo_root: Path) -> str:
    """Resolve the worktree's exact commit, or refuse ambiguous HEAD state."""
    commit = git_read(repo_root, "rev-parse", "HEAD^{commit}")
    if not commit:
        raise SpiceError("repository configuration provenance has no resolvable HEAD")
    return commit


def resolved_commit(repo_root: Path, ref: str) -> str:
    """Resolve one trusted ref to an exact commit."""
    commit = git_read(repo_root, "rev-parse", f"{ref}^{{commit}}")
    if not commit:
        raise SpiceError(
            f"repository configuration provenance ref {ref!r} is unresolvable"
        )
    return commit


def is_ancestor(repo_root: Path, ancestor: str, descendant: str) -> bool:
    """Whether Git proves one commit is an ancestor of another."""
    result = git_run(
        repo_root,
        "merge-base",
        "--is-ancestor",
        ancestor,
        descendant,
    )
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    detail = (result.stderr or result.stdout).strip()
    raise SpiceError(
        "could not inspect repository configuration ancestry: "
        f"{detail or f'git exited {result.returncode}'}"
    )


def commit_parent_rows(
    repo_root: Path, anchor_commit: str, commit: str
) -> tuple[tuple[str, tuple[str, ...]], ...]:
    """Return every candidate provenance commit and its parents."""
    rendered = git_run(
        repo_root,
        "rev-list",
        "--reverse",
        "--topo-order",
        "--parents",
        f"{anchor_commit}..{commit}",
    )
    if rendered.returncode != 0:
        detail = (rendered.stderr or rendered.stdout).strip()
        raise SpiceError(
            "could not inspect repository configuration commit provenance: "
            f"{detail or f'git exited {rendered.returncode}'}"
        )
    rows: list[tuple[str, tuple[str, ...]]] = []
    for line in rendered.stdout.splitlines():
        parts = line.split()
        if parts:
            rows.append((parts[0], tuple(parts[1:])))
    return tuple(rows)


def tracked_config_matches_head(repo_root: Path) -> bool:
    """Whether root spice.toml is tracked and byte-identical to HEAD."""
    path = repo_root / "spice.toml"
    try:
        metadata = path.lstat()
    except OSError:
        return False
    if not stat.S_ISREG(metadata.st_mode):
        return False
    tracked = git_run(
        repo_root,
        "ls-files",
        "--error-unmatch",
        "--",
        "spice.toml",
    )
    if tracked.returncode != 0:
        return False
    status = git_run(
        repo_root,
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--",
        "spice.toml",
    )
    worktree_blob = git_read(
        repo_root,
        "hash-object",
        "--no-filters",
        "--",
        "spice.toml",
    )
    head_blob = git_read(repo_root, "rev-parse", "HEAD:spice.toml")
    return (
        status.returncode == 0
        and not status.stdout.strip()
        and bool(worktree_blob)
        and worktree_blob == head_blob
    )


def config_at_commit(repo_root: Path, commit: str) -> str | None:
    """Read tracked repository configuration at one commit."""
    rendered = git_run(repo_root, "show", f"{commit}:spice.toml")
    if rendered.returncode == 0:
        return rendered.stdout
    missing = git_run(repo_root, "cat-file", "-e", f"{commit}:spice.toml")
    if missing.returncode != 0:
        return None
    detail = (rendered.stderr or rendered.stdout).strip()
    raise SpiceError(
        "could not read repository configuration provenance at "
        f"{commit}: {detail or f'git exited {rendered.returncode}'}"
    )


def _load_records(repo_root: Path) -> tuple[dict[str, object], ...]:
    path = repository_trust_log_path(repo_root)
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return ()
    except OSError as exc:
        raise SpiceError(
            f"could not inspect repository trust log {path}: {exc}"
        ) from exc
    if not stat.S_ISREG(metadata.st_mode):
        raise SpiceError(f"repository configuration trust log is not regular: {path}")
    if stat.S_IMODE(metadata.st_mode) & TRUST_LOG_NONPRIVATE_MASK:
        raise SpiceError(
            f"repository configuration trust log is not private mode 0600: {path}"
        )
    try:
        content = path.read_bytes()
    except OSError as exc:
        raise SpiceError(f"could not read repository trust log {path}: {exc}") from exc
    if content and not content.endswith(b"\n"):
        raise SpiceError(
            f"invalid repository configuration trust log {path}: truncated"
        )
    records: list[dict[str, object]] = []
    for line_number, encoded in enumerate(content.splitlines(), start=1):
        records.append(_decode_record(path, line_number, encoded))
    return tuple(records)


def _decode_record(path: Path, line_number: int, encoded: bytes) -> dict[str, object]:
    if len(encoded) > TRUST_RECORD_MAX_BYTES:
        raise SpiceError(
            f"invalid repository configuration trust log {path}: "
            f"record {line_number} exceeds {TRUST_RECORD_MAX_BYTES} bytes"
        )
    try:
        record = json.loads(encoded)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SpiceError(
            f"invalid repository configuration trust log {path}: "
            f"record {line_number}: {exc}"
        ) from exc
    if not isinstance(record, dict):
        raise SpiceError(
            f"invalid repository configuration trust log {path}: "
            f"record {line_number} must be an object"
        )
    if record.get("schema_version") != TRUST_LOG_SCHEMA_VERSION:
        raise SpiceError(
            f"invalid repository configuration trust log {path}: "
            f"record {line_number} has unsupported schema"
        )
    _required_text(record, "recorded_at")
    return record


def _trust_event(record: Mapping[str, object]) -> TrustEvent:
    value = _required_text(record, "event")
    try:
        return TrustEvent(value)
    except ValueError as exc:
        raise SpiceError(
            f"repository configuration trust event is unsupported: {value}"
        ) from exc


def _append_record(repo_root: Path, record: Mapping[str, object]) -> None:
    encoded = (
        json.dumps(record, separators=(",", ":"), sort_keys=True).encode("utf-8")
        + b"\n"
    )
    if len(encoded) > TRUST_RECORD_MAX_BYTES:
        raise SpiceError(
            "repository configuration trust record exceeds encoded byte bound: "
            f"{len(encoded)} > {TRUST_RECORD_MAX_BYTES}"
        )
    path = repository_trust_log_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    existed = path.exists()
    flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND
    flags |= getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags, TRUST_LOG_MODE)
    except OSError as exc:
        raise SpiceError(f"could not open repository trust log {path}: {exc}") from exc
    chmod_after_close = not hasattr(os, "fchmod")
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise SpiceError(
                f"repository configuration trust log is not regular: {path}"
            )
        if not chmod_after_close:
            os.fchmod(descriptor, TRUST_LOG_MODE)
        written = os.write(descriptor, encoded)
        if written != len(encoded):
            raise SpiceError(
                f"short repository trust append: wrote {written} of {len(encoded)}"
            )
        os.fsync(descriptor)
    except OSError as exc:
        raise SpiceError(
            f"could not append repository trust log {path}: {exc}"
        ) from exc
    finally:
        os.close(descriptor)
    if chmod_after_close:
        path.chmod(TRUST_LOG_MODE)
    if not existed:
        fsync_directory(path.parent)


def _grant_from_record(record: Mapping[str, object]) -> StandingTrustGrant:
    grant = StandingTrustGrant(
        grant_id=_required_digest(record, "grant_id"),
        repository_url=_required_text(record, "repository_url"),
        remote=_required_text(record, "remote"),
        ref=_required_text(record, "ref"),
        anchor_commit=_required_object_id(record, "anchor_commit"),
        capabilities=_required_text_tuple(record, "capabilities"),
        trusted_signers=_required_text_tuple(record, "trusted_signers"),
    )
    approvals = _approval_mapping(record)
    if set(approvals) != set(grant.capabilities):
        raise SpiceError(
            "repository configuration standing grant approvals do not match "
            "its capabilities"
        )
    return grant


def _approval_mapping(record: Mapping[str, object]) -> dict[str, str]:
    raw = record.get("approvals")
    if not isinstance(raw, dict) or not raw:
        raise SpiceError(
            "repository configuration trust record approvals must be a non-empty object"
        )
    return {
        _required_plain_text(key, "approval capability"): _required_digest_value(
            value, "approval digest"
        )
        for key, value in raw.items()
    }


def _validate_derivation_record(
    record: Mapping[str, object],
    grant: StandingTrustGrant,
) -> dict[str, str]:
    approvals = _approval_mapping(record)
    outside_grant = sorted(set(approvals).difference(grant.capabilities))
    if outside_grant:
        raise SpiceError(
            "derived repository configuration approval exceeds its grant: "
            + ", ".join(outside_grant)
        )
    fingerprints = _validate_signatures(record)
    untrusted = sorted(set(fingerprints).difference(grant.trusted_signers))
    if untrusted:
        raise SpiceError(
            "derived repository configuration approval names untrusted signer: "
            + ", ".join(untrusted)
        )
    return approvals


def _validate_signatures(record: Mapping[str, object]) -> tuple[str, ...]:
    raw = record.get("signatures")
    if not isinstance(raw, list) or not raw:
        raise SpiceError(
            "derived repository configuration approval has no signature evidence"
        )
    fingerprints: list[str] = []
    commits: set[str] = set()
    for signature in raw:
        if not isinstance(signature, dict):
            raise SpiceError(
                "derived repository configuration signature must be an object"
            )
        commit = _required_object_id(signature, "commit")
        fingerprint = _required_text(signature, "fingerprint")
        _required_text(signature, "signer")
        if commit in commits:
            raise SpiceError(
                "derived repository configuration signature evidence "
                f"duplicates commit {commit}"
            )
        commits.add(commit)
        fingerprints.append(fingerprint)
    return tuple(fingerprints)


def _required_text(record: Mapping[str, object], key: str) -> str:
    return _required_plain_text(record.get(key), key)


def _required_plain_text(value: object, label: str) -> str:
    if isinstance(value, str) and value.strip():
        return value
    raise SpiceError(f"repository configuration trust {label} must be non-empty text")


def _required_text_tuple(record: Mapping[str, object], key: str) -> tuple[str, ...]:
    raw = record.get(key)
    if not isinstance(raw, list) or not raw:
        raise SpiceError(
            f"repository configuration trust {key} must be a non-empty list"
        )
    values = tuple(_required_plain_text(value, key) for value in raw)
    if len(set(values)) != len(values):
        raise SpiceError(f"repository configuration trust {key} contains duplicates")
    return values


def _required_digest(record: Mapping[str, object], key: str) -> str:
    return _required_digest_value(record.get(key), key)


def _required_object_id(record: Mapping[str, object], key: str) -> str:
    value = _required_plain_text(record.get(key), key)
    try:
        decoded = bytes.fromhex(value)
    except ValueError as exc:
        raise SpiceError(
            f"repository configuration trust {key} must be a Git object ID"
        ) from exc
    if len(decoded) not in GIT_OBJECT_ID_BYTES:
        raise SpiceError(
            f"repository configuration trust {key} must be a Git object ID"
        )
    return value


def _optional_object_id(record: Mapping[str, object], key: str) -> str | None:
    if record.get(key) is None:
        return None
    return _required_object_id(record, key)


def _required_digest_value(value: object, label: str) -> str:
    digest = _required_plain_text(value, label)
    try:
        decoded = bytes.fromhex(digest)
    except ValueError as exc:
        raise SpiceError(
            f"repository configuration trust {label} must be hexadecimal SHA-256"
        ) from exc
    if len(decoded) != SHA256_DIGEST_BYTES:
        raise SpiceError(
            f"repository configuration trust {label} must be hexadecimal SHA-256"
        )
    return digest


def _merge_approvals(target: dict[str, set[str]], approvals: Mapping[str, str]) -> None:
    for capability, digest in approvals.items():
        target.setdefault(capability, set()).add(digest)


def _mutable_approvals(approvals: Mapping[str, str]) -> dict[str, set[str]]:
    target: dict[str, set[str]] = {}
    _merge_approvals(target, approvals)
    return target


def _frozen_approvals(
    approvals: Mapping[str, set[str]],
) -> Mapping[str, frozenset[str]]:
    return MappingProxyType(
        {
            capability: frozenset(digests)
            for capability, digests in sorted(approvals.items())
        }
    )


def _utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def _require_record_count(
    state: RepositoryTrustState,
    expected_record_count: int,
) -> None:
    if state.record_count != expected_record_count:
        raise SpiceError(
            "repository configuration authority changed after plan preview: "
            f"expected records={expected_record_count} "
            f"observed={state.record_count}; preview the plan again"
        )
