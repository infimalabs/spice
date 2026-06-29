"""Repo-truth document pressure: character budgets, scopes, and sticky breaches."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.flexstate import (
    git_state_path,
    load_sticky_items,
    save_sticky_items,
    sticky_items_after_flex_breaches,
    sticky_paths_after_renames,
)
from spice.policy import REPO_TRUTH_DOCS
from spice.policyconfig import ResolvedPolicy, resolve_policy
from spice.repocfg import policy_table, string_list
from spice.studies.walk import staged_renames, tracked_paths

REPO_DOC_CHAR_STICKY_VERSION = 1
REPO_DOC_CHAR_STICKY_STATE_GIT_PATH = "spice/repo-doc-chars-sticky.json"


def repo_truth_docs(repo_root: Path) -> list[str]:
    declared = string_list(policy_table(repo_root).get("repo_truth_docs"))
    return declared or list(REPO_TRUTH_DOCS)


def repo_truth_doc_violations(repo_root: Path, *, persist: bool = False) -> list[str]:
    """Return one ``name: count characters (cap N)`` line per over-cap doc."""
    over: list[str] = []
    resolved = resolve_policy(repo_root)
    paths = _repo_truth_doc_candidate_paths(repo_root, resolved)
    loaded_sticky = sticky_paths_after_renames(
        _load_repo_doc_char_sticky(repo_root),
        _staged_renames_or_empty(repo_root),
    )
    updated_sticky = _repo_doc_char_sticky_after_flex_breaches(
        paths, loaded_sticky, repo_root=repo_root, resolved=resolved
    )
    if persist and updated_sticky != loaded_sticky:
        _save_repo_doc_char_sticky(updated_sticky, repo_root)
    for rel_path in paths:
        count = _doc_char_count(repo_root / rel_path)
        if count is None:
            continue
        scoped = resolved.bound_for_path(
            "repo_truth_doc_chars",
            resolved.limits.repo_truth_doc_chars,
            rel_path,
        )
        if scoped.unlimited:
            continue
        limit = scoped.limit if rel_path in updated_sticky else scoped.flex_limit
        if count > limit:
            over.append(f"  {rel_path.as_posix()}: {count} characters (cap {limit})")
    return over


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


def _repo_truth_doc_candidate_paths(
    repo_root: Path, resolved: ResolvedPolicy
) -> list[Path]:
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


def _repo_doc_char_sticky_after_flex_breaches(
    paths: list[Path],
    sticky_paths: set[Path],
    *,
    repo_root: Path,
    resolved: ResolvedPolicy,
) -> set[Path]:
    return sticky_items_after_flex_breaches(
        paths,
        sticky_paths,
        key_for_item=lambda path: path,
        is_breach=lambda path: _repo_doc_path_breaches_flex(
            path, repo_root=repo_root, resolved=resolved
        ),
    )


def _repo_doc_path_breaches_flex(
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
