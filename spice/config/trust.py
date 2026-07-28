"""Operator approval for executable tracked repository configuration."""

from __future__ import annotations

import hashlib
import hmac
import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.commandplan import command_plan_payload
from spice.config.layers import (
    REPOSITORY_SOURCE,
    LayeredConfig,
    load_config,
    parse_config_text,
)
from spice.config.trustpolicy import (
    CommitSignature,
    RepositoryTrustState,
    StandingTrustGrant,
    commit_parent_rows,
    commit_signature,
    config_at_commit,
    current_commit,
    is_ancestor,
    load_repository_trust_state,
    record_derived_approvals,
    record_exact_approvals,
    record_standing_grant,
    record_standing_revocation,
    repository_trust_log_path,
    resolved_commit,
    tracked_config_matches_head,
)
from spice.errors import SpiceError
from spice.locking import bounded_exclusive_lock
from spice.paths import atomic_write_json, git_dir, shared_state_path
from spice.process.git import git_read

SHARED_AUTHORITY_MIGRATION_RELEASE = "v0.31.0"
SHARED_AUTHORITY_MIGRATION_LOCK_SECONDS = 5.0
SHARED_AUTHORITY_MIGRATION_PATH = (
    Path("migrations")
    / f"{SHARED_AUTHORITY_MIGRATION_RELEASE}-repository-config-authority.json"
)
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
EXECUTABLE_REPOSITORY_CAPABILITIES = tuple(
    ".".join(path) for path in EXECUTABLE_REPOSITORY_CONFIG_PATHS
)


@dataclass(frozen=True)
class RepositoryConfigApproval:
    """The complete executable surface and its effective approval state."""

    digest: str
    approved_digest: str | None
    approved: bool


@dataclass(frozen=True)
class RepositoryConfigPathApproval:
    """One capability digest and its authority evidence."""

    capability: str
    digest: str
    approved: bool
    authority: str | None
    refusal: str | None


@dataclass(frozen=True)
class ExactRepositoryConfigApproval:
    """One immutable executable-config snapshot offered for exact approval."""

    digest: str
    capability_digests: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class StandingTrustPlan:
    """One operator-readable standing-grant mutation."""

    repo_root: Path
    action: str
    grant: StandingTrustGrant | None
    approvals: Mapping[str, str]
    reason: str | None
    payload: Mapping[str, Any]
    record_count: int


def repository_executable_config_digests(repo_root: Path) -> dict[str, str]:
    """Digest each independently executable repository capability."""
    loaded = load_config(repo_root.expanduser().resolve())
    return _capability_digests(_repository_executable_surface(loaded))


def plan_exact_repository_config_approval(
    repo_root: Path,
) -> ExactRepositoryConfigApproval:
    """Capture one internally consistent executable-config approval snapshot."""
    loaded = load_config(repo_root.expanduser().resolve())
    surface = _repository_executable_surface(loaded)
    return ExactRepositoryConfigApproval(
        digest=_surface_digest(surface),
        capability_digests=tuple(_capability_digests(surface).items()),
    )


def repository_config_approval(repo_root: Path) -> RepositoryConfigApproval:
    """Return whether every current executable capability is approved."""
    resolved_root = repo_root.expanduser().resolve()
    snapshot = plan_exact_repository_config_approval(resolved_root)
    digest = snapshot.digest
    capabilities = dict(snapshot.capability_digests)
    if not capabilities:
        return RepositoryConfigApproval(digest, None, False)
    state = _shared_authority_state(resolved_root)
    state, refusal = _refresh_standing_approvals(
        resolved_root, capabilities, state=state
    )
    approved = all(
        capability_digest in state.exact_approvals.get(capability, ())
        or (
            refusal is None
            and capability_digest in state.delegated_approvals.get(capability, ())
        )
        for capability, capability_digest in capabilities.items()
    )
    return RepositoryConfigApproval(
        digest=digest,
        approved_digest=digest if approved else None,
        approved=approved,
    )


