"""Durable image attachments for operator steering inbox items."""

from __future__ import annotations

import base64
import binascii
import contextlib
import json
import mimetypes
import os
import re
import shutil
import uuid
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.paths import fsync_directory

INBOX_ATTACHMENT_DIR_SUFFIX = ".attachments"
INBOX_ATTACHMENT_MANIFEST = "manifest.json"
INBOX_ATTACHMENT_MAX_ITEMS = 8
INBOX_ATTACHMENT_MAX_BYTES = 8 * 1024 * 1024
INBOX_ATTACHMENT_NAME_MAX_CHARS = 96

_DATA_URL_PREFIX_RE = re.compile(
    r"^data:(?P<content_type>[^;,]+)(?:;[^,]*)?;base64,(?P<data>.*)$",
    re.DOTALL,
)
_SAFE_ATTACHMENT_NAME_RE = re.compile(r"[^A-Za-z0-9._-]+")
_INBOX_LIVE_ATTACHMENT_REF_RE = re.compile(
    r"(?P<head>"
    r"/[^\s`\"'<>\]\)]*?\.spice[/\\]inbox[/\\]|"
    r"(?:\.\.?[/\\])*\.spice[/\\]inbox[/\\]"
    r")"
    r"(?P<tail>"
    r"(?!archive[/\\])"
    r"[^\s`\"'<>\]\)]*?\.attachments"
    r"(?:[/\\][^\s`\"'<>\]\)]*)?"
    r")"
)
_INBOX_ARCHIVED_ATTACHMENT_REF_RE = re.compile(
    r"(?P<ref>"
    r"(?:"
    r"/[^\s`\"'<>\]\)]*?\.spice[/\\]inbox[/\\]archive[/\\]|"
    r"(?:\.\.?[/\\])*\.spice[/\\]inbox[/\\]archive[/\\]"
    r")"
    r"[^\s`\"'<>\]\)]*?\.attachments"
    r"(?:[/\\][^\s`\"'<>\]\)]*)?"
    r")"
)
_TRAILING_REF_PUNCTUATION = ".,;:"


@dataclass(frozen=True)
class InboxAttachmentInput:
    name: str
    content_type: str
    data: bytes


@dataclass(frozen=True)
class InboxAttachment:
    path: Path
    name: str
    content_type: str
    size: int


def prepare_inbox_attachments(raw: Any) -> tuple[InboxAttachmentInput, ...]:
    if raw in (None, ""):
        return ()
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes, bytearray)):
        raise ValueError("Attachments must be a list.")
    if len(raw) > INBOX_ATTACHMENT_MAX_ITEMS:
        raise ValueError(
            f"At most {INBOX_ATTACHMENT_MAX_ITEMS} attachments are allowed."
        )
    attachments: list[InboxAttachmentInput] = []
    for index, item in enumerate(raw, start=1):
        if not isinstance(item, Mapping):
            raise ValueError(f"Attachment {index} must be an object.")
        content_type, encoded = _attachment_payload(item)
        if not content_type.startswith("image/"):
            raise ValueError(f"Attachment {index} must be an image.")
        try:
            data = base64.b64decode(encoded, validate=True)
        except (binascii.Error, ValueError) as exc:
            raise ValueError(f"Attachment {index} is not valid base64.") from exc
        if len(data) > INBOX_ATTACHMENT_MAX_BYTES:
            raise ValueError(
                f"Attachment {index} exceeds {INBOX_ATTACHMENT_MAX_BYTES} bytes."
            )
        attachments.append(
            InboxAttachmentInput(
                name=_attachment_name(item.get("name"), content_type, index),
                content_type=content_type,
                data=data,
            )
        )
    return tuple(attachments)


def inbox_attachment_dir(item_path: Path) -> Path:
    return item_path.with_name(f"{item_path.stem}{INBOX_ATTACHMENT_DIR_SUFFIX}")


def collect_inbox_attachments(item_path: Path) -> tuple[InboxAttachment, ...]:
    directory = inbox_attachment_dir(item_path)
    manifest_path = directory / INBOX_ATTACHMENT_MANIFEST
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return ()
    raw_items = manifest.get("attachments") if isinstance(manifest, dict) else None
    if not isinstance(raw_items, list):
        return ()
    attachments: list[InboxAttachment] = []
    for item in raw_items:
        if not isinstance(item, dict):
            continue
        filename = str(item.get("filename") or "")
        path = directory / filename
        if not filename or path.name != filename or not path.is_file():
            continue
        size = path.stat().st_size
        attachments.append(
            InboxAttachment(
                path=path,
                name=str(item.get("name") or filename),
                content_type=str(item.get("content_type") or "image/*"),
                size=size,
            )
        )
    return tuple(attachments)


