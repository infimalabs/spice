"""Task-document family matching, planning, and application."""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from datetime import UTC, datetime
from os import PathLike
from typing import Any

from spice.errors import SpiceError
from spice.tasks import claimstate, config, create, identity, readiness, tw
from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import Doc, Node
from spice.tasks.taskdoc import read_document

INGEST_PROJECT_REQUIRED_ERROR = (
    "task ingest requires a project: pass --project <stem.child>, or run while "
    "holding an active claim to inherit its project"
)
_FAMILY_STATUS_FILTER = (
    "(",
    "status:pending",
    "or",
    "status:waiting",
    "or",
    "status:completed",
    ")",
)


@dataclass(frozen=True)
class FamilyMatch:
    """The visible family rows and exact incoming-node matches by slug."""

    rows: tuple[dict[str, Any], ...]
    by_slug: dict[str, dict[str, Any]]


@dataclass(frozen=True)
class FieldUpdate:
    """One matched-row field that execution must equalize."""

    field: str
    value: str | tuple[str, ...]


@dataclass(frozen=True)
class PlannedNode:
    """A document node bound to either an existing or prospective row."""

    node: Node
    row: dict[str, Any] | None
    handle: str
    incepted: str
    settled: bool
    updates: tuple[FieldUpdate, ...]
    annotations: tuple[str, ...]
    dependency_slugs: tuple[str, ...]


@dataclass(frozen=True)
class EdgeChange:
    """One family-edge mutation in document-slug coordinates."""

    source: str
    target: str


@dataclass(frozen=True)
class PlanVerb:
    """One stable, human-readable fact in an apply report."""

    kind: str
    slug: str = ""
    handle: str = ""
    field: str = ""
    target: str = ""
    line: int = 0
    code: str = ""
    message: str = ""

    def render(self) -> str:
        if self.kind in {"created", "reused", "loose"}:
            return f"{self.kind} {self.slug} {self.handle}"
        if self.kind in {"updated", "drift"}:
            return f"{self.kind} {self.slug} {self.handle} {self.field}"
        if self.kind in {"edge-added", "edge-dropped"}:
            return f"{self.kind} {self.slug} -> {self.target}"
        if self.kind == "warn":
            return f"warn {self.line} {self.code} {self.message}"
        raise ValueError(f"unknown apply-plan verb: {self.kind}")


