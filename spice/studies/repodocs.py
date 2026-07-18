"""Repo-truth document pressure: character budgets, scopes, and sticky breaches."""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.flexstate import (
    FlexSliceClaim,
    render_flex_slice_claim_redirect,
)
from spice.policy import REPO_TRUTH_DOCS
from spice.policyconfig import ResolvedPolicy, resolve_policy
from spice.configlayer import config_string_list, effective_table
from spice.studies import gates
from spice.studies.walk import tracked_paths

REPO_DOC_CHAR_STICKY_VERSION = 1
REPO_DOC_CHAR_STICKY_STATE_GIT_PATH = "repo-doc-chars-sticky.json"
_CHAR_STICKY_LEDGER = gates.path_sticky_ledger(
    REPO_DOC_CHAR_STICKY_STATE_GIT_PATH,
    version=REPO_DOC_CHAR_STICKY_VERSION,
)


@dataclass(frozen=True)
class RepoTruthDocFinding:
    path: Path
    char_count: int
    limit: int
    flex_slice_claim: FlexSliceClaim | None = None


def repo_truth_docs(repo_root: Path) -> list[str]:
    declared = config_string_list(
        effective_table(repo_root, "policy").get("repo_truth_docs")
    )
    return declared or list(REPO_TRUTH_DOCS)


def repo_truth_doc_findings(
    repo_root: Path,
    *,
    persist: bool = False,
    flex_actor: str = "",
    flex_claim_now: float | None = None,
) -> list[RepoTruthDocFinding]:
    resolved = resolve_policy(repo_root)
    paths = repo_truth_doc_candidate_paths(repo_root, resolved)
    renames = gates.staged_gate_renames(
        repo_root,
        errors=(subprocess.CalledProcessError,),
    )
    flex_breaches = {
        path
        for path in paths
        if (
            disposition := _repo_doc_disposition(
                path,
                repo_root=repo_root,
                resolved=resolved,
            )
        )
        is not None
        and disposition.flex_breach
    }
    sticky_state = gates.reconcile_sticky_latch(
        _CHAR_STICKY_LEDGER,
        root=repo_root,
        renames=renames,
        retain=lambda sticky_paths: {
            path
            for path in sticky_paths
            if (
                disposition := _repo_doc_disposition(
                    path,
                    repo_root=repo_root,
                    resolved=resolved,
                )
            )
            is not None
            and disposition.over_base
        },
        breach_keys=flex_breaches,
        persist=persist,
        load_errors=(subprocess.CalledProcessError, SpiceError),
        persist_errors=(subprocess.CalledProcessError, SpiceError),
    )
    peer_claims = gates.peer_flex_slice_claims(
        flex_breaches,
        root=repo_root,
        actor=flex_actor,
        renames=renames,
        now=flex_claim_now,
    )
    findings: list[RepoTruthDocFinding] = []
    for rel_path in paths:
        count = _doc_char_count(repo_root / rel_path)
        if count is None:
            continue
        disposition = gates.bounded_disposition(
            count,
            _repo_doc_bounds(rel_path, resolved=resolved),
            latched=rel_path in sticky_state.updated,
        )
        if disposition.over_limit:
            findings.append(
                RepoTruthDocFinding(
                    path=rel_path,
                    char_count=count,
                    limit=disposition.limit,
                    flex_slice_claim=peer_claims.get(rel_path),
                )
            )
    return findings


def render_repo_truth_doc_lines(findings: list[RepoTruthDocFinding]) -> list[str]:
    lines: list[str] = []
    for finding in findings:
        details = f"{finding.char_count} characters (cap {finding.limit}"
        if finding.flex_slice_claim is not None:
            details += "; " + render_flex_slice_claim_redirect(finding.flex_slice_claim)
        lines.append(f"  {finding.path.as_posix()}: {details})")
    return lines


def render_repo_truth_doc_guard_error(findings: list[RepoTruthDocFinding]) -> str:
    if not findings:
        return "repo-truth docs: ok"
    if all(finding.flex_slice_claim is not None for finding in findings):
        header = (
            "repo-truth docs hit peer-held flex slices; keep this change "
            "append-only or move to another seam:"
        )
    else:
        header = "repo-truth docs exceed the character cap; tighten the doctrine:"
    return header + "\n" + "\n".join(render_repo_truth_doc_lines(findings))


def repo_truth_doc_candidate_paths(
    repo_root: Path, resolved: ResolvedPolicy
) -> list[Path]:
    """Return repo-relative docs governed by repo-doc character budgets."""
    paths = {Path(name) for name in repo_truth_docs(repo_root)}
    paths.update(
        path
        for path in _tracked_paths_or_empty(repo_root)
        if resolved.markdown_depth_budget_applies_to_path(path)
    )
    return sorted(paths, key=lambda path: path.as_posix())


def _tracked_paths_or_empty(repo_root: Path) -> list[Path]:
    try:
        return tracked_paths(repo_root)
    except subprocess.CalledProcessError:
        return []


def _doc_char_count(path: Path) -> int | None:
    if not path.is_file():
        return None
    raw = path.read_bytes()
    if b"\0" in raw:
        return None
    return len(raw.decode("utf-8", errors="replace"))


def _repo_doc_disposition(
    path: Path, *, repo_root: Path, resolved: ResolvedPolicy
) -> gates.BoundedDisposition | None:
    count = _doc_char_count(repo_root / path)
    if count is None:
        return None
    return gates.bounded_disposition(count, _repo_doc_bounds(path, resolved=resolved))


def _repo_doc_bounds(path: Path, *, resolved: ResolvedPolicy) -> gates.BoundedValue:
    scoped = resolved.jittered_bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        path,
    )
    return gates.BoundedValue(
        base_limit=scoped.limit,
        flex_limit=scoped.flex_limit,
        unlimited=scoped.unlimited,
    )
