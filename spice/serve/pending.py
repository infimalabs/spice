"""Authoritative pending-inbox identity for serve payloads."""

from __future__ import annotations

from hashlib import blake2s
from pathlib import Path
from typing import Any

from spice.mail.inbox import InboxItem, collect_inbox_items, inbox_dir, inbox_item_key

_NANOSECONDS_PER_MICROSECOND = 1000


def pending_inbox_identity_payload(repo_root: str | Path | None) -> dict[str, Any]:
    items = collect_inbox_items(repo_root)
    keys = [inbox_item_key(item.name) for item in items]
    return {
        "pendingInboxCount": len(keys),
        "pendingInboxLabel": str(len(keys)),
        "pendingInboxKeys": keys,
        "pendingInboxRevision": pending_inbox_revision(items),
        "pendingInboxVersion": pending_inbox_version(repo_root, items),
    }


def pending_inbox_revision(items: list[InboxItem]) -> str:
    digest = blake2s(digest_size=16)
    for item in items:
        digest.update(inbox_item_key(item.name).encode("utf-8"))
        digest.update(b"\0")
        try:
            stat = item.source_path.stat()
        except OSError:
            digest.update(b"missing")
        else:
            digest.update(str(stat.st_mtime_ns).encode("ascii"))
            digest.update(b":")
            digest.update(str(stat.st_size).encode("ascii"))
        digest.update(b"\0")
    return digest.hexdigest()


def pending_inbox_version(repo_root: str | Path | None, items: list[InboxItem]) -> int:
    """Comparable inbox snapshot version safe for JavaScript Number ordering."""
    if not repo_root:
        return 0
    version_ns = _path_mtime_ns(inbox_dir(repo_root))
    for item in items:
        version_ns = max(version_ns, _path_mtime_ns(item.source_path))
    return version_ns // _NANOSECONDS_PER_MICROSECOND


def _path_mtime_ns(path: Path) -> int:
    try:
        return path.stat().st_mtime_ns
    except OSError:
        return 0
