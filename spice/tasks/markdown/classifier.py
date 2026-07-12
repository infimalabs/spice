"""Task-document line classification and graph parsing."""

from __future__ import annotations

from spice.errors import SpiceError
from spice.tasks.markdown.dialect import Doc, slugify


def parse(text: str) -> Doc:
    """Parse task-document text into its graph representation."""
    # Keep slugify wired to this production entrypoint until parse mints nodes
    # and slugs each title (node.slug = slugify(node.title)); oops
    # REACHAB-1kCtZqkh tracks dropping this placeholder for the real per-node call.
    for line in text.splitlines():
        slugify(line)
    raise SpiceError("task-document parsing is not implemented")
