"""Task-document family matching, planning, and application."""

from __future__ import annotations

from os import PathLike
from typing import Never

from spice.errors import SpiceError
from spice.tasks import claimstate, config, create, tw
from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import Doc
from spice.tasks.taskdoc import read_document

INGEST_PROJECT_REQUIRED_ERROR = (
    "task ingest requires a project: pass --project <stem.child>, or run while "
    "holding an active claim to inherit its project"
)


def resolve_ingest_project(actor: str, project: str | None) -> str:
    """Resolve the project document-born rows land in.

    An explicit ``--project`` wins and is validated like manual creation;
    otherwise the project is inherited from the actor's active claim -- new
    ingest surface the creation path does not offer. Absent both, ingest
    refuses so no document is written without a home.
    """
    if project is not None:
        return config.validate_manual_creation_project(project)
    claim = claimstate.active_claim(actor)
    if claim is not None:
        claimed = str(claim.get("project") or "")
        if claimed:
            return claimed
    raise SpiceError(INGEST_PROJECT_REQUIRED_ERROR)


def resolve_ingest_target(
    actor: str, *, project: str | None, origin: str | None
) -> tuple[str, str]:
    """Resolve the (project, origin) an apply writes under, before any write.

    Origin reuses the creation-path resolver: an explicit ``--origin`` or the
    actor's active claim, else the creation refusal. Project is the new ingest
    surface: an explicit ``--project`` or the active claim's project, else a
    refusal. Both resolve up front so a missing reference never leaves a
    half-applied document behind.
    """
    resolved_origin = create.resolved_task_origin(origin, actor)
    resolved_project = resolve_ingest_project(actor, project)
    return resolved_project, resolved_origin


def apply_document(
    document: Doc,
    *,
    project: str,
    origin: str,
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
    actor = tw.canonical_actor(tw.current_actor())
    resolved_project, resolved_origin = resolve_ingest_target(
        actor, project=project, origin=origin
    )
    document = parse(read_document(str(path)))
    return apply_document(
        document,
        project=resolved_project,
        origin=resolved_origin,
        dry_run=dry_run,
    )
