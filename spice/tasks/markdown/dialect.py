"""Task-document models and storage normal forms shared by parse and ledger."""

from __future__ import annotations

import re
from dataclasses import dataclass, field

LINK_RESIDUE_CHARS = 12
CODE_INDENT_COLS = 4

# The natural slug of a title joins its non-empty ASCII words with single
# hyphens, so two hyphens can never appear inside one. Reserving ``--`` as the
# qualifier separator keeps a qualified slug collision-free against every
# natural slug.
QUALIFIER_SEPARATOR = "--"

# When several nodes are parentless the graph grows one synthetic root that
# depends on each of them; it carries this reserved title and slug. A parsed
# node whose own title slugs to ``document-root`` therefore collides with the
# synthetic root and must qualify away from it (or, parentless, refuse).
DOCUMENT_ROOT_TITLE = "Document root"
DOCUMENT_ROOT_SLUG = "document-root"

_HEADING_RE = re.compile(r"^ {0,3}(#{1,6})\s+(.+?)\s*$")
_LIST_RE = re.compile(r"^(\s*)([-*+]|\d+[.)])(\s+)(.+?)\s*$")
_INLINE_LINK_RE = re.compile(r"\[([^\]]+)\]\(([^)]*)\)")
_HR_RE = re.compile(r"^ {0,3}((?:-\s*){3,}|(?:\*\s*){3,}|(?:_\s*){3,})$")
_SETEXT_RE = re.compile(r"^ {0,3}(=+|-{2,})\s*$")
_BOLD_SPAN_RE = re.compile(r"^([*_]{2,3})([^*_]+)\1$")
_LINKDEF_RE = re.compile(r"^\[[^\]]+\]:\s+\S")
_EMPHASIS_LABEL_RE = re.compile(r"^([*_]{1,3})([^*_:]+?)(:?)\1(:?)\s*(.*)$")
_SLUG_WORD_RE = re.compile(r"[a-z0-9]+")
_ESCAPABLE = set("-*+#>|`=~_[<")

FIELD_LABELS = {
    "acceptance": "acceptance",
    "acceptance criteria": "acceptance",
    "success criteria": "acceptance",
    "done when": "acceptance",
    "definition of done": "acceptance",
    "ac": "acceptance",
    "after": "after",
    "depends on": "after",
    "blocked by": "after",
    "prerequisites": "after",
    "priority": "priority",
    "flow": "flow",
    "due": "due",
    "deadline": "due",
    "tags": "tags",
    "labels": "tags",
}


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
        stored = dedent_content(line, self.content_col)
        self.desc.append(unescape_prose(stored))

    def description(self) -> str:
        return "\n".join(collapse_blank_runs(self.desc))

    def escaped_description_lines(self) -> list[str]:
        """Description lines escaped for normal-form ledger output."""
        return [
            escape_description_line(line, self.content_col)
            for line in collapse_blank_runs(self.desc)
        ]


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


def slugify(title: str) -> str:
    """Reduce a task-document ``title`` to its identity slug.

    An inline link contributes its visible text; the link URL is dropped first so
    it can never leak into the slug. The remaining text is lowercased and its
    ASCII words are joined with single hyphens. A title with no ASCII word slugs
    to the empty string, which lets ``parse`` refuse it with the
    title-has-no-ASCII-words message.
    """
    text = _INLINE_LINK_RE.sub(r"\1", title)
    return "-".join(_SLUG_WORD_RE.findall(text.lower()))


def title_words(title: str) -> list[str]:
    """Every ASCII slug-word a title carries, links and URLs included.

    Unlike :func:`slugify`, which drops a link's URL and keeps only its visible
    text, this preserves every ASCII word in the raw title -- URL words too --
    in reading order. Duplicate qualification uses it to find the words one
    occurrence carries that its rivals do not, so titles that share a base slug
    but differ inside a link still earn distinct qualified slugs.
    """
    return _SLUG_WORD_RE.findall(title.lower())


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


def unescape_prose(text: str) -> str:
    """Drop one CommonMark escape that forces a structural line to prose."""
    stripped = text.lstrip()
    pad = text[: len(text) - len(stripped)]
    if len(stripped) >= 2 and stripped[0] == "\\" and stripped[1] in _ESCAPABLE:
        return pad + stripped[1:]
    if re.match(r"^\d+\\[.)]", stripped) or re.match(
        r"^([A-Za-z][A-Za-z ]{0,30}?)\\:", stripped
    ):
        return pad + stripped.replace("\\", "", 1)
    return text


def escape_description_line(line: str, content_col: int) -> str:
    """Escape stored prose that would otherwise reclassify on parse."""
    stripped = line.lstrip()
    if not stripped:
        return line
    pad = line[: len(line) - len(stripped)]
    if len(pad) >= CODE_INDENT_COLS:
        return line
    ordered = re.match(r"^\d+(?=[.)]\s)", stripped)
    if ordered:
        return f"{pad}{ordered.group(0)}\\{stripped[ordered.end() :]}"
    emphasized = _EMPHASIS_LABEL_RE.match(stripped)
    structural = (
        stripped[:3] in ("```", "~~~")
        or stripped[0] in ">|"
        or stripped.startswith(("---", "<!--"))
        or _HR_RE.match(stripped)
        or _SETEXT_RE.match(stripped)
        or _LINKDEF_RE.match(stripped)
        or _LIST_RE.match(stripped)
        or _BOLD_SPAN_RE.match(stripped)
        or (content_col <= 3 and _HEADING_RE.match(stripped))
        or (
            emphasized
            and (emphasized.group(3) or emphasized.group(4))
            and emphasized.group(2).strip().lower() in FIELD_LABELS
        )
        or (
            _INLINE_LINK_RE.search(stripped)
            and len(_link_residue(stripped)) <= LINK_RESIDUE_CHARS
        )
    )
    if structural:
        return pad + "\\" + stripped
    label = re.match(r"^([A-Za-z][A-Za-z ]{0,30}?)\s*:", stripped)
    if label and label.group(1).strip().lower() in FIELD_LABELS:
        index = stripped.index(":")
        return pad + stripped[:index] + "\\" + stripped[index:]
    return line


def _link_residue(stripped: str) -> str:
    residue = _INLINE_LINK_RE.sub("", stripped)
    return re.sub(r"[\s\W]+", " ", residue).strip()
