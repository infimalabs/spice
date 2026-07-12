"""Task-document family export and normal-form rendering."""

from __future__ import annotations

from typing import Never

from spice.errors import SpiceError
from spice.tasks.markdown.dialect import Doc, graph_signature


def export_document(document: Doc) -> Never:
    """Render a task-document graph in ledger normal form."""
    graph_signature(document)
    raise SpiceError("task-document ledger export is not implemented")


def export_ledger(handle: str) -> Never:
    """Export the task family containing ``handle`` in ledger normal form."""
    document = _load_family(handle)
    return export_document(document)


def _load_family(handle: str) -> Doc:
    raise SpiceError("task-family loading is not implemented")
