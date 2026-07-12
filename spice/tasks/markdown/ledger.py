"""Task-document family export and normal-form rendering."""

from __future__ import annotations

import re

from spice.errors import SpiceError
from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import Doc, Node, graph_signature

_MAX_ATX_LEVEL = 6
_HEADING_CONTENT_COL = 0
_ITEM_INDENT_STEP = 2
_LIST_ITEM_RE = re.compile(r"^ *- ")


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
    """Normal-form writer for one parsed task document.

    Heading-kind containers spell as ATX headings (a bold span past level
    six); list items spell as dash bullets indented one step per containment
    level, so nesting survives to any depth the parser accepts rather than
    running out of heading levels. Node content sits at the owning column --
    fields in canonical order, then description, then annotation blocks --
    and a blank-run collapse keeps structural spacing in normal form while
    leaving annotation interiors (a fenced blank is content) verbatim.
    """

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
        self.rows: list[tuple[str, bool]] = []

    def render(self) -> str:
        for block in self.root.annotations:
            if block.startswith("---"):
                for line in block.split("\n"):
                    self._emit(line, verbatim=True)
        # Pre-order walk with an explicit stack so arbitrarily deep containment
        # never overflows the Python stack. Each frame is (idx, heading_level,
        # item_indent): a heading deepens its children one level, an item
        # indents them one step.
        stack: list[tuple[int, int, int]] = []
        if self.synthetic:
            self._emit("")
            self._emit_content(self.root, _HEADING_CONTENT_COL)
            stack.extend(
                (idx, 1, 0) for idx in reversed(self._ordered_children(self.root))
            )
        else:
            stack.append((self.root.idx, 1, 0))
        while stack:
            idx, heading_level, item_indent = stack.pop()
            node = self.doc.nodes[idx]
            self._emit_node(node, heading_level, item_indent)
            if node.kind == "item":
                child_level, child_indent = (
                    heading_level,
                    item_indent + _ITEM_INDENT_STEP,
                )
            else:
                child_level, child_indent = heading_level + 1, 0
            stack.extend(
                (child, child_level, child_indent)
                for child in reversed(self._ordered_children(node))
            )
        return self._collapse()

    def _emit(self, text: str, verbatim: bool = False) -> None:
        self.rows.append((text, verbatim))

    def _emit_node(self, node: Node, heading_level: int, item_indent: int) -> None:
        if node.kind == "item":
            # A bare bullet right after another bullet stays tight, the way a
            # human writes a plain list; anything else opens a fresh block.
            if not (self._is_bare(node) and self._after_list_item()):
                self._emit("")
            self._emit(f"{' ' * item_indent}- {node.title}")
            self._emit_content(node, item_indent + _ITEM_INDENT_STEP)
            return
        self._emit("")
        if heading_level <= _MAX_ATX_LEVEL:
            self._emit(f"{'#' * heading_level} {node.title}")
        else:
            self._emit(f"**{node.title}**")
        self._emit("")
        self._emit_content(node, _HEADING_CONTENT_COL)

    def _emit_content(self, node: Node, col: int) -> None:
        pad = " " * col
        for item in node.acceptance:
            self._emit(f"{pad}Acceptance: {item}")
        targets = self.after.get(node.idx)
        if targets:
            self._emit(f"{pad}After: {', '.join(targets)}")
        if node.priority:
            self._emit(f"{pad}Priority: {node.priority}")
        if node.flow:
            self._emit(f"{pad}Flow: {', '.join(node.flow)}")
        if node.due:
            self._emit(f"{pad}Due: {node.due}")
        if node.tags:
            self._emit(f"{pad}Tags: {', '.join(node.tags)}")
        description = node.escaped_description_lines(col)
        if description:
            self._emit("")
            for line in description:
                self._emit(f"{pad}{line}" if line else "")
        for block in node.annotations:
            if block.startswith("---"):
                continue
            self._emit("")
            for line in block.split("\n"):
                self._emit(f"{pad}{line}" if line.strip() else "", verbatim=True)

    def _is_bare(self, node: Node) -> bool:
        return not (
            node.acceptance
            or node.description()
            or node.priority
            or node.flow
            or node.due
            or node.tags
            or self.after.get(node.idx)
            or any(not block.startswith("---") for block in node.annotations)
        )

    def _after_list_item(self) -> bool:
        return bool(self.rows) and bool(_LIST_ITEM_RE.match(self.rows[-1][0]))

    def _ordered_children(self, node: Node) -> list[int]:
        # Items (dash bullets) precede headings so a bullet never binds to a
        # heading a preceding sibling opened; sibling order carries no graph
        # meaning, so this reordering is graph-preserving. The synthetic root
        # owns the parentless nodes, which reach it by edge rather than by the
        # ``children`` list, so they are gathered by their missing parent.
        if self.synthetic and node.idx == self.root.idx:
            children = [
                other
                for other in self.doc.nodes
                if other.parent is None and other.idx != self.root.idx
            ]
        else:
            children = [self.doc.nodes[child] for child in node.children]
        items = [child.idx for child in children if child.kind == "item"]
        headings = [child.idx for child in children if child.kind != "item"]
        return items + headings

    def _collapse(self) -> str:
        out: list[str] = []
        for text, verbatim in self.rows:
            if not verbatim and not text and (not out or not out[-1]):
                continue
            out.append(text)
        while out and not out[-1]:
            out.pop()
        body = "\n".join(out)
        return body + "\n" if body else ""


def export_ledger(handle: str) -> str:
    """Export the task family containing ``handle`` in ledger normal form."""
    return export_document(_load_family(handle))


def _load_family(handle: str) -> Doc:
    raise SpiceError("task-family loading is not implemented")