@dataclass(frozen=True)
class ApplyPlan:
    """Complete, cycle-checked apply intent with no board writes performed."""

    project: str
    origin: str
    root_handle: str
    nodes: tuple[PlannedNode, ...]
    edge_additions: tuple[EdgeChange, ...]
    edge_drops: tuple[EdgeChange, ...]
    verbs: tuple[PlanVerb, ...]

    def report(self) -> str:
        return "\n".join(
            [f"root {self.root_handle}", *(verb.render() for verb in self.verbs)]
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


def load_family_rows(project: str, origin: str) -> list[dict[str, Any]]:
    """Load non-deleted document rows in exactly one project/origin family."""
    rows = tw.export(
        [
            *_FAMILY_STATUS_FILTER,
            f"project.is:{project}",
            f"origin.is:{origin}",
        ]
    )
    family = [
        row
        for row in rows
        if str(row.get("project") or "") == project
        and str(row.get("origin") or "") == origin
        and str(row.get(config.TASKDOC_ID_UDA) or "")
    ]
    return sorted(family, key=identity.render_handle)


def match_family(document: Doc, *, project: str, origin: str) -> FamilyMatch:
    """Match document nodes to exact-slug rows within one visible family."""
    rows = load_family_rows(project, origin)
    rows_by_slug: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        slug = str(row.get(config.TASKDOC_ID_UDA) or "")
        rows_by_slug.setdefault(slug, []).append(row)

    matched: dict[str, dict[str, Any]] = {}
    for node in document.nodes:
        candidates = rows_by_slug.get(node.slug, [])
        if len(candidates) > 1:
            handles = sorted(identity.render_handle(row) for row in candidates)
            raise SpiceError(
                f"{node.slug} is ambiguous in family: {', '.join(handles)}"
            )
        if candidates:
            matched[node.slug] = candidates[0]
    return FamilyMatch(rows=tuple(rows), by_slug=matched)


_FIELD_ORDER = ("description", "acceptance", "priority", "due", "tags")
_ALL_VISIBLE_STATUS_FILTER = [*_FAMILY_STATUS_FILTER]
_TASKWARRIOR_UTC_TIMESTAMP_LENGTH = 16


def _is_settled(row: dict[str, Any]) -> bool:
    """Whether runtime state has permanently transferred ownership to board."""
    return (
        str(row.get("status") or "") != "pending"
        or claimstate.phase_index(row) > 0
        or bool(row.get("start"))
        or bool(row.get("claim_at"))
    )


def _normalized_tag(tag: str) -> str:
    return "".join(c if c.isalnum() else "_" for c in tag.strip().lower()).strip("_")


def _normalized_tags(tags: Iterable[object]) -> tuple[str, ...]:
    return tuple(
        sorted(
            {normalized for tag in tags if (normalized := _normalized_tag(str(tag)))}
        )
    )


def _desired_flow(node: Node, project: str) -> tuple[str, ...]:
    if node.flow:
        return tuple(config.resolve_flow(list(node.flow), project))
    default = config.resolve_flow(None, project)
    if node.acceptance or default[0] == "plan":
        return tuple(default)
    return ("plan", *(phase for phase in default if phase != "plan"))


def _normalized_due(value: str) -> str:
    if not value:
        return ""
    if len(value) == _TASKWARRIOR_UTC_TIMESTAMP_LENGTH and value.endswith("Z"):
        try:
            return (
                datetime.strptime(value, "%Y%m%dT%H%M%SZ")
                .replace(tzinfo=UTC)
                .strftime("%Y%m%dT%H%M%SZ")
            )
        except ValueError:
            pass
    result = tw.run(["calc", value])
    try:
        resolved = datetime.fromisoformat(result.stdout.strip()).astimezone(UTC)
    except ValueError as exc:
        raise SpiceError(f"invalid due date: {value}") from exc
    return resolved.strftime("%Y%m%dT%H%M%SZ")


def _node_fields(node: Node) -> dict[str, str | tuple[str, ...]]:
    return {
        "description": node.description(),
        "acceptance": " | ".join(node.acceptance),
        "priority": config.map_priority(node.priority or "none"),
        "due": node.due or "",
        "tags": _normalized_tags(node.tags),
    }


def _row_field(row: dict[str, Any], field: str) -> str | tuple[str, ...]:
    if field == "description":
        return str(row.get("task_description") or "").strip()
    if field == "tags":
        return _normalized_tags(row.get("tags") or ())
    return str(row.get(field) or "")


def _fields_differ(
    field: str, desired: str | tuple[str, ...], current: str | tuple[str, ...]
) -> bool:
    if field == "due":
        return _normalized_due(str(desired)) != _normalized_due(str(current))
    return desired != current


def _annotation_additions(node: Node, row: dict[str, Any] | None) -> tuple[str, ...]:
    existing = {
        str(annotation.get("description") or "").rstrip()
        for annotation in ((row or {}).get("annotations") or ())
        if isinstance(annotation, dict)
    }
    additions: list[str] = []
    for block in node.annotations:
        normalized = block.rstrip()
        if normalized not in existing:
            additions.append(normalized)
            existing.add(normalized)
    return tuple(additions)


def _creation_order(document: Doc) -> tuple[int, ...]:
    """Stable dependency post-order: prerequisites first and root last."""
    adjacency: dict[int, list[int]] = {node.idx: [] for node in document.nodes}
    for source, target, _kind in document.edges:
        adjacency[source].append(target)
    for targets in adjacency.values():
        targets.sort()

    visited: set[int] = set()
    order: list[int] = []

    def visit(start: int) -> None:
        stack: list[tuple[int, bool]] = [(start, False)]
        while stack:
            node_idx, exiting = stack.pop()
            if exiting:
                order.append(node_idx)
                continue
            if node_idx in visited:
                continue
            visited.add(node_idx)
            stack.append((node_idx, True))
            stack.extend((target, False) for target in reversed(adjacency[node_idx]))

    visit(document.root)
    for node in document.nodes:
        visit(node.idx)
    return tuple(order)


def _desired_edges(document: Doc) -> set[tuple[str, str]]:
    return {
        (document.nodes[source].slug, document.nodes[target].slug)
        for source, target, _kind in document.edges
    }


def _depends(row: dict[str, Any]) -> set[str]:
    raw = row.get("depends") or ()
    if isinstance(raw, str):
        return {raw} if raw else set()
    return {str(value) for value in raw if value}


def _post_state_adjacency(
    *,
    board_rows: Iterable[dict[str, Any]],
    family: FamilyMatch,
    row_ids: dict[str, str],
    planned_additions: set[tuple[str, str]],
    planned_drops: set[tuple[str, str]],
) -> dict[str, set[str]]:
    family_ids = {
        str(row.get("uuid") or "") for row in family.rows if str(row.get("uuid") or "")
    }
    adjacency: dict[str, set[str]] = {}
    for row in board_rows:
        source = str(row.get("uuid") or "")
        if source:
            adjacency.setdefault(source, set()).update(_depends(row))

    for source_slug, target_slug in planned_drops:
        source = row_ids[source_slug]
        target = row_ids[target_slug]
        if source in family_ids and target in family_ids:
            adjacency.setdefault(source, set()).discard(target)
    for source_slug, target_slug in planned_additions:
        adjacency.setdefault(row_ids[source_slug], set()).add(row_ids[target_slug])
    for row_id in row_ids.values():
        adjacency.setdefault(row_id, set())
    return adjacency


def _first_cycle(adjacency: dict[str, set[str]]) -> tuple[str, ...]:
    vertices = set(adjacency)
    vertices.update(target for targets in adjacency.values() for target in targets)
    white, grey, black = 0, 1, 2
    color = dict.fromkeys(vertices, white)
    for start in sorted(vertices):
        if color[start] != white:
            continue
        color[start] = grey
        stack = [(start, iter(sorted(adjacency.get(start, ()))))]
        while stack:
            current, neighbors = stack[-1]
            target = next(neighbors, None)
            if target is None:
                color[current] = black
                stack.pop()
            elif color.get(target, white) == grey:
                path = tuple(item[0] for item in stack)
                return path[path.index(target) :]
            elif color.get(target, white) == white:
                color[target] = grey
                stack.append((target, iter(sorted(adjacency.get(target, ())))))
    return ()


def _cycle_document_slug(
    cycle: tuple[str, ...], document: Doc, row_ids: dict[str, str]
) -> str:
    node_order = {node.slug: node.idx for node in document.nodes}
    slug_by_id = {
        row_id: slug for slug, row_id in row_ids.items() if slug in node_order
    }
    slugs = [slug_by_id[row_id] for row_id in cycle if row_id in slug_by_id]
    return min(
        slugs or [document.nodes[document.root].slug], key=node_order.__getitem__
    )


def _refuse_post_state_cycle(
    *,
    document: Doc,
    board_rows: Iterable[dict[str, Any]],
    family: FamilyMatch,
    row_ids: dict[str, str],
    planned_additions: set[tuple[str, str]],
    planned_drops: set[tuple[str, str]],
) -> None:
    adjacency = _post_state_adjacency(
        board_rows=board_rows,
        family=family,
        row_ids=row_ids,
        planned_additions=planned_additions,
        planned_drops=planned_drops,
    )
    cycle = _first_cycle(adjacency)
    if cycle:
        slug = _cycle_document_slug(cycle, document, row_ids)
        raise SpiceError(f"dependency cycle at {slug}")


@dataclass(frozen=True)
class _PlanIdentities:
    handles: dict[str, str]
    incepted: dict[str, str]
    creation_order: tuple[int, ...]


@dataclass(frozen=True)
class _NodeDiff:
    planned: PlannedNode
    update_verbs: tuple[PlanVerb, ...]
    drift_verbs: tuple[PlanVerb, ...]
    additions: frozenset[tuple[str, str]]
    drops: frozenset[tuple[str, str]]
    edge_drift: bool


def _validate_plan_input(document: Doc) -> None:
    if document.refusals:
        raise SpiceError(document.refusals[0])
    for node in document.nodes:
        if any("|" in criterion for criterion in node.acceptance):
            raise SpiceError(f"acceptance criterion on {node.slug} contains '|'")
        if node.due:
            _normalized_due(node.due)


def _desired_targets(document: Doc) -> dict[str, set[str]]:
    desired_by_source: dict[str, set[str]] = {
        node.slug: set() for node in document.nodes
    }
    for source, target in _desired_edges(document):
        desired_by_source[source].add(target)
    return desired_by_source


def _plan_identities(
    document: Doc,
    family: FamilyMatch,
    board_rows: Iterable[dict[str, Any]],
    project: str,
) -> _PlanIdentities:
    existing_incepted = {
        str(row.get("incepted") or "") for row in board_rows if row.get("incepted")
    }
    handles: dict[str, str] = {
        slug: identity.render_handle(row) for slug, row in family.by_slug.items()
    }
    incepted: dict[str, str] = {
        slug: str(row.get("incepted") or "") for slug, row in family.by_slug.items()
    }
    creation_order = _creation_order(document)
    for node_idx in creation_order:
        node = document.nodes[node_idx]
        if node.slug in handles:
            continue
        stamp = identity.mint_incepted(existing_incepted)
        existing_incepted.add(stamp)
        incepted[node.slug] = stamp
        handles[node.slug] = f"{identity.key_for(project, node.title)}-{stamp}"
    return _PlanIdentities(handles, incepted, creation_order)


def _family_row_ids(document: Doc, family: FamilyMatch) -> dict[str, str]:
    row_ids: dict[str, str] = {
        str(row.get(config.TASKDOC_ID_UDA) or ""): str(row.get("uuid") or "")
        or f"existing:{row.get(config.TASKDOC_ID_UDA)}"
        for row in family.rows
    }
    row_ids.update(
        {
            node.slug: f"new:{node.slug}"
            for node in document.nodes
            if node.slug not in family.by_slug
        }
    )
    return row_ids


def _family_slugs_by_uuid(family: FamilyMatch) -> dict[str, str]:
    return {
        str(row.get("uuid") or ""): str(row.get(config.TASKDOC_ID_UDA) or "")
        for row in family.rows
        if row.get("uuid")
    }


def _field_actions(
    node: Node, row: dict[str, Any] | None, settled: bool, project: str
) -> tuple[list[FieldUpdate], list[str]]:
    updates: list[FieldUpdate] = []
    drifts: list[str] = []
    if row is None:
        return updates, drifts
    if str(row.get("description") or "") != node.title:
        drifts.append("title")
    for field, desired in _node_fields(node).items():
        if not _fields_differ(field, desired, _row_field(row, field)):
            continue
        if settled:
            drifts.append(field)
        else:
            updates.append(FieldUpdate(field, desired))
    if tuple(claimstate.phases_of(row)) != _desired_flow(node, project):
        drifts.append("flow")
    return updates, drifts


def _edge_actions(
    node: Node,
    row: dict[str, Any] | None,
    settled: bool,
    desired_targets: set[str],
    family_slug_by_uuid: dict[str, str],
) -> tuple[frozenset[tuple[str, str]], frozenset[tuple[str, str]], bool]:
    current_targets = {
        family_slug_by_uuid[target]
        for target in _depends(row or {})
        if target in family_slug_by_uuid
    }
    additions = frozenset(
        (node.slug, target) for target in desired_targets - current_targets
    )
    drops = frozenset(
        (node.slug, target) for target in current_targets - desired_targets
    )
    edge_drift = bool(row and settled and (additions or drops))
    if edge_drift:
        return frozenset(), frozenset(), True
    return additions, drops, False


def _plan_node(
    node: Node,
    *,
    row: dict[str, Any] | None,
    project: str,
    identities: _PlanIdentities,
    desired_targets: set[str],
    family_slug_by_uuid: dict[str, str],
    node_order: dict[str, int],
) -> _NodeDiff:
    settled = bool(row and _is_settled(row))
    updates, drifts = _field_actions(node, row, settled, project)
    annotations = _annotation_additions(node, row)
    if annotations and row is not None:
        updates.append(FieldUpdate("annotations", annotations))
    additions, drops, edge_drift = _edge_actions(
        node, row, settled, desired_targets, family_slug_by_uuid
    )
    handle = identities.handles[node.slug]
    update_verbs = tuple(
        PlanVerb("updated", node.slug, handle, field)
        for field in (*_FIELD_ORDER, "annotations")
        if any(update.field == field for update in updates)
    )
    drift_verbs = tuple(
        PlanVerb("drift", node.slug, handle, field)
        for field in ("title", *_FIELD_ORDER, "flow")
        if field in drifts
    )
    planned = PlannedNode(
        node=node,
        row=row,
        handle=handle,
        incepted=identities.incepted[node.slug],
        settled=settled,
        updates=tuple(updates),
        annotations=annotations,
        dependency_slugs=tuple(sorted(desired_targets, key=node_order.__getitem__)),
    )
    return _NodeDiff(planned, update_verbs, drift_verbs, additions, drops, edge_drift)


def _plan_node_diffs(
    document: Doc,
    *,
    family: FamilyMatch,
    project: str,
    identities: _PlanIdentities,
    desired_by_source: dict[str, set[str]],
) -> tuple[_NodeDiff, ...]:
    family_slug_by_uuid = _family_slugs_by_uuid(family)
    node_order = {node.slug: node.idx for node in document.nodes}
    return tuple(
        _plan_node(
            node,
            row=family.by_slug.get(node.slug),
            project=project,
            identities=identities,
            desired_targets=desired_by_source[node.slug],
            family_slug_by_uuid=family_slug_by_uuid,
            node_order=node_order,
        )
        for node in document.nodes
    )


def _edge_sort_key(
    edge: tuple[str, str], node_order: dict[str, int]
) -> tuple[int, int, str]:
    source, target = edge
    return node_order[source], node_order.get(target, len(node_order)), target


def _node_report_verbs(
    document: Doc,
    family: FamilyMatch,
    identities: _PlanIdentities,
    diffs: tuple[_NodeDiff, ...],
    edge_additions: set[tuple[str, str]],
    edge_drops: set[tuple[str, str]],
) -> tuple[tuple[PlanVerb, ...], tuple[PlanVerb, ...], tuple[PlanVerb, ...]]:
    created_slugs = {
        node.slug for node in document.nodes if node.slug not in family.by_slug
    }
    changed_slugs = {verb.slug for diff in diffs for verb in diff.update_verbs} | {
        source for source, _target in edge_additions | edge_drops
    }
    created = tuple(
        PlanVerb(
            "created",
            document.nodes[idx].slug,
            identities.handles[document.nodes[idx].slug],
        )
        for idx in identities.creation_order
        if document.nodes[idx].slug in created_slugs
    )
    reused = tuple(
        PlanVerb("reused", node.slug, identities.handles[node.slug])
        for node in document.nodes
        if node.slug in family.by_slug and node.slug not in changed_slugs
    )
    updated = tuple(verb for diff in diffs for verb in diff.update_verbs)
    return created, reused, updated


def _edge_report_verbs(
    document: Doc,
    family: FamilyMatch,
    edge_additions: set[tuple[str, str]],
    edge_drops: set[tuple[str, str]],
) -> tuple[tuple[PlanVerb, ...], tuple[PlanVerb, ...]]:
    created_slugs = {
        node.slug for node in document.nodes if node.slug not in family.by_slug
    }
    node_order = {node.slug: node.idx for node in document.nodes}
    edge_added = tuple(
        PlanVerb("edge-added", source, target=target)
        for source, target in sorted(
            edge_additions, key=lambda edge: _edge_sort_key(edge, node_order)
        )
        if not ({source, target} <= created_slugs)
    )
    edge_dropped = tuple(
        PlanVerb("edge-dropped", source, target=target)
        for source, target in sorted(
            edge_drops, key=lambda edge: _edge_sort_key(edge, node_order)
        )
    )
    return edge_added, edge_dropped


def _standing_fact_verbs(
    document: Doc, family: FamilyMatch, diffs: tuple[_NodeDiff, ...]
) -> tuple[tuple[PlanVerb, ...], tuple[PlanVerb, ...], tuple[PlanVerb, ...]]:
    node_order = {node.slug: node.idx for node in document.nodes}
    listed_slugs = set(node_order)
    loose = tuple(
        PlanVerb(
            "loose",
            str(row.get(config.TASKDOC_ID_UDA) or ""),
            identity.render_handle(row),
        )
        for row in family.rows
        if str(row.get(config.TASKDOC_ID_UDA) or "") not in listed_slugs
    )
    drift = tuple(verb for diff in diffs for verb in diff.drift_verbs) + tuple(
        PlanVerb("drift", diff.planned.node.slug, diff.planned.handle, "after")
        for diff in diffs
        if diff.edge_drift
    )
    warnings = tuple(
        PlanVerb("warn", line=line, code=code, message=message)
        for line, code, message in document.warnings
    )
    return loose, drift, warnings


def _report_verbs(
    document: Doc,
    family: FamilyMatch,
    identities: _PlanIdentities,
    diffs: tuple[_NodeDiff, ...],
    edge_additions: set[tuple[str, str]],
    edge_drops: set[tuple[str, str]],
) -> tuple[PlanVerb, ...]:
    created, reused, updated = _node_report_verbs(
        document, family, identities, diffs, edge_additions, edge_drops
    )
    edge_added, edge_dropped = _edge_report_verbs(
        document, family, edge_additions, edge_drops
    )
    loose, drift, warnings = _standing_fact_verbs(document, family, diffs)
    return (
        *created,
        *reused,
        *updated,
        *edge_added,
        *edge_dropped,
        *loose,
        *drift,
        *warnings,
    )


def plan_document(document: Doc, *, project: str, origin: str) -> ApplyPlan:
    """Compute and validate the complete apply plan without writing the board."""
    _validate_plan_input(document)
    family = match_family(document, project=project, origin=origin)
    board_rows = tw.export(_ALL_VISIBLE_STATUS_FILTER)
    desired_by_source = _desired_targets(document)
    identities = _plan_identities(document, family, board_rows, project)
    row_ids = _family_row_ids(document, family)
    diffs = _plan_node_diffs(
        document,
        family=family,
        project=project,
        identities=identities,
        desired_by_source=desired_by_source,
    )
    edge_additions = {edge for diff in diffs for edge in diff.additions}
    edge_drops = {edge for diff in diffs for edge in diff.drops}
    _refuse_post_state_cycle(
        document=document,
        board_rows=board_rows,
        family=family,
        row_ids=row_ids,
        planned_additions=edge_additions,
        planned_drops=edge_drops,
    )
    verbs = _report_verbs(
        document, family, identities, diffs, edge_additions, edge_drops
    )
    return ApplyPlan(
        project=project,
        origin=origin,
        root_handle=identities.handles[document.nodes[document.root].slug],
        nodes=tuple(diff.planned for diff in diffs),
        edge_additions=tuple(EdgeChange(*edge) for edge in sorted(edge_additions)),
        edge_drops=tuple(EdgeChange(*edge) for edge in sorted(edge_drops)),
        verbs=verbs,
    )


def _created_slugs(plan: ApplyPlan) -> tuple[str, ...]:
    return tuple(verb.slug for verb in plan.verbs if verb.kind == "created")


def _row_by_incepted(incepted: str) -> dict[str, Any]:
    rows = tw.export([f"incepted.is:{incepted}"])
    if len(rows) != 1:
        raise SpiceError(f"created task identity is ambiguous: {incepted}")
    return rows[0]


def _create_plan_rows(plan: ApplyPlan) -> dict[str, dict[str, Any]]:
    planned_by_slug = {planned.node.slug: planned for planned in plan.nodes}
    rows_by_slug = {
        planned.node.slug: planned.row
        for planned in plan.nodes
        if planned.row is not None
    }
    actor = tw.canonical_actor(tw.current_actor())
    for slug in _created_slugs(plan):
        planned = planned_by_slug[slug]
        node = planned.node
        dependency_handles = [
            identity.render_handle(rows_by_slug[target])
            for target in planned.dependency_slugs
        ]
        parent = plan.nodes[node.parent].node.slug if node.parent is not None else ""
        args = create._build_add_args(
            title=node.title,
            body=node.description() or None,
            actor=actor,
            incepted=planned.incepted,
            resolved_project=plan.project,
            phases=list(_desired_flow(node, plan.project)),
            priority=node.priority or "none",
            tags=list(node.tags),
            after=dependency_handles,
            acceptance=list(node.acceptance),
            wait=None,
            scheduled=None,
            until=None,
            due=node.due,
            extra=[
                f"{config.TASKDOC_ID_UDA}:{node.slug}",
                f"{config.TASKDOC_PARENT_UDA}:{parent}",
            ],
            creation_surface=None,
            origin=plan.origin,
            auto_due=False,
        )
        tw.run(args)
        rows_by_slug[slug] = _row_by_incepted(planned.incepted)
    return rows_by_slug


def _fresh_row(row: dict[str, Any]) -> dict[str, Any]:
    uuid = identity.uuid_of(row)
    rows = tw.export([uuid])
    if len(rows) != 1:
        raise SpiceError(
            f"task disappeared during apply: {identity.render_handle(row)}"
        )
    return rows[0]


def _field_modifications(
    updates: Iterable[FieldUpdate], fresh: dict[str, Any]
) -> list[str]:
    modifications: list[str] = []
    for update in updates:
        if update.field == "annotations":
            continue
        if update.field == "description":
            modifications.append(f"task_description:{update.value}")
        elif update.field == "tags":
            desired = set(update.value)
            current = set(_normalized_tags(fresh.get("tags") or ()))
            modifications.extend(f"-{tag}" for tag in sorted(current - desired))
            modifications.extend(f"+{tag}" for tag in sorted(desired - current))
        else:
            modifications.append(f"{update.field}:{update.value}")
    return modifications


def _append_annotations(planned: PlannedNode, row: dict[str, Any]) -> None:
    uuid = identity.uuid_of(row)
    for block in planned.annotations:
        claimstate.annotate(uuid, block)


def _execute_field_updates(
    plan: ApplyPlan, rows_by_slug: dict[str, dict[str, Any]]
) -> set[tuple[str, str]]:
    demoted: set[tuple[str, str]] = set()
    for planned in plan.nodes:
        row = rows_by_slug[planned.node.slug]
        statement_updates = tuple(
            update for update in planned.updates if update.field != "annotations"
        )
        if planned.row is not None and statement_updates:
            fresh = _fresh_row(row)
            rows_by_slug[planned.node.slug] = fresh
            if _is_settled(fresh):
                demoted.update(
                    (planned.node.slug, update.field) for update in statement_updates
                )
            else:
                modifications = _field_modifications(statement_updates, fresh)
                if modifications:
                    tw.run([identity.uuid_of(fresh), "modify", *modifications])
        _append_annotations(planned, rows_by_slug[planned.node.slug])
    return demoted


def _family_execution_rows(
    plan: ApplyPlan, listed_rows: dict[str, dict[str, Any]]
) -> dict[str, dict[str, Any]]:
    rows = {
        str(row.get(config.TASKDOC_ID_UDA) or ""): row
        for row in load_family_rows(plan.project, plan.origin)
    }
    rows.update(listed_rows)
    return rows


def _execute_edge_change(
    change: EdgeChange,
    *,
    drop: bool,
    planned_by_slug: dict[str, PlannedNode],
    rows_by_slug: dict[str, dict[str, Any]],
) -> bool:
    planned = planned_by_slug[change.source]
    if planned.row is None:
        return True
    fresh = _fresh_row(rows_by_slug[change.source])
    rows_by_slug[change.source] = fresh
    if _is_settled(fresh):
        return False
    uuid = identity.uuid_of(fresh)
    changed_at = tw.now_iso()
    target_uuid = identity.uuid_of(rows_by_slug[change.target])
    modifier = f"depends:-{target_uuid}" if drop else f"depends:{target_uuid}"
    dependencies = _depends(fresh)
    if drop:
        dependencies.discard(target_uuid)
    else:
        dependencies.add(target_uuid)
    transition = readiness.dependency_transition_args(
        fresh,
        dependencies=dependencies,
        at=changed_at,
    )
    tw.run([uuid, "modify", modifier, *transition])
    return True


def _execute_edge_updates(
    plan: ApplyPlan, listed_rows: dict[str, dict[str, Any]]
) -> set[tuple[str, str, str]]:
    rows_by_slug = _family_execution_rows(plan, listed_rows)
    planned_by_slug = {planned.node.slug: planned for planned in plan.nodes}
    demoted: set[tuple[str, str, str]] = set()
    for kind, changes, drop in (
        ("edge-added", plan.edge_additions, False),
        ("edge-dropped", plan.edge_drops, True),
    ):
        for change in changes:
            landed = _execute_edge_change(
                change,
                drop=drop,
                planned_by_slug=planned_by_slug,
                rows_by_slug=rows_by_slug,
            )
            if not landed:
                demoted.add((kind, change.source, change.target))
    return demoted


def _execution_verbs(
    plan: ApplyPlan,
    demoted_fields: set[tuple[str, str]],
    demoted_edges: set[tuple[str, str, str]],
) -> tuple[PlanVerb, ...]:
    buckets: dict[str, list[PlanVerb]] = {
        kind: []
        for kind in (
            "created",
            "reused",
            "updated",
            "edge-added",
            "edge-dropped",
            "loose",
            "drift",
            "warn",
        )
    }
    planned_by_slug = {planned.node.slug: planned for planned in plan.nodes}
    for verb in plan.verbs:
        if verb.kind == "updated" and (verb.slug, verb.field) in demoted_fields:
            buckets["drift"].append(
                PlanVerb("drift", verb.slug, verb.handle, verb.field)
            )
        elif (verb.kind, verb.slug, verb.target) in demoted_edges:
            handle = planned_by_slug[verb.slug].handle
            buckets["drift"].append(PlanVerb("drift", verb.slug, handle, "after"))
        else:
            buckets[verb.kind].append(verb)

    node_order = {planned.node.slug: planned.node.idx for planned in plan.nodes}
    drift_field_order = {
        field: index
        for index, field in enumerate(("title", *_FIELD_ORDER, "flow", "after"))
    }
    deduped_drift = {
        (verb.slug, verb.field): verb for verb in buckets["drift"]
    }.values()
    buckets["drift"] = sorted(
        deduped_drift,
        key=lambda verb: (node_order[verb.slug], drift_field_order[verb.field]),
    )
    return tuple(
        verb
        for kind in (
            "created",
            "reused",
            "updated",
            "edge-added",
            "edge-dropped",
            "loose",
            "drift",
            "warn",
        )
        for verb in buckets[kind]
    )


def execute_plan(plan: ApplyPlan) -> str:
    """Land a validated plan, rechecking settled ownership before each write."""
    rows_by_slug = _create_plan_rows(plan)
    demoted_fields = _execute_field_updates(plan, rows_by_slug)
    demoted_edges = _execute_edge_updates(plan, rows_by_slug)
    verbs = _execution_verbs(plan, demoted_fields, demoted_edges)
    return "\n".join([f"root {plan.root_handle}", *(verb.render() for verb in verbs)])


def apply_document(
    document: Doc,
    *,
    project: str,
    origin: str,
    dry_run: bool = False,
) -> str:
    """Plan and apply a parsed task document to its board family."""
    plan = plan_document(document, project=project, origin=origin)
    if dry_run:
        return plan.report()
    return execute_plan(plan)


def ingest_path(
    path: str | PathLike[str],
    *,
    project: str | None,
    origin: str | None = None,
    dry_run: bool = False,
    infer_ordered_dependencies: bool = False,
) -> str:
    """Read, parse, and apply one task document."""
    actor = tw.canonical_actor(tw.current_actor())
    resolved_project, resolved_origin = resolve_ingest_target(
        actor, project=project, origin=origin
    )
    document = parse(
        read_document(str(path)),
        infer_ordered_dependencies=infer_ordered_dependencies,
    )
    return apply_document(
        document,
        project=resolved_project,
        origin=resolved_origin,
        dry_run=dry_run,
    )