def repository_config_path_approval(
    repo_root: Path,
    config_path: Sequence[str],
) -> RepositoryConfigPathApproval:
    """Resolve exact or delegated authority for one executable capability."""
    resolved_root = repo_root.expanduser().resolve()
    capability_path = _declared_capability_path(tuple(config_path))
    capability = ".".join(capability_path)
    digests = repository_executable_config_digests(resolved_root)
    digest = digests.get(capability)
    if digest is None:
        raise SpiceError(
            f"repository executable configuration capability {capability} is absent"
        )
    state = _shared_authority_state(resolved_root)
    if digest in state.exact_approvals.get(capability, ()):
        return RepositoryConfigPathApproval(
            capability, digest, True, "shared exact digest", None
        )
    state, reason = _refresh_standing_approvals(
        resolved_root,
        {capability: digest},
        state=state,
    )
    if reason is None and digest in state.delegated_approvals.get(capability, ()):
        return RepositoryConfigPathApproval(
            capability, digest, True, "standing provenance grant", None
        )
    return RepositoryConfigPathApproval(
        capability, digest, False, None, reason or "has no operator approval"
    )


def repository_config_trust_state(repo_root: Path) -> RepositoryTrustState:
    """Return current shared authority after the one-release migration."""
    return _shared_authority_state(repo_root.expanduser().resolve())


def require_repository_config_approval(
    repo_root: Path,
    config_path: Sequence[str],
    *,
    command: str,
) -> None:
    """Refuse one repository-sourced executable until its capability is approved."""
    resolved_root = repo_root.expanduser().resolve()
    path = tuple(config_path)
    _declared_capability_path(path)
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

    approval = repository_config_path_approval(resolved_root, path)
    if approval.approved:
        return
    dotted = ".".join(path)
    reason = approval.refusal or "has no operator approval"
    raise SpiceError(
        "repository executable configuration "
        f"{dotted} from {source.path} {reason}; refusing command {command}; "
        f"run `spice init --apply` in {resolved_root} to approve exact capability "
        f"{approval.capability} digest {approval.digest}, or preview a standing "
        "grant with `spice config trust grant --signer <fingerprint>`"
    )


def require_planned_repository_config_approval_current(
    repo_root: Path,
    approval: ExactRepositoryConfigApproval,
) -> None:
    """Refuse when executable config no longer matches an operator-visible plan."""
    resolved_root = repo_root.expanduser().resolve()
    _shared_authority_state(resolved_root)
    current = plan_exact_repository_config_approval(resolved_root)
    if current != approval:
        raise SpiceError(
            "repository executable configuration changed after the exact approval "
            "plan was created; preview `spice init` again"
        )


def record_planned_repository_config_approval(
    repo_root: Path,
    approval: ExactRepositoryConfigApproval,
    *,
    source: str,
) -> None:
    """Record only the capability digests named by an accepted exact plan."""
    resolved_root = repo_root.expanduser().resolve()
    require_planned_repository_config_approval_current(resolved_root, approval)
    approvals = dict(approval.capability_digests)
    if not approvals:
        return
    record_exact_approvals(
        resolved_root,
        approvals,
        commit=git_read(resolved_root, "rev-parse", "HEAD^{commit}") or None,
        source=source,
    )


