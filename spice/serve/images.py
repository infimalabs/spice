"""Transcript image extraction: rollout lines that carry pictures.

Three rollout shapes carry images: assistant messages whose content list
holds ``image_url`` items, ``function_call_output`` payloads whose output
list holds them, and ``view_image`` tool calls naming a file the agent
looked at. Each becomes ordinary image markdown. Embedded base64 payloads
are rewritten to an API URL that decodes the image straight from the
transcript line on demand, so transcripts stay the single source of truth
and nothing is copied out of them.
"""

from __future__ import annotations

import base64
import binascii
import json
import re
from pathlib import Path
from typing import Any
from urllib.parse import quote

from spice.agent.driver import driver_for_transcript

DATA_IMAGE_RE = re.compile(r"^data:(image/[a-zA-Z0-9.+-]+);base64,(.*)$", re.DOTALL)


def assistant_image_markdown(
    payload: dict[str, Any], *, worktree_id: str | None, source_offset: int | None
) -> str | None:
    if payload.get("type") != "message" or payload.get("role") != "assistant":
        return None
    content = payload.get("content")
    if not isinstance(content, list):
        return None
    return _image_items_markdown(
        content, worktree_id=worktree_id, source_offset=source_offset
    )


def tool_output_image_markdown(
    payload: dict[str, Any], *, worktree_id: str | None, source_offset: int | None
) -> str | None:
    if payload.get("type") != "function_call_output":
        return None
    output = payload.get("output")
    if not isinstance(output, list):
        return None
    return _image_items_markdown(
        output, worktree_id=worktree_id, source_offset=source_offset
    )


def view_image_markdown(payload: dict[str, Any]) -> str | None:
    if payload.get("type") != "function_call" or payload.get("name") != "view_image":
        return None
    raw = payload.get("arguments")
    if not isinstance(raw, str):
        return None
    try:
        args = json.loads(raw)
    except json.JSONDecodeError:
        return None
    path = args.get("path") if isinstance(args, dict) else None
    if not path:
        return None
    return markdown_image_reference("view_image", str(path))


def rollout_image_from_offset(
    rollout_path: Path, *, offset: int, item_index: int
) -> tuple[bytes, str] | None:
    """Decode the embedded image at (line offset, content item) in a rollout."""
    if offset < 0 or item_index < 0:
        return None
    try:
        with rollout_path.open("rb") as handle:
            handle.seek(offset)
            raw_line = handle.readline()
    except OSError:
        return None
    try:
        loaded = json.loads(raw_line.decode("utf-8", errors="replace"))
    except json.JSONDecodeError:
        return None
    if not isinstance(loaded, dict):
        return None
    event = driver_for_transcript(rollout_path).normalize_transcript_line(loaded)
    if event is None or event.get("type") != "response_item":
        return None
    payload = event.get("payload")
    if not isinstance(payload, dict):
        return None
    items = _image_content_items(payload)
    if items is None or item_index >= len(items):
        return None
    return _decode_data_image(_item_image_url(items[item_index]))


def markdown_image_reference(alt: str, target: str) -> str:
    escaped_alt = alt.replace("]", "\\]")
    escaped_target = (
        target.replace("%", "%25")
        .replace(" ", "%20")
        .replace("(", "%28")
        .replace(")", "%29")
        .replace("<", "%3C")
        .replace(">", "%3E")
        .replace("\n", "%0A")
    )
    return f"![{escaped_alt}]({escaped_target})"


def worktree_file_image_url(worktree_id: str, path: str) -> str:
    encoded = quote(worktree_id, safe="")
    return f"/api/work/trees/{encoded}/files/image?path={quote(path, safe='/')}"


def embedded_image_url(worktree_id: str, *, source_offset: int, item_index: int) -> str:
    encoded = quote(worktree_id, safe="")
    return (
        f"/api/work/trees/{encoded}/messages/image"
        f"?offset={source_offset}&item={item_index}"
    )


def _image_items_markdown(
    items: list[Any], *, worktree_id: str | None, source_offset: int | None
) -> str | None:
    parts: list[str] = []
    for item_index, item in enumerate(items):
        url = _item_image_url(item)
        if not url:
            continue
        alt = "image"
        if isinstance(item, dict):
            alt = str(item.get("alt") or item.get("type") or "image")
        target = url
        if (
            worktree_id
            and source_offset is not None
            and DATA_IMAGE_RE.match(url) is not None
        ):
            target = embedded_image_url(
                worktree_id, source_offset=source_offset, item_index=item_index
            )
        parts.append(markdown_image_reference(alt, target))
    return "\n\n".join(parts) if parts else None


def _image_content_items(payload: dict[str, Any]) -> list[Any] | None:
    if payload.get("type") == "function_call_output":
        output = payload.get("output")
        return output if isinstance(output, list) else None
    if payload.get("type") == "message" and payload.get("role") == "assistant":
        content = payload.get("content")
        return content if isinstance(content, list) else None
    return None


def _item_image_url(item: Any) -> str:
    if not isinstance(item, dict):
        return ""
    raw = item.get("image_url") or item.get("url") or ""
    url = raw.get("url") if isinstance(raw, dict) else raw
    return str(url or "")


def _decode_data_image(target: str) -> tuple[bytes, str] | None:
    match = DATA_IMAGE_RE.match(target)
    if match is None:
        return None
    mime_type, encoded = match.groups()
    compact = "".join(encoded.split())
    try:
        return base64.b64decode(compact, validate=True), mime_type
    except (binascii.Error, ValueError):
        return None
