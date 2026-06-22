"""Worktree discovery for the serve UI: one lane target per git worktree."""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from pathlib import Path

from spice.worktrees import WorktreeRecord, list_worktrees

_WORKTREE_ID_RE = re.compile(r"[^a-z0-9]+")
_WORKTREE_ID_DIGEST_CHARS = 8


@dataclass(frozen=True)
class WorktreeTarget:
    id: str
    repo_root: Path
    name: str
    branch: str

    @property
    def display_name(self) -> str:
        return self.name


def discover_serve_worktrees(
    *,
    cwd: Path,
    fallback_roots: list[Path | None] | None = None,
) -> list[WorktreeTarget]:
    records = _registered_worktree_records(cwd)
    targets = [
        _target_from_record(record)
        for record in records
        if not record.bare and record.path.exists()
    ]
    known_roots = {target.repo_root.resolve() for target in targets}
    for root in fallback_roots or []:
        if root is None or not root.exists():
            continue
        resolved = root.resolve()
        if resolved in known_roots:
            continue
        targets.append(
            WorktreeTarget(
                id=worktree_id_for_path(resolved),
                repo_root=resolved,
                name=resolved.name,
                branch=read_worktree_branch_name(resolved),
            )
        )
        known_roots.add(resolved)
    return sorted(targets, key=lambda target: (target.name.lower(), target.branch))


def match_serve_worktree(
    targets: list[WorktreeTarget],
    selector: str | Path | None,
) -> WorktreeTarget | None:
    if selector is None:
        return None
    raw = str(selector)
    if not raw:
        return None
    for target in targets:
        if raw in {target.id, target.branch, target.name, str(target.repo_root)}:
            return target
    candidate = Path(raw).expanduser()
    try:
        resolved = candidate.resolve()
    except OSError:
        return None
    for target in targets:
        if target.repo_root.resolve() == resolved:
            return target
    return None


def worktree_id_for_path(path: Path) -> str:
    resolved = path.resolve()
    slug = _WORKTREE_ID_RE.sub("-", resolved.name.lower()).strip("-") or "worktree"
    digest = hashlib.sha1(str(resolved).encode("utf-8")).hexdigest()[
        :_WORKTREE_ID_DIGEST_CHARS
    ]
    return f"{slug}-{digest}"


def read_worktree_branch_name(repo_root: Path) -> str:
    try:
        git_path = repo_root / ".git"
        git_dir = git_path
        if git_path.is_file():
            git_dir = Path(
                git_path.read_text(encoding="utf-8", errors="replace")
                .strip()
                .removeprefix("gitdir:")
                .strip()
            )
            if not git_dir.is_absolute():
                git_dir = (repo_root / git_dir).resolve()
        head = (git_dir / "HEAD").read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return ""
    prefix = "ref: refs/heads/"
    return head.removeprefix(prefix) if head.startswith(prefix) else ""


def _registered_worktree_records(cwd: Path) -> list[WorktreeRecord]:
    try:
        return list_worktrees(cwd=cwd)
    except RuntimeError:
        return []


def _target_from_record(record: WorktreeRecord) -> WorktreeTarget:
    root = record.path.resolve()
    return WorktreeTarget(
        id=worktree_id_for_path(root),
        repo_root=root,
        name=root.name,
        branch=record.branch_name or read_worktree_branch_name(root),
    )
