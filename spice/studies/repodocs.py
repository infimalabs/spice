"""Repo-truth document pressure: character budgets, scopes, and sticky breaches."""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path

from spice.flexstate import (
    FlexSliceClaim,
    claim_flex_slice_paths,
    git_state_path,
    load_sticky_items,
    render_flex_slice_claim_redirect,
    save_sticky_items,
    sticky_paths_after_renames,
)
from spice.policy import REPO_TRUTH_DOCS
from spice.policyconfig import ResolvedPolicy, resolve_policy
from spice.repocfg import policy_table, string_list
from spice.studies.walk import staged_renames, tracked_paths

REPO_DOC_CHAR_STICKY_VERSION = 1
REPO_DOC_CHAR_STICKY_STATE_GIT_PATH = "spice/repo-doc-chars-sticky.json"


@dataclass(frozen=True)
class RepoTruthDocFinding:
    path: Path
    char_count: int
    limit: int
    flex_slice_claim: FlexSliceClaim | None = None


def repo_truth_docs(repo_root: Path) -> list[str]:
    declared = string_list(policy_table(repo_root).get("repo_truth_docs"))
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
    renames = _staged_renames_or_empty(repo_root)
    loaded_sticky = sticky_paths_after_renames(
        _load_repo_doc_char_sticky(repo_root),
        renames,
    )
    flex_breaches = {
        path
        for path in paths
        if _repo_doc_path_breaches_flex(path, repo_root=repo_root, resolved=resolved)
    }
    updated_sticky = loaded_sticky | flex_breaches
    if persist and updated_sticky != loaded_sticky:
        _save_repo_doc_char_sticky(updated_sticky, repo_root)
    claim_decisions = claim_flex_slice_paths(
        flex_breaches,
        root=repo_root,
        actor=flex_actor,
        renames=renames,
        now=flex_claim_now,
    )
    peer_claims = {
        path: decision.claim
        for path, decision in claim_decisions.items()
        if decision.peer_held
    }
    findings: list[RepoTruthDocFinding] = []
    for rel_path in paths:
        count = _doc_char_count(repo_root / rel_path)
        if count is None:
            continue
        scoped = resolved.jittered_bound_for_path(
            "repo_truth_doc_chars",
            resolved.limits.repo_truth_doc_chars,
            rel_path,
        )
        if scoped.unlimited:
            continue
        limit = scoped.limit if rel_path in updated_sticky else scoped.flex_limit
        if count > limit:
            findings.append(
                RepoTruthDocFinding(
                    path=rel_path,
                    char_count=count,
                    limit=limit,
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


def clear_repo_truth_doc_sticky_state(
    repo_root: Path, *, resolved: ResolvedPolicy | None = None
) -> None:
    state_path = repo_doc_char_sticky_state_path(repo_root)
    if state_path is None or not state_path.exists():
        return
    active_policy = resolved or resolve_policy(repo_root)
    sticky = _load_repo_doc_char_sticky(repo_root)
    retained = {
        rel_path
        for rel_path in sticky
        if _repo_doc_path_exceeds_base(
            rel_path, repo_root=repo_root, resolved=active_policy
        )
    }
    if retained:
        _save_repo_doc_char_sticky(retained, repo_root)
    else:
        state_path.unlink()


def repo_doc_char_sticky_state_path(repo_root: Path) -> Path | None:
    try:
        return git_state_path(REPO_DOC_CHAR_STICKY_STATE_GIT_PATH, root=repo_root)
    except subprocess.CalledProcessError:
        return None


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


def _staged_renames_or_empty(repo_root: Path) -> dict[Path, Path]:
    try:
        return staged_renames(repo_root)
    except subprocess.CalledProcessError:
        return {}


def _doc_char_count(path: Path) -> int | None:
    if not path.is_file():
        return None
    raw = path.read_bytes()
    if b"\0" in raw:
        return None
    return len(raw.decode("utf-8", errors="replace"))


def _load_repo_doc_char_sticky(repo_root: Path) -> set[Path]:
    try:
        return load_sticky_items(
            root=repo_root,
            state_path=None,
            git_path=REPO_DOC_CHAR_STICKY_STATE_GIT_PATH,
            entries_key="paths",
            decode=lambda raw: Path(raw) if isinstance(raw, str) else None,
            version=REPO_DOC_CHAR_STICKY_VERSION,
        )
    except subprocess.CalledProcessError:
        return set()


def _save_repo_doc_char_sticky(paths: set[Path], repo_root: Path) -> None:
    try:
        save_sticky_items(
            paths,
            root=repo_root,
            state_path=None,
            git_path=REPO_DOC_CHAR_STICKY_STATE_GIT_PATH,
            entries_key="paths",
            encode=lambda path: path.as_posix(),
            version=REPO_DOC_CHAR_STICKY_VERSION,
        )
    except subprocess.CalledProcessError:
        return


def _repo_doc_path_breaches_flex(
    path: Path, *, repo_root: Path, resolved: ResolvedPolicy
) -> bool:
    count = _doc_char_count(repo_root / path)
    if count is None:
        return False
    scoped = resolved.jittered_bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        path,
    )
    return not scoped.unlimited and count > scoped.flex_limit


def _repo_doc_path_exceeds_base(
    path: Path, *, repo_root: Path, resolved: ResolvedPolicy
) -> bool:
    count = _doc_char_count(repo_root / path)
    if count is None:
        return False
    scoped = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        path,
    )
    return not scoped.unlimited and count > scoped.limit
