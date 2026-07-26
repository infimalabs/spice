"""Transcript image extraction: rollout lines that carry pictures.

Images reach here as the typed `Image` facts a transcript line decoded into --
pictures an assistant message carried, pictures a tool handed back, or (as a
`view_image` tool call) a file the agent looked at. Each becomes ordinary image
markdown. Embedded base64 payloads are rewritten to an API URL that decodes the
image straight from the transcript line on demand, so transcripts stay the
single source of truth and nothing is copied out of them.

An embedded URL addresses its picture by the image's position among the typed
`Image` facts on its line, which is the same ordering `rollout_image_from_offset`
walks when the browser asks for the bytes.
"""

from __future__ import annotations

import base64
import binascii
import json
import re
from collections.abc import Sequence
from pathlib import Path
from urllib.parse import quote

from spice.agent.driver import AgentDriver
from spice.transcript.events import Image, ToolCall
from spice.transcript.reader import TranscriptEventReader

DATA_IMAGE_RE = re.compile(r"^data:(image/[a-zA-Z0-9.+-]+);base64,(.*)$", re.DOTALL)
VIEW_IMAGE_TOOL = "view_image"


def image_markdown(
    images: Sequence[Image], *, worktree_id: str | None, source_offset: int | None
) -> str | None:
    """Markdown for every picture one transcript line carried, or None."""
    parts: list[str] = []
    for item_index, image in enumerate(images):
        if not image.url:
            continue
        target = image.url
        if (
            worktree_id
            and source_offset is not None
            and DATA_IMAGE_RE.match(image.url) is not None
        ):
            target = embedded_image_url(
                worktree_id, source_offset=source_offset, item_index=item_index
            )
        parts.append(markdown_image_reference(image.content_type or "image", target))
    return "\n\n".join(parts) if parts else None


def view_image_markdown(call: ToolCall) -> str | None:
    """Markdown for the file a `view_image` tool call named, or None."""
    if call.custom or call.name != VIEW_IMAGE_TOOL:
        return None
    try:
        args = json.loads(call.arguments or "")
    except json.JSONDecodeError:
        return None
    path = args.get("path") if isinstance(args, dict) else None
    if not path:
        return None
    return markdown_image_reference(VIEW_IMAGE_TOOL, str(path))


def rollout_image_from_offset(
    rollout_path: Path, *, offset: int, item_index: int, driver: AgentDriver
) -> tuple[bytes, str] | None:
    """Decode the embedded image at (line offset, image position) in a rollout."""
    if offset < 0 or item_index < 0:
        return None
    read = TranscriptEventReader(rollout_path, driver, source_actor=None).read(
        "bounded", start_offset=offset, end_offset=offset + 1
    )
    images = [event for event in read.events if isinstance(event, Image)]
    if item_index >= len(images):
        return None
    return _decode_data_image(images[item_index].url)


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


def worktree_file_image_url(
    worktree_id: str, path: str, *, missing_placeholder: bool = True
) -> str:
    encoded = quote(worktree_id, safe="")
    url = f"/api/work/trees/{encoded}/files/image?path={quote(path, safe='/')}"
    if missing_placeholder:
        url += "&missing=placeholder"
    return url


def embedded_image_url(worktree_id: str, *, source_offset: int, item_index: int) -> str:
    encoded = quote(worktree_id, safe="")
    return (
        f"/api/work/trees/{encoded}/messages/image"
        f"?offset={source_offset}&item={item_index}"
    )


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
