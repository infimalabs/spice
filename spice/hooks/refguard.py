"""Reference-transaction guard for the currently checked-out branch."""

from __future__ import annotations

import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError

ZERO_OID_CHARS = {"0"}
PROTECTED_COMMIT_LIMIT = 5


@dataclass(frozen=True)
class RefUpdate:
    old: str
    new: str
    ref: str


def handle_reference_transaction(
    repo_root: Path, state: str, stdin_text: str | None = None
) -> int:
    if state != "prepared":
        return 0
    current_ref = _git_read(repo_root, "symbolic-ref", "--quiet", "HEAD")
    if not current_ref:
        return 0
    upstream = _git_read(
        repo_root, "rev-parse", "--verify", "--quiet", "@{upstream}^{commit}"
    )
    if not upstream:
        return 0

    text = sys.stdin.read() if stdin_text is None else stdin_text
    for update in _parse_updates(text):
        if update.ref != current_ref:
            continue
        protected = _abandoned_upstream_commits(
            repo_root, old=update.old, new=update.new, upstream=upstream
        )
        if protected:
            listed = ", ".join(_short_oid(repo_root, commit) for commit in protected)
            raise SpiceError(
                "reference-transaction guard refused to abandon "
                f"upstream-merged commits on current branch {current_ref}: {listed}"
            )
    return 0


def _parse_updates(text: str) -> list[RefUpdate]:
    updates: list[RefUpdate] = []
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        if not raw_line.strip():
            continue
        parts = raw_line.split()
        if len(parts) != 3:
            raise SpiceError(f"malformed reference-transaction line {line_number}")
        updates.append(RefUpdate(old=parts[0], new=parts[1], ref=parts[2]))
    return updates


def _abandoned_upstream_commits(
    repo_root: Path, *, old: str, new: str, upstream: str
) -> list[str]:
    if _is_zero_oid(old):
        return []
    old_commit = _commit_oid(repo_root, old)
    if not old_commit:
        return []
    new_commit = "" if _is_zero_oid(new) else _commit_oid(repo_root, new)
    if new and not _is_zero_oid(new) and not new_commit:
        return []
    if new_commit and _is_ancestor(repo_root, old_commit, new_commit):
        return []

    merge_bases = _git_lines(repo_root, "merge-base", "--all", old_commit, upstream)
    if not merge_bases:
        return []
    args = ["rev-list", f"--max-count={PROTECTED_COMMIT_LIMIT}", *merge_bases]
    if new_commit:
        args.extend(["--not", new_commit])
    return _git_lines(repo_root, *args)


def _is_zero_oid(value: str) -> bool:
    return bool(value) and set(value) <= ZERO_OID_CHARS


def _commit_oid(repo_root: Path, value: str) -> str:
    if value.startswith("ref:"):
        return ""
    return _git_read(
        repo_root, "rev-parse", "--verify", "--quiet", f"{value}^{{commit}}"
    )


def _short_oid(repo_root: Path, value: str) -> str:
    return _git_read(repo_root, "rev-parse", "--short", value) or value


def _is_ancestor(repo_root: Path, ancestor: str, descendant: str) -> bool:
    return (
        _git(repo_root, "merge-base", "--is-ancestor", ancestor, descendant).returncode
        == 0
    )


def _git_read(repo_root: Path, *args: str) -> str:
    completed = _git(repo_root, *args)
    return completed.stdout.strip() if completed.returncode == 0 else ""


def _git_lines(repo_root: Path, *args: str) -> list[str]:
    text = _git_read(repo_root, *args)
    return [line for line in text.splitlines() if line]


def _git(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
        text=True,
    )
