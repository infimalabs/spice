"""Task-document family export and normal-form rendering."""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from spice.errors import SpiceError
from spice.tasks import claimstate, config, identity
from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import (
    DOCUMENT_ROOT_SLUG,
    DOCUMENT_ROOT_TITLE,
    Doc,
    Node,
    graph_signature,
)

_MAX_ATX_LEVEL = 6
_ROLLUP_CONTENT_COL = 0
_LEAF_CONTENT_COL = 2
_PRIORITY_NAMES = {"H": "high", "M": "medium", "L": "low"}
_RUNTIME_ANNOTATION_PREFIXES = (
    "ack ",
    "claim stolen:",
    "suspect wording:",
    "validation:",
    "review:",
    "review follow-up depends on ",
    "depends:",
    "forced delete of live claim:",
    "deleted:",
    "wording review resolved:",
)


def export_document(document: Doc) -> str:
    """Render a task-document graph in ledger normal form.

    The graph is written in its single canonical spelling: rollups become ATX
    headings (bold-span sections past level six), leaves become dash bullets,
    containment becomes nesting, and every non-tree edge becomes an ``After:``
    line. Node content sits at the node's content column -- fields first, then
    description paragraphs, then annotation blocks -- with blank runs collapsed
    and prose that would re-classify escaped on the way out. A synthetic root
    is never written back; its merged preamble content renders before the
    parentless leaves it depends on, and those dependency edges re-derive from
    the shape on re-read.

    The result obeys the two ledger laws: parsing it reproduces the same graph
    (round trip), and re-exporting the re-parsed document is byte-identical
    (fixed point). The round trip is asserted here -- the ledger refuses to
    emit a document it cannot read back to the same graph.
    """
    if not document.nodes or document.root < 0:
        return ""
    text = _Renderer(document).render()
    if graph_signature(parse(text)) != graph_signature(document):
        raise SpiceError("ledger export does not round-trip to the same graph")
    return text


