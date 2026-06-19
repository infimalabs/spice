"""Payload helpers for inbox attachment metadata."""

from __future__ import annotations

from pathlib import Path
from urllib.parse import quote

from spice.mail.attachments import InboxAttachment, shared_attachment_display_path


def inbox_attachment_payloads(
    attachments: tuple[InboxAttachment, ...],
    *,
    repo_root: Path | None,
    worktree_id: str | None,
) -> list[dict[str, object]]:
    return [
        _attachment_payload(attachment, repo_root=repo_root, worktree_id=worktree_id)
        for attachment in attachments
    ]


def _attachment_payload(
    attachment: InboxAttachment,
    *,
    repo_root: Path | None,
    worktree_id: str | None,
) -> dict[str, object]:
    rel_path = _relative_attachment_path(attachment.path, repo_root)
    payload: dict[str, object] = {
        "name": attachment.name,
        "contentType": attachment.content_type,
        "size": attachment.size,
        "path": rel_path,
    }
    if worktree_id and rel_path:
        payload["url"] = (
            f"/api/work/trees/{quote(worktree_id, safe='')}/files/image"
            f"?path={quote(rel_path, safe='')}"
        )
    return payload


def _relative_attachment_path(path: Path, repo_root: Path | None) -> str:
    if repo_root is None:
        return str(path)
    shared_path = shared_attachment_display_path(path, repo_root=repo_root)
    if shared_path is not None:
        return shared_path.as_posix()
    try:
        return path.relative_to(repo_root).as_posix()
    except ValueError:
        return str(path)