def plan_standing_repository_trust(
    repo_root: Path,
    *,
    capabilities: Sequence[str],
    trusted_signers: Sequence[str],
) -> StandingTrustPlan:
    """Plan an opt-in common-Git-dir standing provenance grant."""
    resolved_root = repo_root.expanduser().resolve()
    signers = _normalized_nonempty(trusted_signers, "trusted signer")
    current = repository_executable_config_digests(resolved_root)
    selected = _selected_capabilities(capabilities, current)
    remote, ref = _trusted_upstream(resolved_root)
    repository_url = _remote_url(resolved_root, remote)
    anchor = current_commit(resolved_root)
    if resolved_commit(resolved_root, ref) != anchor:
        raise SpiceError(
            "standing repository configuration trust requires HEAD to equal "
            f"its trusted ref {ref}; advance or reconcile the lane first"
        )
    if not tracked_config_matches_head(resolved_root):
        raise SpiceError(
            "standing repository configuration trust requires tracked, clean "
            "spice.toml bytes identical to HEAD"
        )
    approvals = {capability: current[capability] for capability in selected}
    grant = _standing_grant(
        repository_url=repository_url,
        remote=remote,
        ref=ref,
        anchor_commit=anchor,
        capabilities=selected,
        trusted_signers=signers,
    )
    state = _shared_authority_state(resolved_root)
    if state.active_grant is not None:
        raise SpiceError(
            "revoke the active repository configuration standing grant before "
            "authoring a replacement"
        )
    payload = _grant_plan_payload(
        resolved_root,
        grant,
        approvals,
        record_count=state.record_count,
    )
    return StandingTrustPlan(
        resolved_root,
        "grant",
        grant,
        approvals,
        None,
        payload,
        state.record_count,
    )


def plan_standing_repository_revocation(
    repo_root: Path,
    *,
    reason: str,
) -> StandingTrustPlan:
    """Plan revocation of all current common-Git-dir authority."""
    resolved_root = repo_root.expanduser().resolve()
    state = _shared_authority_state(resolved_root)
    grant = state.active_grant
    if grant is None and not state.exact_approvals and not state.delegated_approvals:
        raise SpiceError("repository configuration has no active authority to revoke")
    normalized_reason = reason.strip()
    if not normalized_reason:
        raise SpiceError("standing repository configuration revocation needs a reason")
    payload = _revoke_plan_payload(
        resolved_root,
        grant,
        normalized_reason,
        record_count=state.record_count,
    )
    return StandingTrustPlan(
        resolved_root,
        "revoke",
        grant,
        {},
        normalized_reason,
        payload,
        state.record_count,
    )


def apply_standing_trust_plan(plan: StandingTrustPlan) -> None:
    """Append the already-authorized standing grant or revocation fact."""
    if plan.action == "grant":
        if plan.grant is None:
            raise SpiceError("standing trust grant plan is missing its grant")
        record_standing_grant(
            plan.repo_root,
            plan.grant,
            plan.approvals,
            expected_record_count=plan.record_count,
        )
        return
    if plan.action == "revoke" and plan.reason is not None:
        record_standing_revocation(
            plan.repo_root,
            plan.grant,
            reason=plan.reason,
            expected_record_count=plan.record_count,
        )
        return
    raise SpiceError(f"unsupported standing trust plan action {plan.action!r}")


def standing_trust_plan_rows(plan: StandingTrustPlan) -> tuple[str, ...]:
    """Render one concise preview/apply handoff."""
    rows = [
        f"repository-config-trust action={plan.action} "
        f"digest={plan.payload['plan_digest']}",
        f"authority-path={repository_trust_log_path(plan.repo_root)}",
    ]
    if plan.grant is not None:
        rows.extend(
            (
                f"grant={plan.grant.grant_id}",
                f"repository-url={plan.grant.repository_url}",
                f"trusted-ref={plan.grant.ref}",
                f"anchor-commit={plan.grant.anchor_commit}",
                f"capabilities={','.join(plan.grant.capabilities)}",
                f"trusted-signers={','.join(plan.grant.trusted_signers)}",
            )
        )
    if plan.reason is not None:
        rows.append(f"reason={plan.reason}")
    rows.append("preview: no changes applied; pass --apply to execute")
    return tuple(rows)