class _Renderer:
    """Normal-form writer for one parsed task document."""

    def __init__(self, document: Doc) -> None:
        self.doc = document
        self.root = document.nodes[document.root]
        self.synthetic = self.root.kind == "document"
        # ``After:`` targets per source node, as sorted slugs. Containment is
        # nesting, so tree edges never appear; the synthetic root's edges onto
        # its parentless leaves re-derive from structure, so they are dropped
        # while any preamble ``After:`` onto a nested node still exports.
        auto = {
            node.idx
            for node in document.nodes
            if node.parent is None and node.idx != self.root.idx
        }
        self.after: dict[int, list[str]] = {}
        for source, target, kind in document.edges:
            if kind == "containment":
                continue
            if self.synthetic and source == self.root.idx and target in auto:
                continue
            self.after.setdefault(source, []).append(document.nodes[target].slug)
        for targets in self.after.values():
            targets.sort()

    def render(self) -> str:
        frontmatter = [
            block for block in self.root.annotations if block.startswith("---")
        ]
        blocks: list[tuple[list[str], bool]] = []
        if self.synthetic:
            preamble = self._content_lines(self.root, _ROLLUP_CONTENT_COL)
            if preamble:
                blocks.append((preamble, False))
            blocks.extend(
                self._render_node(node, 1) for node in self._order(self._top_level())
            )
        else:
            blocks.append(self._render_node(self.root, 1))
        lines: list[str] = []
        for block in frontmatter:
            lines.extend(block.split("\n"))
        body = self._join(blocks)
        if lines and body:
            lines.append("")
        lines.extend(body)
        text = "\n".join(lines).strip("\n")
        return text + "\n" if text else ""

    def _top_level(self) -> list[Node]:
        return [
            node
            for node in self.doc.nodes
            if node.parent is None and node.idx != self.root.idx
        ]

    def _order(self, nodes: list[Node]) -> list[Node]:
        # Leaves precede rollups so a column-0 bullet never binds to a heading
        # that a preceding sibling opened; sibling order carries no graph
        # meaning, so this reordering is graph-preserving.
        leaves = [node for node in nodes if not node.children]
        rollups = [node for node in nodes if node.children]
        return leaves + rollups

    def _render_node(self, node: Node, depth: int) -> tuple[list[str], bool]:
        if node.children:
            lines = [self._heading(node.title, depth)]
            for group in (
                self._content_lines(node, _ROLLUP_CONTENT_COL),
                self._join(
                    self._render_node(self.doc.nodes[child], depth + 1)
                    for child in self._ordered_children(node)
                ),
            ):
                if group:
                    lines.append("")
                    lines.extend(group)
            return lines, False
        content = self._content_lines(node, _LEAF_CONTENT_COL)
        bullet = f"- {node.title}"
        if not content:
            return [bullet], True
        return [bullet, *content], False

    def _ordered_children(self, node: Node) -> list[int]:
        children = [self.doc.nodes[child] for child in node.children]
        return [child.idx for child in self._order(children)]

    def _heading(self, title: str, depth: int) -> str:
        if depth <= _MAX_ATX_LEVEL:
            return f"{'#' * depth} {title}"
        return f"**{title}**"

    def _content_lines(self, node: Node, col: int) -> list[str]:
        pad = " " * col
        fields: list[str] = [f"{pad}Acceptance: {item}" for item in node.acceptance]
        targets = self.after.get(node.idx)
        if targets:
            fields.append(f"{pad}After: {', '.join(targets)}")
        if node.priority:
            fields.append(f"{pad}Priority: {node.priority}")
        if node.flow:
            fields.append(f"{pad}Flow: {', '.join(node.flow)}")
        if node.due:
            fields.append(f"{pad}Due: {node.due}")
        if node.tags:
            fields.append(f"{pad}Tags: {', '.join(node.tags)}")
        description = [
            f"{pad}{line}" if line else ""
            for line in node.escaped_description_lines(col)
        ]
        annotations = [
            self._annotation(block, col)
            for block in node.annotations
            if not block.startswith("---")
        ]
        return self._stack([fields, description, *annotations])

    def _annotation(self, block: str, col: int) -> list[str]:
        pad = " " * col
        return [f"{pad}{line}" if line.strip() else "" for line in block.split("\n")]

    def _stack(self, groups: list[list[str]]) -> list[str]:
        out: list[str] = []
        for group in groups:
            if not group:
                continue
            if out:
                out.append("")
            out.extend(group)
        return out

    def _join(self, blocks: Iterable[tuple[list[str], bool]]) -> list[str]:
        out: list[str] = []
        prev_tight = False
        for lines, tight in blocks:
            if out and not (prev_tight and tight):
                out.append("")
            out.extend(lines)
            prev_tight = tight
        return out


def export_ledger(handle: str) -> str:
    """Export the task family containing ``handle`` in ledger normal form."""
    return export_document(_load_family(handle))


def _load_family(handle: str) -> Doc:
    from spice.tasks.markdown.apply import load_family_rows

    target = identity.resolve(handle)
    target_slug = str(target.get(config.TASKDOC_ID_UDA) or "")
    if not target_slug:
        raise SpiceError(f"{identity.render_handle(target)} is not in a task document")
    project = str(target.get("project") or "")
    origin = str(target.get("origin") or "")
    rows = load_family_rows(project, origin)
    if all(str(row.get("uuid") or "") != str(target.get("uuid") or "") for row in rows):
        raise SpiceError(f"{identity.render_handle(target)} is not in a visible family")
    return _document_from_rows(rows)


def _document_from_rows(rows: list[dict[str, Any]]) -> Doc:
    rows_by_slug: dict[str, dict[str, Any]] = {}
    for row in rows:
        slug = str(row.get(config.TASKDOC_ID_UDA) or "")
        if slug in rows_by_slug:
            handles = sorted(
                identity.render_handle(candidate)
                for candidate in (rows_by_slug[slug], row)
            )
            raise SpiceError(f"{slug} is ambiguous in family: {', '.join(handles)}")
        rows_by_slug[slug] = row

    nodes = [_node_from_row(index, row) for index, row in enumerate(rows)]
    index_by_slug = {node.slug: node.idx for node in nodes}
    for node, row in zip(nodes, rows, strict=True):
        parent_slug = str(row.get(config.TASKDOC_PARENT_UDA) or "")
        if not parent_slug:
            continue
        parent_idx = index_by_slug.get(parent_slug)
        if parent_idx is None:
            raise SpiceError(f"{node.slug} has unknown taskdoc_parent: {parent_slug}")
        if parent_idx == node.idx:
            raise SpiceError(f"{node.slug} cannot be its own taskdoc_parent")
        node.parent = parent_idx
        nodes[parent_idx].children.append(node.idx)
    edges = _family_edges(nodes, rows)
    _mark_node_kinds(nodes, edges)
    root = _family_root(nodes, edges)
    return Doc(nodes=nodes, root=root, edges=edges, refusals=[], warnings=[])


