"""Authoritative pending-inbox identity for serve payloads."""

from __future__ import annotations

from hashlib import blake2s
from pathlib import Path
from typing import Any

from spice.mail.inbox import (
    PendingInboxEntry,
    collect_pending_inbox_entries,
    inbox_dir,
    inbox_item_key,
)

_NANOSECONDS_PER_MICROSECOND = 1000


def pending_inbox_identity_payload(repo_root: str | Path | None) -> dict[str, Any]:
    # Identity is derived from names plus stat metadata only: reading each queued
    # body here is what made steering submit slow as the unacknowledged backlog
    # grew.
    entries = collect_pending_inbox_entries(repo_root)
    keys = [inbox_item_key(entry.name) for entry in entries]
    return {
        "pendingInboxCount": len(keys),
        "pendingInboxLabel": str(len(keys)),
        "pendingInboxKeys": keys,
        "pendingInboxRevision": pending_inbox_revision(entries),
        "pendingInboxVersion": pending_inbox_version(repo_root, entries),
    }


def pending_inbox_revision(entries: list[PendingInboxEntry]) -> str:
    digest = blake2s(digest_size=16)
    for entry in entries:
        digest.update(inbox_item_key(entry.name).encode("utf-8"))
        digest.update(b"\0")
        digest.update(str(entry.mtime_ns).encode("ascii"))
        digest.update(b":")
        digest.update(str(entry.size).encode("ascii"))
        digest.update(b"\0")
    return digest.hexdigest()


def pending_inbox_version(
    repo_root: str | Path | None, entries: list[PendingInboxEntry]
) -> int:
    """Comparable inbox snapshot version safe for JavaScript Number ordering.

    Never 0 for a real worktree: the UI treats an identity payload without a
    positive version as a protocol violation, and a worktree that has never
    seen inbox activity (missing inbox dir) still needs a valid identity.
    """
    if not repo_root:
        return 0
    version_ns = _path_mtime_ns(inbox_dir(repo_root))
    for entry in entries:
        version_ns = max(version_ns, entry.mtime_ns)
    return max(1, version_ns // _NANOSECONDS_PER_MICROSECOND)


def _path_mtime_ns(path: Path) -> int:
    try:
        return path.stat().st_mtime_ns
    except OSError:
        return 0