def _refresh_standing_approvals(
    repo_root: Path,
    current: Mapping[str, str],
    *,
    state: RepositoryTrustState | None = None,
) -> tuple[RepositoryTrustState, str | None]:
    effective = state or load_repository_trust_state(repo_root)
    delegated = {
        capability: digest
        for capability, digest in current.items()
        if digest not in effective.exact_approvals.get(capability, ())
    }
    if not delegated:
        return effective, None
    grant = effective.active_grant
    if grant is None:
        return effective, _exact_approval_refusal(effective, current)
    excluded = sorted(set(delegated).difference(grant.capabilities))
    if excluded:
        return (
            effective,
            "standing grant excludes capability " + ", ".join(excluded),
        )
    refusal = _provenance_refusal(repo_root, grant)
    if refusal is not None:
        return effective, refusal
    missing = {
        capability: digest
        for capability, digest in delegated.items()
        if digest not in effective.delegated_approvals.get(capability, ())
    }
    if not missing:
        return effective, None
    try:
        signatures, refusal = _relevant_signatures(repo_root, grant, tuple(missing))
    except SpiceError as exc:
        return effective, f"has provenance-ambiguous commit history ({exc})"
    if refusal is not None:
        return effective, refusal
    head = current_commit(repo_root)
    record_derived_approvals(
        repo_root,
        grant,
        commit=head,
        approvals=missing,
        signatures=signatures,
    )
    return load_repository_trust_state(repo_root), None


def _provenance_refusal(
    repo_root: Path,
    grant: StandingTrustGrant,
) -> str | None:
    try:
        if _remote_url(repo_root, grant.remote) != grant.repository_url:
            return "has untrusted repository remote provenance"
        configured = _trusted_upstream(repo_root)
        head = current_commit(repo_root)
        trusted = resolved_commit(repo_root, grant.ref)
        descends_from_anchor = is_ancestor(repo_root, grant.anchor_commit, trusted)
    except SpiceError as exc:
        return f"has provenance-ambiguous Git state ({exc})"
    if configured != (grant.remote, grant.ref):
        return "has provenance-ambiguous upstream routing"
    if not descends_from_anchor:
        return "diverged from the standing grant anchor"
    if head != trusted:
        return (
            "has agent-local or divergent HEAD; authority is never inferred "
            "from agent action alone"
        )
    if not tracked_config_matches_head(repo_root):
        return "has provenance-ambiguous uncommitted or untracked spice.toml bytes"
    return None


def _relevant_signatures(
    repo_root: Path,
    grant: StandingTrustGrant,
    capabilities: tuple[str, ...],
) -> tuple[tuple[tuple[str, CommitSignature], ...], str | None]:
    relevant: list[str] = []
    for commit, parents in commit_parent_rows(
        repo_root, grant.anchor_commit, current_commit(repo_root)
    ):
        if _commit_changes_capability(repo_root, commit, parents, capabilities):
            relevant.append(commit)
    if not relevant:
        return (
            (),
            "has changed capability digest without attributable commit provenance",
        )
    signatures: list[tuple[str, CommitSignature]] = []
    for commit in relevant:
        signature = commit_signature(repo_root, commit)
        if not signature.verified:
            kind = "unsigned" if not signature.fingerprint else "unverifiable"
            return (), f"has {kind} executable-config commit {commit}"
        if signature.fingerprint not in grant.trusted_signers:
            return (
                (),
                "has executable-config commit "
                f"{commit} from untrusted signer {signature.fingerprint}",
            )
        signatures.append((commit, signature))
    return tuple(signatures), None


def _commit_changes_capability(
    repo_root: Path,
    commit: str,
    parents: tuple[str, ...],
    capabilities: tuple[str, ...],
) -> bool:
    after = _capability_digests_at_commit(repo_root, commit)
    for parent in parents or ("",):
        before = _capability_digests_at_commit(repo_root, parent) if parent else {}
        if any(before.get(name) != after.get(name) for name in capabilities):
            return True
    return False


def _capability_digests_at_commit(
    repo_root: Path,
    commit: str,
) -> dict[str, str]:
    content = config_at_commit(repo_root, commit)
    if content is None:
        return {}
    values = parse_config_text(
        content,
        source_name="repository-provenance",
        source_path=f"{commit}:spice.toml",
    )
    return _capability_digests(_surface_from_mapping(values))


