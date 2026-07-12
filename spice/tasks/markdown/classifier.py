"""Task-document line classification and graph parsing."""

from __future__ import annotations

from spice.errors import SpiceError
from spice.tasks.markdown.dialect import TaskDocument


def parse(text: str) -> TaskDocument:
    """Parse task-document text into its graph representation."""
    raise SpiceError("task-document parsing is not implemented")
