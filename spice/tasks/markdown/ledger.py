"""Task-document family export and normal-form rendering."""

from __future__ import annotations

from typing import Never

from spice.errors import SpiceError
from spice.tasks.markdown.dialect import TaskDocument


def export_document(document: TaskDocument) -> Never:
    """Render a task-document graph in ledger normal form."""
    raise SpiceError("task-document ledger export is not implemented")


def export_ledger(handle: str) -> Never:
    """Export the task family containing ``handle`` in ledger normal form."""
    return export_document(handle)
