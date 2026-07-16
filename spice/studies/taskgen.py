"""Shared task creation for studies that turn findings into board work."""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass

from spice.tasks import claimstate, create, identity, tw

_FINDING_TAG_PREFIX = "study_finding"
_FINDING_DIGEST_LENGTH = 16


@dataclass(frozen=True)
class StudyTaskSpec:
    """Task metadata plus the stable semantic identity of one study finding."""

    study: str
    finding_identity: tuple[str, ...]
    title: str
    project: str
    tags: tuple[str, ...]
    acceptance: tuple[str, ...]


def create_study_tasks(
    specs: list[StudyTaskSpec],
    *,
    deferred: bool = False,
    origin: str | None = None,
    print_created: bool = True,
) -> list[str]:
    """Create or reuse actionable tasks for normalized study findings."""
    handles: list[str] = []
    actionable_rows = _actionable_rows()
    completed_rows = tw.export(["status:completed"])
    for spec in specs:
        finding_tag = _finding_tag(spec)
        existing = _matching_rows(actionable_rows, spec, finding_tag)
        if existing:
            handle = identity.render_handle(existing[0])
            handles.append(handle)
            if print_created:
                print(f"  task reused: {handle}")
            continue

        prior = _matching_rows(completed_rows, spec, finding_tag)
        handle = create.add(
            spec.title,
            description=_finding_description(spec),
            project=spec.project,
            tags=[*spec.tags, finding_tag],
            acceptance=list(spec.acceptance),
            deferred=deferred,
            origin=origin,
        )
        handles.append(handle)
        created_row = identity.resolve(handle)
        actionable_rows.append(created_row)
        if prior:
            previous_handle = identity.render_handle(prior[-1])
            claimstate.annotate(
                identity.uuid_of(created_row),
                f"study finding recurred after completed task {previous_handle}",
            )
        if print_created:
            print(f"  task created: {handle}")
    return handles


def _actionable_rows() -> list[dict[str, object]]:
    return tw.export(["(", "status:pending", "or", "status:waiting", ")"])


def _matching_rows(
    rows: list[dict[str, object]], spec: StudyTaskSpec, finding_tag: str
) -> list[dict[str, object]]:
    matches = [
        row
        for row in rows
        if str(row.get("project") or "") == spec.project
        and finding_tag in _row_tags(row)
    ]
    return sorted(matches, key=lambda row: str(row.get("incepted") or ""))


def _finding_tag(spec: StudyTaskSpec) -> str:
    identity_text = "\0".join((spec.study, *spec.finding_identity))
    digest = hashlib.sha256(identity_text.encode("utf-8")).hexdigest()[
        :_FINDING_DIGEST_LENGTH
    ]
    study = re.sub(r"[^0-9a-z]+", "_", spec.study.lower()).strip("_")
    return f"{_FINDING_TAG_PREFIX}_{study}_{digest}"


def _finding_description(spec: StudyTaskSpec) -> str:
    identity_text = " | ".join(spec.finding_identity)
    return f"Study finding identity: {spec.study} | {identity_text}"


def _row_tags(row: dict[str, object]) -> tuple[str, ...]:
    raw = row.get("tags")
    if not isinstance(raw, list):
        return ()
    return tuple(str(tag) for tag in raw)
