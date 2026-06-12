"""Git worktree discovery and `--worktree` target resolution."""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class WorktreeRecord:
    path: Path
    branch: str | None = None
    bare: bool = False

    @property
    def basename(self) -> str:
        return self.path.name

    @property
    def branch_name(self) -> str | None:
        if self.branch is None:
            return None
        return self.branch.removeprefix("refs/heads/")


def resolve_worktree_target(target: str, *, cwd: Path | None = None) -> Path:
    """Resolve `target` (path, branch, or basename) to a registered worktree."""
    records = non_bare_worktree_records(cwd=cwd)
    raw_path = Path(target).expanduser()
    if raw_path.exists():
        resolved = _resolve_existing_worktree_path(raw_path)
        if any(record.path.resolve() == resolved for record in records):
            return resolved
        raise RuntimeError(f"path is not a registered git worktree: {target!r}")
    matches = [
        record for record in records if target in {record.branch, record.branch_name}
    ] or [record for record in records if target == record.basename]
    if not matches:
        raise RuntimeError(f"no git worktree matched {target!r}")
    resolved_set = {record.path.resolve() for record in matches}
    if len(resolved_set) != 1:
        choices = ", ".join(sorted(path.as_posix() for path in resolved_set))
        raise RuntimeError(f"ambiguous git worktree target {target!r}: {choices}")
    return resolved_set.pop()


def non_bare_worktree_records(*, cwd: Path | None = None) -> list[WorktreeRecord]:
    return [record for record in list_worktrees(cwd=cwd) if not record.bare]


def list_worktrees(*, cwd: Path | None = None) -> list[WorktreeRecord]:
    try:
        completed = subprocess.run(
            ["git", "worktree", "list", "--porcelain"],
            cwd=cwd,
            capture_output=True,
            check=True,
            text=True,
        )
    except subprocess.CalledProcessError as exc:
        raise RuntimeError(
            f"could not list git worktrees from {cwd or Path.cwd()}"
        ) from exc
    records: list[WorktreeRecord] = []
    current_path: Path | None = None
    current_branch: str | None = None
    current_bare = False
    for line in completed.stdout.splitlines():
        if line.startswith("worktree "):
            if current_path is not None:
                records.append(
                    WorktreeRecord(
                        current_path,
                        current_branch,
                        _is_bare_record(current_path, current_bare),
                    )
                )
            current_path = Path(line.removeprefix("worktree ")).expanduser()
            current_branch = None
            current_bare = False
        elif line.startswith("branch "):
            current_branch = line.removeprefix("branch ")
        elif line == "bare":
            current_bare = True
    if current_path is not None:
        records.append(
            WorktreeRecord(
                current_path,
                current_branch,
                _is_bare_record(current_path, current_bare),
            )
        )
    return records


def _is_bare_record(path: Path, bare_marker: bool) -> bool:
    if bare_marker:
        return True
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(path),
            "rev-parse",
            "--is-bare-repository",
            "--is-inside-work-tree",
        ],
        capture_output=True,
        check=False,
        text=True,
    )
    if completed.returncode != 0:
        return True
    lines = [line.strip() for line in completed.stdout.splitlines()]
    is_bare = lines[0] if len(lines) >= 1 else ""
    is_inside_work_tree = lines[1] if len(lines) >= 2 else ""
    return is_bare == "true" or is_inside_work_tree != "true"


def _resolve_existing_worktree_path(path: Path) -> Path:
    try:
        completed = subprocess.run(
            ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
            capture_output=True,
            check=True,
            text=True,
        )
    except subprocess.CalledProcessError as exc:
        raise RuntimeError(f"path is not a git worktree: {path}") from exc
    return Path(completed.stdout.strip()).resolve()
