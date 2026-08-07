"""Reference-transaction guard for the currently checked-out branch."""

from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.process.git import git_lines, git_read, git_run

ZERO_OID_CHARS = {"0"}
PROTECTED_COMMIT_LIMIT = 5
HEADS_PREFIX = "refs/heads/"
SELF_REMOTE = "."


@dataclass(frozen=True)
class RefUpdate:
    """One line of the hook's stdin. The reported old value is not carried.

    Git names three fields and `_parse_updates` still requires all three, but
    the old one is whatever the caller declared rather than what the ref holds,
    so keeping it would only invite reading it again.
    """

    new: str
    ref: str


def handle_reference_transaction(
    repo_root: Path, state: str, stdin_text: str | None = None
) -> int:
    if state != "prepared":
        return 0
    current_ref = git_read(repo_root, "symbolic-ref", "--quiet", "HEAD")
    if not current_ref:
        return 0
    upstream = _true_upstream_commit(repo_root, current_ref)
    if not upstream:
        return 0
    # Git reports the old value the caller *declared*, and `git update-ref
    # <ref> <new>` declares none, so that field arrives as the zero OID -- a
    # rewind then reads as a ref creation. In the `prepared` state the ref
    # itself still holds its pre-transaction value, so ask the repository. No
    # tip to read means this branch is being created and abandons nothing.
    current_tip = _commit_oid(repo_root, current_ref)
    if not current_tip:
        return 0

    text = sys.stdin.read() if stdin_text is None else stdin_text
    for update in _parse_updates(text):
        if update.ref != current_ref:
            continue
        protected = _abandoned_upstream_commits(
            repo_root, tip=current_tip, new=update.new, upstream=upstream
        )
        if protected:
            listed = ", ".join(_short_oid(repo_root, commit) for commit in protected)
            raise SpiceError(
                "reference-transaction guard refused to abandon "
                f"upstream-merged commits on current branch {current_ref}: {listed}. "
                "This is expected when a task boundary has advanced "
                "origin/upstream; keep those commits and continue with an "
                "append-only commit instead of amending or resetting backwards."
            )
    return 0


def _true_upstream_commit(repo_root: Path, current_ref: str) -> str:
    """The published tip this guard protects: the branch's declared upstream.

    Read from the repository's own local scope and from nowhere else. A lane
    runs under a per-process git shadow that redefines the current branch's
    upstream across two scopes at once: the generated system config supplies the
    first `branch.<name>.merge` value, which is the one git tracking reads, and
    the single-valued `branch.<name>.remote` takes a command-scope override to
    `.`. Together those aim `@{upstream}` back at this very branch, which reads
    every local commit as already published and refuses ordinary amends, so
    `@{upstream}` is never consulted here -- not even as a fallback, because a
    fallback is precisely where that shadow would come back in.

    Which remote and which branch are whatever the pairing names; nothing here
    assumes `origin` or `main`. A branch declaring no remote pairing has
    published nothing, and the empty result lets the transaction through.
    """
    branch = current_ref.removeprefix(HEADS_PREFIX)
    remote = git_read(
        repo_root, "config", "--local", "--get", f"branch.{branch}.remote"
    )
    merge = git_read(repo_root, "config", "--local", "--get", f"branch.{branch}.merge")
    if not remote or not merge or remote == SELF_REMOTE:
        return ""
    tracked = merge.removeprefix(HEADS_PREFIX)
    return _commit_oid(repo_root, f"refs/remotes/{remote}/{tracked}")


def _parse_updates(text: str) -> list[RefUpdate]:
    updates: list[RefUpdate] = []
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        if not raw_line.strip():
            continue
        parts = raw_line.split()
        if len(parts) != 3:
            raise SpiceError(f"malformed reference-transaction line {line_number}")
        updates.append(RefUpdate(new=parts[1], ref=parts[2]))
    return updates


def _abandoned_upstream_commits(
    repo_root: Path, *, tip: str, new: str, upstream: str
) -> list[str]:
    new_commit = "" if _is_zero_oid(new) else _commit_oid(repo_root, new)
    if new and not _is_zero_oid(new) and not new_commit:
        return []
    if new_commit and _is_ancestor(repo_root, tip, new_commit):
        return []

    merge_bases = git_lines(repo_root, "merge-base", "--all", tip, upstream)
    if not merge_bases:
        return []
    args = ["rev-list", f"--max-count={PROTECTED_COMMIT_LIMIT}", *merge_bases]
    if new_commit:
        args.extend(["--not", new_commit])
    return git_lines(repo_root, *args)


def _is_zero_oid(value: str) -> bool:
    return bool(value) and set(value) <= ZERO_OID_CHARS


def _commit_oid(repo_root: Path, value: str) -> str:
    if value.startswith("ref:"):
        return ""
    return git_read(
        repo_root, "rev-parse", "--verify", "--quiet", f"{value}^{{commit}}"
    )


def _short_oid(repo_root: Path, value: str) -> str:
    return git_read(repo_root, "rev-parse", "--short", value) or value


def _is_ancestor(repo_root: Path, ancestor: str, descendant: str) -> bool:
    return (
        git_run(
            repo_root, "merge-base", "--is-ancestor", ancestor, descendant
        ).returncode
        == 0
    )
