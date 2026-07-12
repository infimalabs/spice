"""Task-document family matching, planning, and application."""

from __future__ import annotations

from os import PathLike
from typing import Never

from spice.errors import SpiceError
from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import Doc
from spice.tasks.taskdoc import read_document


def apply_document(
    document: Doc,
    *,
    project: str | None,
    origin: str | None,
    dry_run: bool = False,
) -> Never:
    """Plan and apply a parsed task document to its board family."""
    raise SpiceError("task-document apply is not implemented")


def ingest_path(
    path: str | PathLike[str],
    *,
    project: str | None,
    priority: str | None = None,
    origin: str | None = None,
    creation_surface: str | None = None,
    dry_run: bool = False,
) -> Never:
    """Read, parse, and apply one task document."""
    document = parse(read_document(str(path)))
    return apply_document(
        document,
        project=project,
        origin=origin,
        dry_run=dry_run,
    )