def _node_from_row(index: int, row: dict[str, Any]) -> Node:
    slug = str(row.get(config.TASKDOC_ID_UDA) or "")
    title = str(row.get("description") or "")
    node = Node(
        idx=index,
        kind="item",
        title=title,
        line=index + 1,
        acceptance=_acceptance(row),
        annotations=_content_annotations(row),
        priority=_PRIORITY_NAMES.get(str(row.get("priority") or "")),
        flow=claimstate.phases_of(row),
        due=str(row.get("due") or "") or None,
        tags=sorted(str(tag) for tag in (row.get("tags") or ())),
        slug=slug,
    )
    description = str(row.get("task_description") or "")
    node.desc.extend(description.splitlines())
    return node


def _acceptance(row: dict[str, Any]) -> list[str]:
    raw = str(row.get("acceptance") or "")
    return raw.split(" | ") if raw else []


def _content_annotations(row: dict[str, Any]) -> list[str]:
    annotations: list[str] = []
    for annotation in row.get("annotations") or ():
        if not isinstance(annotation, dict):
            continue
        text = str(annotation.get("description") or "")
        if text.startswith(_RUNTIME_ANNOTATION_PREFIXES):
            continue
        annotations.append(text)
    return annotations


def _family_edges(
    nodes: list[Node], rows: list[dict[str, Any]]
) -> list[tuple[int, int, str]]:
    index_by_uuid = {
        str(row.get("uuid") or ""): node.idx
        for node, row in zip(nodes, rows, strict=True)
        if row.get("uuid")
    }
    edges: list[tuple[int, int, str]] = []
    seen: set[tuple[int, int]] = set()
    for node in nodes:
        if node.parent is not None:
            _append_edge(edges, seen, node.parent, node.idx, "containment")
    for node, row in zip(nodes, rows, strict=True):
        for target_uuid in _depends(row):
            target_idx = index_by_uuid.get(target_uuid)
            if target_idx is not None:
                _append_edge(edges, seen, node.idx, target_idx, "after")
    return edges


def _mark_node_kinds(nodes: list[Node], edges: list[tuple[int, int, str]]) -> None:
    parentless = {node.idx for node in nodes if node.parent is None}
    for node in nodes:
        synthetic_target = any(
            source == node.idx and target in parentless and target != node.idx
            for source, target, _kind in edges
        )
        if (
            node.slug == DOCUMENT_ROOT_SLUG
            and node.title == DOCUMENT_ROOT_TITLE
            and synthetic_target
        ):
            node.kind = "document"
        else:
            node.kind = "heading" if node.children else "item"


def _depends(row: dict[str, Any]) -> tuple[str, ...]:
    raw = row.get("depends") or ()
    if isinstance(raw, str):
        return (raw,) if raw else ()
    return tuple(str(value) for value in raw if value)


def _append_edge(
    edges: list[tuple[int, int, str]],
    seen: set[tuple[int, int]],
    source: int,
    target: int,
    kind: str,
) -> None:
    if (source, target) in seen:
        return
    seen.add((source, target))
    edges.append((source, target, kind))


def _family_root(nodes: list[Node], edges: list[tuple[int, int, str]]) -> int:
    synthetic = [node.idx for node in nodes if node.kind == "document"]
    if len(synthetic) > 1:
        raise SpiceError("task-document family has multiple synthetic roots")
    parentless = [node.idx for node in nodes if node.parent is None]
    if synthetic:
        root = synthetic[0]
    elif len(parentless) == 1:
        return parentless[0]
    elif parentless:
        root = len(nodes)
        nodes.append(
            Node(
                idx=root,
                kind="document",
                title=DOCUMENT_ROOT_TITLE,
                line=0,
                slug=DOCUMENT_ROOT_SLUG,
            )
        )
    else:
        raise SpiceError("task-document family has no root")

    seen = {(source, target) for source, target, _kind in edges}
    for target in parentless:
        if target != root:
            _append_edge(edges, seen, root, target, "after")
    return root
