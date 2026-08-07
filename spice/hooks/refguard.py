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
    upstream_ref = _declared_upstream_ref(repo_root, current_ref)
    if not upstream_ref:
        return 0
    # Git reports the old value the caller *declared*, and `git update-ref
    # <ref> <new>` declares none, so that field arrives as the zero OID -- a
    # rewind then reads as a ref creation. In the `prepared` state the ref
    # itself still holds its pre-transaction value, so ask the repository. No
    # tip to read means this branch is being created and abandons nothing.
    current_tip = _commit_oid(repo_root, current_ref)
    if not current_tip:
        return 0
    upstream = _commit_oid(repo_root, upstream_ref)

    text = sys.stdin.read() if stdin_text is None else stdin_text
    for update in _parse_updates(text):
        if update.ref != current_ref:
            continue
        if not upstream:
            _require_a_judgeable_upstream(
                repo_root, current_ref, upstream_ref, tip=current_tip, new=update.new
            )
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


def _declared_upstream_ref(repo_root: Path, current_ref: str) -> str:
    """The remote-tracking ref this branch's own pairing names, if it names one.

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
    assumes `origin` or `main`. Only the naming is answered here -- whether the
    named ref exists is a separate question, and conflating the two would let a
    pairing nobody can resolve read exactly like a branch that tracks nothing.
    """
    branch = current_ref.removeprefix(HEADS_PREFIX)
    remote = git_read(
        repo_root, "config", "--local", "--get", f"branch.{branch}.remote"
    )
    merge = git_read(repo_root, "config", "--local", "--get", f"branch.{branch}.merge")
    if not remote or not merge or remote == SELF_REMOTE:
        return ""
    return f"refs/remotes/{remote}/{merge.removeprefix(HEADS_PREFIX)}"


def _require_a_judgeable_upstream(
    repo_root: Path, current_ref: str, upstream_ref: str, *, tip: str, new: str
) -> None:
    """Refuse a rewind whose declared upstream resolves to nothing.

    An append gives up no history whatever the upstream turns out to be, so it
    passes; refusing it would strand a lane whose remote is merely unfetched.
    A rewind is the call that needs the upstream, and passing it in silence
    would report the same success as a guard that checked and found it clear.
    """
    if _abandons_nothing(repo_root, tip=tip, new=new):
        return
    raise SpiceError(
        "reference-transaction guard cannot establish the upstream of current "
        f"branch {current_ref}: its pairing names {upstream_ref}, which does "
        "not resolve to a commit, so whether this rewind gives up published "
        "work is unknown rather than cleared. Fetch that remote, or correct "
        "the branch's remote and merge pairing, and run this again."
    )


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
    if _abandons_nothing(repo_root, tip=tip, new=new):
        return []
    merge_bases = git_lines(repo_root, "merge-base", "--all", tip, upstream)
    if not merge_bases:
        return []
    args = ["rev-list", f"--max-count={PROTECTED_COMMIT_LIMIT}", *merge_bases]
    if not _is_zero_oid(new):
        args.extend(["--not", _commit_oid(repo_root, new)])
    return git_lines(repo_root, *args)


def _abandons_nothing(repo_root: Path, *, tip: str, new: str) -> bool:
    """Whether the new value leaves the branch's current tip still reachable.

    Deleting the branch reaches nothing, so it stays subject to judgement. A
    value naming no commit at all is not this guard's to rule on. Anything
    that still reaches `tip` is an append and gives up no history.
    """
    if _is_zero_oid(new):
        return False
    new_commit = _commit_oid(repo_root, new)
    if not new_commit:
        return True
    return _is_ancestor(repo_root, tip, new_commit)


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
