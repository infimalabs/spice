"""Authoritative pending-inbox identity for serve payloads."""

from __future__ import annotations

from hashlib import blake2s
from pathlib import Path
from typing import Any

from spice.mail.inbox import InboxItem, collect_inbox_items, inbox_item_key


def pending_inbox_identity_payload(repo_root: str | Path | None) -> dict[str, Any]:
    items = collect_inbox_items(repo_root)
    keys = [inbox_item_key(item.name) for item in items]
    return {
        "pendingInboxCount": len(keys),
        "pendingInboxLabel": str(len(keys)),
        "pendingInboxKeys": keys,
        "pendingInboxRevision": pending_inbox_revision(items),
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