def _trusted_upstream(repo_root: Path) -> tuple[str, str]:
    from spice.tasks.git.boundaries import branch_upstream_target

    target = branch_upstream_target(repo_root)
    if target is None:
        raise SpiceError(
            "standing repository configuration trust requires an origin-backed "
            "tracked branch"
        )
    return target


def _remote_url(repo_root: Path, remote: str) -> str:
    url = git_read(repo_root, "remote", "get-url", remote)
    if not url:
        raise SpiceError(
            f"standing repository configuration trust cannot resolve remote {remote}"
        )
    if not _unambiguous_remote_url(url):
        raise SpiceError(
            "standing repository configuration trust refuses a relative remote "
            f"URL that can resolve differently across linked worktrees: {url}"
        )
    return url


def _unambiguous_remote_url(url: str) -> bool:
    if Path(url).is_absolute() or "://" in url:
        return True
    colon = url.find(":")
    slash = url.find("/")
    return colon > 0 and (slash < 0 or colon < slash)


def _standing_grant(
    *,
    repository_url: str,
    remote: str,
    ref: str,
    anchor_commit: str,
    capabilities: tuple[str, ...],
    trusted_signers: tuple[str, ...],
) -> StandingTrustGrant:
    fields = {
        "repository_url": repository_url,
        "remote": remote,
        "ref": ref,
        "anchor_commit": anchor_commit,
        "capabilities": capabilities,
        "trusted_signers": trusted_signers,
    }
    encoded = json.dumps(
        fields,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return StandingTrustGrant(
        grant_id=hashlib.sha256(encoded).hexdigest(),
        **fields,
    )


def _grant_plan_payload(
    repo_root: Path,
    grant: StandingTrustGrant,
    approvals: Mapping[str, str],
    *,
    record_count: int,
) -> dict[str, Any]:
    return command_plan_payload(
        command="config trust grant",
        metadata={
            "repository": str(repo_root),
            "authority_path": str(repository_trust_log_path(repo_root)),
        },
        operations=[
            {
                "kind": "repository-config-trust-grant",
                "target": grant.grant_id,
                "scope": "common-git-state",
                "repository_url": grant.repository_url,
                "remote": grant.remote,
                "ref": grant.ref,
                "anchor_commit": grant.anchor_commit,
                "capabilities": list(grant.capabilities),
                "trusted_signers": list(grant.trusted_signers),
                "approvals": dict(approvals),
                "observed_record_count": record_count,
            }
        ],
    )


def _revoke_plan_payload(
    repo_root: Path,
    grant: StandingTrustGrant | None,
    reason: str,
    *,
    record_count: int,
) -> dict[str, Any]:
    return command_plan_payload(
        command="config trust revoke",
        metadata={
            "repository": str(repo_root),
            "authority_path": str(repository_trust_log_path(repo_root)),
        },
        operations=[
            {
                "kind": "repository-config-trust-revoke",
                "target": grant.grant_id if grant is not None else "all",
                "scope": "common-git-state",
                "authority": "all exact and delegated approvals",
                "reason": reason,
                "observed_record_count": record_count,
            }
        ],
    )


def _selected_capabilities(
    requested: Sequence[str],
    current: Mapping[str, str],
) -> tuple[str, ...]:
    selected = (
        _normalized_nonempty(requested, "capability")
        if requested
        else tuple(sorted(current))
    )
    unknown = sorted(set(selected).difference(EXECUTABLE_REPOSITORY_CAPABILITIES))
    if unknown:
        raise SpiceError(
            "unknown executable repository capability: " + ", ".join(unknown)
        )
    absent = sorted(set(selected).difference(current))
    if absent:
        raise SpiceError(
            "repository does not currently define capability: " + ", ".join(absent)
        )
    if not selected:
        raise SpiceError(
            "repository has no executable configuration capabilities to trust"
        )
    return selected


def _normalized_nonempty(values: Sequence[str], label: str) -> tuple[str, ...]:
    normalized = tuple(
        dict.fromkeys(value.strip() for value in values if value.strip())
    )
    if not normalized:
        raise SpiceError(f"standing repository configuration trust needs a {label}")
    return normalized


def _legacy_approved_digest(repo_root: Path) -> str | None:
    from spice.hooks.initplan import InitReceiptStatus, load_initialization_receipt

    receipt = load_initialization_receipt(repo_root)
    return (
        receipt.approved_repository_config_digest
        if receipt is not None
        and receipt.repo_root == repo_root
        and receipt.status is InitReceiptStatus.COMPLETE
        else None
    )


def _shared_authority_state(repo_root: Path) -> RepositoryTrustState:
    """Migrate the old worktree digest once, then use only shared authority."""
    marker = shared_state_path(repo_root, SHARED_AUTHORITY_MIGRATION_PATH)
    if marker.exists():
        return load_repository_trust_state(repo_root)
    with bounded_exclusive_lock(
        marker.with_suffix(".lock"),
        timeout_seconds=SHARED_AUTHORITY_MIGRATION_LOCK_SECONDS,
        action="migrate repository configuration authority",
    ):
        if not marker.exists():
            _migrate_legacy_authority(repo_root, marker)
    return load_repository_trust_state(repo_root)


def _migrate_legacy_authority(repo_root: Path, marker: Path) -> None:
    legacy = _legacy_approved_digest(repo_root)
    snapshot = plan_exact_repository_config_approval(repo_root)
    approvals: dict[str, str] = {}
    if legacy is not None and hmac.compare_digest(legacy, snapshot.digest):
        approvals = dict(snapshot.capability_digests)
        if approvals:
            record_exact_approvals(
                repo_root,
                approvals,
                commit=git_read(repo_root, "rev-parse", "HEAD^{commit}") or None,
                source=(
                    f"{SHARED_AUTHORITY_MIGRATION_RELEASE} legacy initialization "
                    "receipt migration"
                ),
            )
    atomic_write_json(
        marker,
        {
            "schema_version": 1,
            "release": SHARED_AUTHORITY_MIGRATION_RELEASE,
            "legacy_digest": legacy,
            "migrated_capabilities": sorted(approvals),
            "authority_path": str(repository_trust_log_path(repo_root)),
        },
        write_if_changed=True,
    )


def _exact_approval_refusal(
    state: RepositoryTrustState,
    current: Mapping[str, str],
) -> str:
    changed: list[str] = []
    for capability, digest in current.items():
        approved = sorted(state.exact_approvals.get(capability, ()))
        if approved and digest not in approved:
            changed.append(
                f"{capability} approved={','.join(approved)} current={digest}"
            )
    if changed:
        return "changed since operator approval (" + "; ".join(changed) + ")"
    return "has no operator approval or active standing grant"


def _declared_capability_path(path: tuple[str, ...]) -> tuple[str, ...]:
    matches = [
        declared
        for declared in EXECUTABLE_REPOSITORY_CONFIG_PATHS
        if path[: len(declared)] == declared
    ]
    if not matches:
        raise SpiceError(
            "repository executable configuration guard path "
            f"{'.'.join(path)} is absent from "
            "EXECUTABLE_REPOSITORY_CONFIG_PATHS"
        )
    return max(matches, key=len)


def _repository_executable_surface(loaded: LayeredConfig) -> dict[str, Any]:
    return _surface_from_mapping(loaded.layer(REPOSITORY_SOURCE).values)


def _surface_from_mapping(repository: Mapping[str, Any]) -> dict[str, Any]:
    surface: dict[str, Any] = {}
    for path in EXECUTABLE_REPOSITORY_CONFIG_PATHS:
        value = _mapping_value(repository, path)
        if value is not _MISSING:
            surface[".".join(path)] = _plain_value(value)
    return surface


def _capability_digests(surface: Mapping[str, Any]) -> dict[str, str]:
    return {
        capability: _surface_digest({capability: value})
        for capability, value in sorted(surface.items())
    }


def _surface_digest(surface: Mapping[str, Any]) -> str:
    encoded = json.dumps(
        surface,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


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
