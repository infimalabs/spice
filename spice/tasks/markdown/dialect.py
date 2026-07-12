"""Task-document models and storage normal forms shared by parse and ledger."""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class Node:
    idx: int
    kind: str
    title: str
    line: int
    level: int = 0
    parent: int | None = None
    children: list[int] = field(default_factory=list)
    desc: list[str] = field(default_factory=list)
    acceptance: list[str] = field(default_factory=list)
    annotations: list[str] = field(default_factory=list)
    after_raw: list[tuple[str, int]] = field(default_factory=list)
    priority: str | None = None
    flow: list[str] | None = None
    due: str | None = None
    tags: list[str] = field(default_factory=list)
    checked: bool | None = None
    content_col: int = 0
    slug: str = ""

    def store_description_line(self, line: str) -> None:
        """Store one line relative to this node's content column."""
        self.desc.append(dedent_content(line, self.content_col))

    def description(self) -> str:
        return "\n".join(collapse_blank_runs(self.desc))


@dataclass
class Doc:
    nodes: list[Node]
    root: int
    edges: list[tuple[int, int, str]]
    refusals: list[str]
    warnings: list[tuple[int, str, str]]


NodeSignature = tuple[
    str,
    str | None,
    str | None,
    tuple[str, ...],
    str | None,
    tuple[str, ...],
    tuple[str, ...],
    str,
    tuple[str, ...],
]
GraphSignature = tuple[
    tuple[tuple[str, NodeSignature], ...],
    frozenset[tuple[str, str]],
]


def graph_signature(document: Doc) -> GraphSignature:
    """Comparable graph value preserved by parse and ledger round trips."""
    nodes: list[tuple[str, NodeSignature]] = []
    for node in document.nodes:
        parent = document.nodes[node.parent].slug if node.parent is not None else None
        nodes.append(
            (
                node.slug,
                (
                    node.title,
                    parent,
                    node.priority,
                    tuple(node.flow or ()),
                    node.due,
                    tuple(node.tags),
                    tuple(node.acceptance),
                    node.description(),
                    tuple(annotation.rstrip() for annotation in node.annotations),
                ),
            )
        )
    cross_edges = frozenset(
        (document.nodes[source].slug, document.nodes[target].slug)
        for source, target, kind in document.edges
        if kind != "containment"
    )
    return tuple(sorted(nodes)), cross_edges


def indent_width(line: str) -> int:
    """Leading indentation width after four-column tab expansion."""
    prefix = line[: len(line) - len(line.lstrip())]
    return len(prefix.replace("\t", "    "))


def dedent_content(line: str, content_col: int) -> str:
    """Store a content line relative to its owning node's content column."""
    expanded = line.replace("\t", "    ").rstrip()
    indent = indent_width(expanded)
    return expanded[min(indent, content_col) :]


def collapse_blank_runs(lines: list[str]) -> list[str]:
    """Return lines with one internal blank and no edge blank lines."""
    collapsed: list[str] = []
    for line in lines:
        if not line.strip() and (not collapsed or collapsed[-1] == ""):
            continue
        collapsed.append(line.rstrip())
    while collapsed and collapsed[-1] == "":
        collapsed.pop()
    return collapsed