def write_inbox_attachments(
    item_path: Path, attachments: Sequence[InboxAttachmentInput]
) -> tuple[InboxAttachment, ...]:
    if not attachments:
        return ()
    final_dir = inbox_attachment_dir(item_path)
    tmp_dir = final_dir.with_name(
        f"{final_dir.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp"
    )
    tmp_dir.mkdir(parents=True, exist_ok=False)
    manifest: list[dict[str, Any]] = []
    try:
        for index, attachment in enumerate(attachments, start=1):
            filename = f"{index:02d}-{attachment.name}"
            path = tmp_dir / filename
            _write_bytes_fsynced(path, attachment.data)
            manifest.append(
                {
                    "name": attachment.name,
                    "filename": filename,
                    "content_type": attachment.content_type,
                    "size": len(attachment.data),
                }
            )
        _write_text_fsynced(
            tmp_dir / INBOX_ATTACHMENT_MANIFEST,
            json.dumps({"attachments": manifest}, indent=2, sort_keys=True) + "\n",
        )
        fsync_directory(tmp_dir)
        os.replace(tmp_dir, final_dir)
        fsync_directory(final_dir.parent)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            shutil.rmtree(tmp_dir)
        raise
    return collect_inbox_attachments(item_path)


def archive_inbox_attachments(source_path: Path, archive_path: Path) -> None:
    source_dir = inbox_attachment_dir(source_path)
    if not source_dir.is_dir():
        return
    archive_dir = inbox_attachment_dir(archive_path)
    archive_dir.parent.mkdir(parents=True, exist_ok=True)
    if archive_dir.exists():
        shutil.rmtree(source_dir)
        return
    os.replace(source_dir, archive_dir)
    fsync_directory(archive_dir.parent)


def remove_inbox_attachment_dir(path: Path) -> None:
    with contextlib.suppress(FileNotFoundError):
        shutil.rmtree(path)


def attachment_text_path(directory: Path) -> Path:
    stem = directory.name.removesuffix(INBOX_ATTACHMENT_DIR_SUFFIX)
    return directory.with_name(f"{stem}.txt")


def archive_inbox_attachment_references(text: str) -> str:
    """Point live inbox attachment references at their deterministic archive path."""
    if not text:
        return text
    return _INBOX_LIVE_ATTACHMENT_REF_RE.sub(_archive_inbox_attachment_reference, text)


def find_archived_inbox_attachment_references(text: str) -> tuple[str, ...]:
    """Return archived inbox attachment references without surrounding punctuation."""
    if not text:
        return ()
    refs: list[str] = []
    for match in _INBOX_ARCHIVED_ATTACHMENT_REF_RE.finditer(text):
        ref = match.group("ref").rstrip(_TRAILING_REF_PUNCTUATION)
        if ref:
            refs.append(ref)
    return tuple(refs)


def _archive_inbox_attachment_reference(match: re.Match[str]) -> str:
    head = match.group("head")
    tail = match.group("tail")
    punctuation = ""
    while tail.endswith(tuple(_TRAILING_REF_PUNCTUATION)):
        punctuation = f"{tail[-1]}{punctuation}"
        tail = tail[:-1]
    separator = "\\" if head.endswith("\\") else "/"
    return f"{head}archive{separator}{tail}{punctuation}"


def _attachment_payload(item: Mapping[str, Any]) -> tuple[str, str]:
    data_url = str(item.get("dataUrl") or item.get("data_url") or "")
    declared_type = str(
        item.get("contentType") or item.get("content_type") or ""
    ).strip()
    if data_url:
        match = _DATA_URL_PREFIX_RE.match(data_url)
        if not match:
            raise ValueError("Attachment dataUrl must be base64.")
        content_type = match.group("content_type").strip() or declared_type
        return content_type.lower(), "".join(match.group("data").split())
    encoded = str(item.get("data") or item.get("base64") or "")
    if not encoded:
        raise ValueError("Attachment data is required.")
    return declared_type.lower(), "".join(encoded.split())


def _attachment_name(raw: Any, content_type: str, index: int) -> str:
    fallback_extension = mimetypes.guess_extension(content_type) or ".img"
    raw_name = Path(str(raw or f"image-{index}{fallback_extension}")).name
    cleaned = _SAFE_ATTACHMENT_NAME_RE.sub("-", raw_name).strip(".-")
    if not cleaned:
        cleaned = f"image-{index}{fallback_extension}"
    if "." not in cleaned:
        cleaned += fallback_extension
    return cleaned[:INBOX_ATTACHMENT_NAME_MAX_CHARS]


def _write_bytes_fsynced(path: Path, data: bytes) -> None:
    with path.open("wb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())


def _write_text_fsynced(path: Path, text: str) -> None:
    with path.open("w", encoding="utf-8") as handle:
        handle.write(text)
        handle.flush()
        os.fsync(handle.fileno())
