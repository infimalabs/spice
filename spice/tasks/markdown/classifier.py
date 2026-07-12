"""Single-pass task-document line classification and attachment."""

from __future__ import annotations

import re

from spice.tasks.config import APPROVED_PHASES
from spice.tasks.markdown.dialect import (
    CODE_INDENT_COLS,
    DOCUMENT_ROOT_SLUG,
    DOCUMENT_ROOT_TITLE,
    FIELD_LABELS,
    LONG_TITLE_CHARS,
    QUALIFIER_SEPARATOR,
    Doc,
    Node,
    collapse_blank_runs,
    dedent_content,
    indent_width,
    slugify,
    title_carries_url,
    title_words,
    unescape_prose,
)

_HEADING_RE = re.compile(r"^ {0,3}(#{1,6})\s+(.+?)\s*$")
_LIST_RE = re.compile(r"^(\s*)([-*+]|\d+[.)])(\s+)(.+?)\s*$")
_HR_RE = re.compile(r"^ {0,3}((?:-\s*){3,}|(?:\*\s*){3,}|(?:_\s*){3,})$")
_SETEXT_RE = re.compile(r"^ {0,3}(=+|-{2,})\s*$")
_FENCE_RE = re.compile(r"^(`{3,}|~{3,})(.*)$")
_LINKDEF_RE = re.compile(r"^\[[^\]]+\]:\s+\S")
_INLINE_LINK_RE = re.compile(r"\[([^\]]+)\]\(([^)]*)\)")
_BOLD_SPAN_RE = re.compile(r"^([*_]{2,3})([^*_]+)\1$")
_CHECKBOX_RE = re.compile(r"^\[([ xX])\]\s+(.+)$")
_PLAIN_FIELD_RE = re.compile(r"^([A-Za-z][A-Za-z ]{0,30}?)\s*:\s*(.*)$")
_EMPHASIS_FIELD_RE = re.compile(r"^([*_]{1,3})([^*_:]+?)(:?)\1(:?)\s*(.*)$")
_ESCAPED_PROSE_RE = re.compile(
    r"^(?:\\[-*+#>|`=~_\[<]|\d+\\[.)]|[A-Za-z][A-Za-z ]{0,30}?\\:)"
)
_LINK_RESIDUE_CHARS = 12
_PRIORITIES = frozenset(("high", "medium", "low", "none"))
_SLUG_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*(?:--[a-z0-9]+(?:-[a-z0-9]+)*)?$")

_FIELD_SECTIONS = {
    "acceptance": "acceptance",
    "acceptance-criteria": "acceptance",
    "ac": "acceptance",
    "done-when": "acceptance",
    "definition-of-done": "acceptance",
    "success-criteria": "acceptance",
    "dependencies": "after",
    "depends-on": "after",
    "prerequisites": "after",
    "blocked-by": "after",
    "notes": "notes",
    "context": "notes",
    "background": "notes",
    "references": "notes",
    "links": "notes",
    "open-questions": "notes",
    "risks": "notes",
}


class Parser:
    """Top-to-bottom first-match classifier with attachment state."""

    def __init__(self) -> None:
        self.nodes: list[Node] = []
        self.preamble = Node(idx=-1, kind="preamble", title="", line=0)
        self.current: Node | None = None
        self.heading_stack: list[tuple[int, Node]] = []
        self.real_heading_stack: list[tuple[int, Node]] = []
        self.list_stack: list[tuple[int, int, Node, bool]] = []
        self.ordered_runs: dict[tuple[int, int], Node] = {}
        self.sequence_edges: list[tuple[int, int, str]] = []
        self.field_section: tuple[str, Node] | None = None
        self.note_section_block: tuple[int, int] | None = None
        self.acceptance_capture: tuple[Node, int] | None = None
        self.acceptance_count = 0
        self.seen_scalar_fields: set[tuple[int, str]] = set()
        self.warnings: list[tuple[int, str, str]] = []
        self.refusals: list[str] = []
        self.fence: list[str] | None = None
        self.fence_open = ("", 0)
        self.fence_node: Node | None = None
        self.in_comment = False
        self.comment_line = 0
        self.in_frontmatter = False
        self.frontmatter_lines: list[tuple[str, int]] = []
        self.last_desc: tuple[Node, str] | None = None
        self.last_attach: Node | None = None
        self.annotation_run: tuple[Node, str, int] | None = None
        self.prev_blank = True

    def warn(self, line: int, code: str, message: str) -> None:
        self.warnings.append((line, code, message))

    def warn_title(self, node: Node) -> None:
        if title_carries_url(node.title):
            self.warn(
                node.line,
                "url-title",
                "title contains a URL; slug omits URL text",
            )
        if len(node.title) > LONG_TITLE_CHARS:
            self.warn(
                node.line,
                "long-title",
                f"title is {len(node.title)} characters; consider decomposing it",
            )

    def target_node(self) -> Node:
        return self.current if self.current is not None else self.preamble

    def mint(
        self,
        kind: str,
        title: str,
        line: int,
        level: int,
        parent: Node | None,
        *,
        content_col: int = 0,
    ) -> Node:
        node = Node(
            idx=len(self.nodes),
            kind=kind,
            title=title,
            line=line,
            level=level,
            content_col=content_col,
        )
        if parent is not None:
            node.parent = parent.idx
            parent.children.append(node.idx)
        self.nodes.append(node)
        self.warn_title(node)
        self.current = node
        self.last_attach = node
        return node

    def feed(self, text: str) -> None:
        lines = text.splitlines()
        last_line = 0
        for index, line in enumerate(lines):
            last_line = index + 1
            self.handle_line(line, index)
        if self.in_frontmatter:
            self.abort_frontmatter(
                "unclosed-frontmatter",
                "frontmatter never closed; its lines replay as content",
                last_line,
            )
        if self.in_comment:
            self.warn(
                self.comment_line, "unclosed-comment", "HTML comment never closed"
            )
        if self.fence is not None:
            self.warn(last_line, "unclosed-fence", "code fence never closed")
            char, width = self.fence_open
            self.fence.append(char * width)
            self.close_fence()
        self.close_acceptance_capture()
        self.last_desc = None

    def handle_line(self, line: str, index: int) -> None:
        stripped = line.strip()
        line_number = index + 1
        annotation_continues = False
        try:
            if self.acceptance_capture is not None and stripped:
                item = _LIST_RE.match(line)
                is_criterion = (
                    item is not None
                    and indent_width(item.group(1)) < self.code_threshold()
                    and not _HR_RE.match(stripped)
                )
                if not is_criterion:
                    self.close_acceptance_capture()
            if self._span_interior(line, stripped, index):
                return
            if self._span_opener(line, stripped, index):
                return
            if self._blank(stripped):
                return
            if self._setext(line, stripped, line_number):
                return
            self.last_desc = None
            if self._thematic_break(line):
                return
            if self._escaped_prose(line, stripped, line_number):
                return
            if self._heading(line, line_number):
                return
            if self._list_item(line, line_number):
                return
            if self._field_line(line, stripped, line_number):
                return
            if self._bold_heading(line, stripped, line_number):
                return
            if self._annotation(line, stripped):
                annotation_continues = True
                return
            fieldish = _fieldish_label(stripped)
            if fieldish is not None:
                self.warn(
                    line_number,
                    "fieldish-prose",
                    f"{fieldish} is not a task-document field; kept as prose",
                )
            self.attach_description(line)
            self.prev_blank = False
        finally:
            if not annotation_continues:
                self.annotation_run = None

    def close_fence(self) -> None:
        node = self.fence_node or self.target_node()
        lines = self.fence or []
        base = indent_width(lines[0]) if lines else 0
        block = "\n".join(dedent_content(line, base) for line in lines)
        node.annotations.append(block)
        self.fence = None
        self.fence_node = None
        self.last_attach = node

    def abort_frontmatter(self, code: str, message: str, line_number: int) -> None:
        self.in_frontmatter = False
        self.warn(line_number, code, message)
        buffered = self.frontmatter_lines
        self.frontmatter_lines = []
        for line, index in buffered:
            self.handle_line(line, index)

    def _span_interior(self, line: str, stripped: str, index: int) -> bool:
        if self.fence is not None:
            self.fence.append(line)
            closer = _FENCE_RE.match(stripped)
            char, width = self.fence_open
            if (
                closer
                and closer.group(1)[0] == char
                and len(closer.group(1)) >= width
                and not closer.group(2).strip()
            ):
                self.close_fence()
            return True
        if self.in_frontmatter:
            if stripped in ("---", "..."):
                self.in_frontmatter = False
                body = [text for text, _index in self.frontmatter_lines]
                self.preamble.annotations.append("\n".join(["---", *body, "---"]))
                self.frontmatter_lines = []
            elif not stripped or _HEADING_RE.match(line):
                self.abort_frontmatter(
                    "frontmatter-abort",
                    "leading '---' is not frontmatter (blank or heading inside); "
                    "its lines replay as content",
                    index + 1,
                )
                self.handle_line(line, index)
            else:
                self.frontmatter_lines.append((line, index))
            return True
        if self.in_comment:
            if "-->" in stripped:
                self.in_comment = False
            return True
        return False

    def _span_opener(self, line: str, stripped: str, index: int) -> bool:
        if self.code_indented(line):
            return False
        fence = _FENCE_RE.match(stripped)
        if fence:
            self.fence = [line]
            self.fence_open = (fence.group(1)[0], len(fence.group(1)))
            self.fence_node = self.attach_target_for(line)
            self.last_desc = None
            self.prev_blank = False
            return True
        if index == 0 and stripped == "---":
            self.in_frontmatter = True
            return True
        if stripped.startswith("<!--"):
            if "-->" not in stripped:
                self.in_comment = True
                self.comment_line = index + 1
                self.last_desc = None
                return True
            if stripped.endswith("-->"):
                self.last_desc = None
                return True
        return False

    def _blank(self, stripped: str) -> bool:
        if stripped:
            return False
        self.last_desc = None
        if self.acceptance_capture is not None:
            if self.acceptance_count:
                self.close_acceptance_capture()
            self.prev_blank = True
            return True
        self.target_node().desc.append("")
        self.prev_blank = True
        return True

    def _setext(self, line: str, stripped: str, line_number: int) -> bool:
        if self.last_desc is None or not _SETEXT_RE.match(line):
            return False
        node_holding, text_line = self.last_desc
        if not (node_holding.desc and node_holding.desc[-1] == text_line):
            return False
        node_holding.desc.pop()
        level = 1 if stripped.startswith("=") else 2
        self.handle_heading(level, text_line.strip(), line_number)
        self.last_desc = None
        self.prev_blank = False
        return True

    def _thematic_break(self, line: str) -> bool:
        if self.code_indented(line) or not _HR_RE.match(line.strip()):
            return False
        self.prev_blank = False
        return True

    def _escaped_prose(self, line: str, stripped: str, line_number: int) -> bool:
        if self.code_indented(line) or not _ESCAPED_PROSE_RE.match(stripped):
            return False
        self.attach_description(line)
        self.prev_blank = False
        return True

    def _heading(self, line: str, line_number: int) -> bool:
        if self.code_indented(line):
            return False
        heading = _HEADING_RE.match(line)
        if not heading:
            return False
        level = len(heading.group(1))
        title = re.sub(r"\s+#+\s*$", "", heading.group(2)).strip()
        self.handle_heading(level, title, line_number)
        self.prev_blank = False
        return True

    def _field_line(self, line: str, stripped: str, line_number: int) -> bool:
        if self.code_indented(line):
            return False
        field = _field_parts(stripped)
        if field is None:
            return False
        target = self.attach_target_for(line)
        if self.store_field(target, field, line_number):
            self.last_desc = None
        else:
            self.store_description(
                target,
                unescape_prose(dedent_content(line, target.content_col)),
            )
        self.prev_blank = False
        return True

    def _bold_heading(self, line: str, stripped: str, line_number: int) -> bool:
        if self.code_indented(line) or not self.prev_blank:
            return False
        bold = _BOLD_SPAN_RE.match(stripped)
        if bold is None:
            return False
        base_level = self.real_heading_stack[-1][0] if self.real_heading_stack else 0
        level = base_level + 1
        self.handle_heading(level, bold.group(2).strip(), line_number, real=False)
        self.warn(
            line_number,
            "bold-heading",
            f"sole bold span promoted to level {level} section",
        )
        self.prev_blank = False
        return True

    def _list_item(self, line: str, line_number: int) -> bool:
        item = _LIST_RE.match(line)
        if not item:
            return False
        if indent_width(item.group(1)) >= self.code_threshold():
            self.warn(line_number, "indent-code", "indented-code line kept as content")
            self.attach_description(line)
            self.prev_blank = False
            return True
        self.handle_list_item(item, line_number)
        self.prev_blank = False
        return True

    def _annotation(self, line: str, stripped: str) -> bool:
        if self.code_indented(line):
            return False
        shape = _annotation_shape(stripped)
        if shape is None:
            return False
        target = self.attach_target_for(line)
        stored = dedent_content(line, target.content_col)
        if (
            self.annotation_run is not None
            and self.annotation_run[0] is target
            and self.annotation_run[1] == shape
        ):
            annotation_index = self.annotation_run[2]
            target.annotations[annotation_index] += "\n" + stored
        else:
            target.annotations.append(stored)
            annotation_index = len(target.annotations) - 1
        self.annotation_run = (target, shape, annotation_index)
        self.last_attach = target
        self.prev_blank = False
        return True

    def handle_heading(
        self, level: int, title: str, line_number: int, *, real: bool = True
    ) -> None:
        self.field_section = None
        self.note_section_block = None
        while self.heading_stack and self.heading_stack[-1][0] >= level:
            self.heading_stack.pop()
        if real:
            while self.real_heading_stack and self.real_heading_stack[-1][0] >= level:
                self.real_heading_stack.pop()
        parent = self.heading_stack[-1][1] if self.heading_stack else None
        section_kind = _FIELD_SECTIONS.get(_simple_slug(title))
        if section_kind is not None:
            target = parent if parent is not None else self.preamble
            self.field_section = (section_kind, target)
            self.current = target
            self.last_attach = target
            self.list_stack.clear()
            self.ordered_runs.clear()
            self.warn(
                line_number,
                "field-section",
                f"{title} heading feeds {target.title or 'document root'}",
            )
            return
        node = self.mint("heading", title, line_number, level, parent)
        self.heading_stack.append((level, node))
        self.list_stack.clear()
        self.ordered_runs.clear()
        if real:
            self.real_heading_stack.append((level, node))

    def handle_list_item(self, item: re.Match[str], line_number: int) -> None:
        indent = indent_width(item.group(1))
        marker = item.group(2)
        content_col = indent + len(marker) + indent_width(item.group(3))
        while self.list_stack and self.list_stack[-1][0] >= indent:
            self.list_stack.pop()
        parent = (
            self.list_stack[-1][2]
            if self.list_stack
            else (self.heading_stack[-1][1] if self.heading_stack else None)
        )
        target = parent if parent is not None else self.preamble
        title, checked = self.strip_checkbox(item.group(4), line_number)
        if self.acceptance_capture is not None:
            target, _start_line = self.acceptance_capture
            target.acceptance.append(title)
            self.acceptance_count += 1
            self.current = target
            self.last_attach = target
            return
        field = _field_parts(title)
        if field is not None:
            if not self.store_field(target, field, line_number):
                self.store_description(target, title)
            self.current = target
            self.last_attach = target
            return
        if self.field_section is not None:
            self.store_section_item(self.field_section, title, line_number)
            return

        ordered = marker[0].isdigit()
        predecessor: Node | None = None
        run_key = (target.idx, indent)
        if ordered:
            number = int(marker[:-1])
            predecessor = None if number == 1 else self.ordered_runs.get(run_key)
            if number != 1 and predecessor is None:
                self.warn(
                    line_number,
                    "ordered-start",
                    "numbered line did not start at 1 or continue an ordered run; kept as prose",
                )
                self.attach_description(_list_prose(item, title))
                return
        else:
            self.ordered_runs.pop(run_key, None)
        node = self.mint(
            "item",
            title,
            line_number,
            indent,
            parent,
            content_col=content_col,
        )
        node.checked = checked
        self.list_stack.append((indent, content_col, node, ordered))
        if ordered:
            if predecessor is not None:
                self.sequence_edges.append((node.idx, predecessor.idx, "after"))
            self.ordered_runs[run_key] = node

    def strip_checkbox(self, title: str, line_number: int) -> tuple[str, bool | None]:
        checkbox = _CHECKBOX_RE.match(title)
        if checkbox is None:
            return title, None
        checked = checkbox.group(1).lower() == "x"
        if checked:
            self.warn(
                line_number,
                "checked-discarded",
                "checked marker stripped; the board owns status",
            )
        return checkbox.group(2), checked

    def store_field(
        self, target: Node, field: tuple[str, str], line_number: int
    ) -> bool:
        name, value = field
        if name in ("priority", "flow", "due"):
            key = (target.idx, name)
            if key in self.seen_scalar_fields:
                self.warn(
                    line_number,
                    "field-repeat",
                    f"{name.title()} repeated; last value won",
                )
            self.seen_scalar_fields.add(key)
        if name == "acceptance":
            if value:
                target.acceptance.append(value)
            else:
                self.acceptance_capture = (target, line_number)
                self.acceptance_count = 0
        elif name == "after":
            targets = _after_targets(value)
            if targets is None:
                self.warn(
                    line_number,
                    "after-prose",
                    "After-shaped line kept as prose because targets were not slug-shaped",
                )
                return False
            target.after_raw.extend((slug, line_number) for slug in targets)
        elif name == "priority":
            normalized = value.lower()
            if normalized not in _PRIORITIES:
                self.refusals.append(f"invalid priority: {value}")
            target.priority = normalized
        elif name == "flow":
            phases = [part.strip().lower() for part in value.split(",") if part.strip()]
            invalid = [phase for phase in phases if phase not in APPROVED_PHASES]
            if not phases:
                invalid.append("")
            self.refusals.extend(f"invalid flow phase: {phase}" for phase in invalid)
            target.flow = phases
        elif name == "due":
            target.due = value
        elif name == "tags":
            target.tags.extend(part for part in re.split(r"[\s,]+", value) if part)
        self.last_attach = target
        return True

    def store_section_item(
        self, section: tuple[str, Node], title: str, line_number: int
    ) -> None:
        kind, target = section
        if kind == "acceptance":
            target.acceptance.append(title)
        elif kind == "after":
            targets = _after_targets(title)
            if targets is None:
                self.warn(
                    line_number,
                    "after-prose",
                    "dependency-section item kept as prose because it was not slug-shaped",
                )
                self.store_description(target, title)
            else:
                target.after_raw.extend((slug, line_number) for slug in targets)
        else:
            block = f"> {title}"
            if self.note_section_block == (target.idx, len(target.annotations) - 1):
                target.annotations[-1] += "\n" + block
            else:
                target.annotations.append(block)
                self.note_section_block = (target.idx, len(target.annotations) - 1)
        self.current = target
        self.last_attach = target

    def close_acceptance_capture(self) -> None:
        if self.acceptance_capture is None:
            return
        _target, line_number = self.acceptance_capture
        if not self.acceptance_count:
            self.warn(
                line_number,
                "empty-acceptance",
                "Acceptance intro captured no criteria",
            )
        self.acceptance_capture = None
        self.acceptance_count = 0

    def code_threshold(self) -> int:
        if self.list_stack:
            return self.list_stack[-1][1] + CODE_INDENT_COLS
        return CODE_INDENT_COLS

    def code_indented(self, line: str) -> bool:
        return indent_width(line) >= self.code_threshold()

    def attach_target_for(self, line: str) -> Node:
        if self.field_section is not None:
            return self.field_section[1]
        if self.current is None:
            return self.preamble
        if not self.prev_blank:
            return self.last_attach if self.last_attach is not None else self.current
        indent = indent_width(line)
        for _marker, content_col, node, _ordered in reversed(self.list_stack):
            if indent >= content_col:
                return node
        return self.heading_stack[-1][1] if self.heading_stack else self.preamble

    def attach_description(self, line: str) -> None:
        target = self.attach_target_for(line)
        text = unescape_prose(dedent_content(line, target.content_col))
        self.store_description(target, text)

    def store_description(self, target: Node, text: str) -> None:
        target.desc.append(text)
        self.last_desc = (target, target.desc[-1])
        self.last_attach = target

    def document(self) -> Doc:
        """Finalize slugs, resolve the root, build edges, and refuse cycles."""
        if not self.nodes:
            self.refusals.append(
                "task document has no nodes (content but no structure)"
                if self._preamble_has_content()
                else "task document is empty"
            )
            return Doc(
                nodes=self.nodes,
                root=-1,
                edges=[],
                refusals=self.refusals,
                warnings=self.warnings,
            )
        parentless = [node for node in self.nodes if node.parent is None]
        needs_synthetic = len(parentless) != 1
        self._assign_slugs(needs_synthetic)
        root = self._resolve_root(parentless, needs_synthetic)
        edges = self._build_edges(root, parentless, needs_synthetic)
        self._refuse_cycles(edges)
        return Doc(
            nodes=self.nodes,
            root=root.idx,
            edges=edges,
            refusals=self.refusals,
            warnings=self.warnings,
        )

    def _preamble_has_content(self) -> bool:
        preamble = self.preamble
        return bool(
            collapse_blank_runs(preamble.desc)
            or preamble.annotations
            or preamble.acceptance
            or preamble.after_raw
            or preamble.tags
            or preamble.priority
            or preamble.flow
            or preamble.due
        )

    def _assign_slugs(self, needs_synthetic: bool) -> None:
        """Assign every node its identity slug, qualifying duplicates.

        The base slug is the title's :func:`slugify`. Titles with no ASCII word
        refuse. A base slug shared by several nodes -- or one that lands on the
        reserved ``document-root`` while a synthetic root needs it -- qualifies:
        structure first (``parent--slug``), then distinguishing words
        (``slug--words``), else the document refuses. Nodes finalize in index
        order so a parent's slug is settled before it qualifies a child.
        """
        base: dict[int, str] = {}
        groups: dict[str, list[Node]] = {}
        for node in self.nodes:
            slug = slugify(node.title)
            base[node.idx] = slug
            if slug:
                groups.setdefault(slug, []).append(node)
            else:
                self.refusals.append(
                    f"title has no ASCII words: {node.title} (line {node.line})"
                )
        refused: set[str] = set()
        for node in self.nodes:
            slug = base[node.idx]
            if not slug:
                continue
            members = groups[slug]
            reserved = needs_synthetic and slug == DOCUMENT_ROOT_SLUG
            if len(members) == 1 and not reserved:
                node.slug = slug
            else:
                node.slug = self._qualify(node, slug, members, reserved, refused)

    def _qualify(
        self,
        node: Node,
        base: str,
        members: list[Node],
        reserved: bool,
        refused: set[str],
    ) -> str:
        rivals = [member for member in members if member is not node]
        if node.parent is None:
            if reserved:
                self.refusals.append(
                    f"title collides with the synthetic root: {node.title} "
                    f"(line {node.line}); rename or nest it"
                )
                return base
            # Parentless occurrences share one structural slot -- root position --
            # so they qualify each other by wording; a lone one keeps the bare slug.
            cohort = [rival for rival in rivals if rival.parent is None]
            if not cohort:
                return base
        else:
            # A parent that no sibling shares distinguishes structurally; only
            # occurrences under the same parent fall through to wording.
            cohort = [rival for rival in rivals if rival.parent == node.parent]
            if not cohort:
                parent_slug = self.nodes[node.parent].slug
                return self._qualified(
                    node, f"{parent_slug}{QUALIFIER_SEPARATOR}{base}"
                )
        words = self._distinguishing_words(node, cohort)
        if words:
            return self._qualified(node, f"{base}{QUALIFIER_SEPARATOR}{words}")
        if base not in refused:
            refused.add(base)
            lines = sorted({node.line, *(rival.line for rival in cohort)})
            self.refusals.append(
                f"duplicate title in document: {base} "
                f"(lines {lines[0]} and {lines[-1]}; nothing distinguishes them)"
            )
        return base

    def _qualified(self, node: Node, slug: str) -> str:
        self.warn(
            node.line, "dup-qualified", f"duplicated title took qualified slug {slug}"
        )
        return slug

    def _distinguishing_words(self, node: Node, rivals: list[Node]) -> str:
        rival_words: set[str] = set()
        for rival in rivals:
            rival_words.update(title_words(rival.title))
        distinguishers: list[str] = []
        seen: set[str] = set()
        for word in title_words(node.title):
            if word not in rival_words and word not in seen:
                seen.add(word)
                distinguishers.append(word)
        return "-".join(distinguishers)

    def _resolve_root(self, parentless: list[Node], needs_synthetic: bool) -> Node:
        if needs_synthetic:
            root = Node(
                idx=len(self.nodes),
                kind="document",
                title=DOCUMENT_ROOT_TITLE,
                line=0,
                slug=DOCUMENT_ROOT_SLUG,
            )
            self.nodes.append(root)
        else:
            root = parentless[0]
        self._merge_preamble(root)
        return root

    def _merge_preamble(self, root: Node) -> None:
        preamble = self.preamble
        body = collapse_blank_runs(preamble.desc)
        if body:
            separator = [""] if root.desc else []
            root.desc = [*body, *separator, *root.desc]
        root.acceptance = [*preamble.acceptance, *root.acceptance]
        root.annotations = [*preamble.annotations, *root.annotations]
        root.after_raw = [*preamble.after_raw, *root.after_raw]
        root.tags = [*preamble.tags, *root.tags]
        if root.priority is None:
            root.priority = preamble.priority
        if root.flow is None:
            root.flow = preamble.flow
        if root.due is None:
            root.due = preamble.due

    def _build_edges(
        self, root: Node, parentless: list[Node], needs_synthetic: bool
    ) -> list[tuple[int, int, str]]:
        """Combine containment, ordered chains, and resolved ``After`` edges.

        Containment comes from parenthood; the synthetic root depends on each
        parentless node; ordered runs and every node's ``After:`` targets (the
        root now also carrying the preamble's) add dependency edges. An
        ``After`` edge that merely restates containment is dropped, so the
        preamble depending on its own section never doubles the tree edge.
        """
        edges: list[tuple[int, int, str]] = []
        containment: set[tuple[int, int]] = set()
        for node in self.nodes:
            for child in node.children:
                edges.append((node.idx, child, "containment"))
                containment.add((node.idx, child))
        after_seen: set[tuple[int, int]] = set()

        def add_after(source: int, target: int) -> None:
            if (source, target) in containment or (source, target) in after_seen:
                return
            after_seen.add((source, target))
            edges.append((source, target, "after"))

        if needs_synthetic:
            for node in parentless:
                add_after(root.idx, node.idx)
        for source, target, _kind in self.sequence_edges:
            add_after(source, target)
        by_slug = {node.slug: node.idx for node in self.nodes if node.slug}
        by_base: dict[str, list[int]] = {}
        for node in self.nodes:
            by_base.setdefault(slugify(node.title), []).append(node.idx)
        for node in self.nodes:
            for slug, line in node.after_raw:
                target = self._resolve_after(slug, line, by_slug, by_base)
                if target is not None:
                    add_after(node.idx, target)
        return edges

    def _resolve_after(
        self,
        slug: str,
        line: int,
        by_slug: dict[str, int],
        by_base: dict[str, list[int]],
    ) -> int | None:
        if slug in by_slug:
            return by_slug[slug]
        matches = by_base.get(slug, [])
        if len(matches) > 1:
            self.refusals.append(
                f"ambiguous After target: {slug} "
                "(title is duplicated; use a qualified slug)"
            )
            return None
        if matches:
            return matches[0]
        self.refusals.append(f"unknown After target: {slug} (line {line})")
        return None

    def _refuse_cycles(self, edges: list[tuple[int, int, str]]) -> None:
        """Refuse once at the first node a directed edge closes a cycle onto.

        Containment and dependency edges share one directed graph. An iterative
        depth-first search -- so deep documents never exhaust the call stack --
        marks each node grey while on the stack; an edge back to a grey node is
        a cycle, reported by that node's slug. Roots and neighbors are walked in
        index order so the named slug is deterministic.
        """
        adjacency: dict[int, list[int]] = {node.idx: [] for node in self.nodes}
        for source, target, _kind in edges:
            adjacency[source].append(target)
        for neighbors in adjacency.values():
            neighbors.sort()
        white, grey, black = 0, 1, 2
        color = dict.fromkeys(adjacency, white)
        for start in sorted(adjacency):
            if color[start] != white:
                continue
            color[start] = grey
            stack = [(start, iter(adjacency[start]))]
            while stack:
                node_idx, neighbors = stack[-1]
                advanced = False
                for nxt in neighbors:
                    if color[nxt] == grey:
                        self.refusals.append(
                            f"dependency cycle at {self.nodes[nxt].slug}"
                        )
                        return
                    if color[nxt] == white:
                        color[nxt] = grey
                        stack.append((nxt, iter(adjacency[nxt])))
                        advanced = True
                        break
                if not advanced:
                    color[node_idx] = black
                    stack.pop()


def parse(text: str) -> Doc:
    parser = Parser()
    parser.feed(text.removeprefix("\ufeff"))
    return parser.document()


def _link_residue(stripped: str) -> str:
    residue = _INLINE_LINK_RE.sub("", stripped)
    return re.sub(r"[\s\W]+", " ", residue).strip()


def _annotation_shape(stripped: str) -> str | None:
    if stripped.startswith(">"):
        return "blockquote"
    if stripped.startswith("|"):
        return "table"
    if _LINKDEF_RE.match(stripped):
        return "link-definition"
    if (
        _INLINE_LINK_RE.search(stripped)
        and len(_link_residue(stripped)) <= _LINK_RESIDUE_CHARS
    ):
        return "link-dominated"
    return None


def _field_parts(text: str) -> tuple[str, str] | None:
    emphasized = _EMPHASIS_FIELD_RE.match(text)
    if emphasized and (emphasized.group(3) or emphasized.group(4)):
        label = emphasized.group(2).strip().lower()
        value = emphasized.group(5).strip()
    else:
        plain = _PLAIN_FIELD_RE.match(text)
        if plain is None:
            return None
        label = plain.group(1).strip().lower()
        value = plain.group(2).strip()
    canonical = FIELD_LABELS.get(label)
    return (canonical, value) if canonical is not None else None


def _fieldish_label(text: str) -> str | None:
    emphasized = _EMPHASIS_FIELD_RE.match(text)
    if emphasized and (emphasized.group(3) or emphasized.group(4)):
        label = emphasized.group(2).strip()
    else:
        plain = _PLAIN_FIELD_RE.match(text)
        if plain is None:
            return None
        label = plain.group(1).strip()
    return None if label.lower() in FIELD_LABELS else label


def _simple_slug(title: str) -> str:
    # Field-label and after-target normalization reuse the canonical title
    # slugger so classification and identity never diverge on the slug rule.
    return slugify(title)


def _after_targets(value: str) -> list[str] | None:
    targets = [target.strip() for target in value.split(",")]
    if not targets or any(not target for target in targets):
        return None
    normalized: list[str] = []
    for target in targets:
        if _SLUG_RE.fullmatch(target):
            normalized.append(target)
            continue
        if re.search(r"[.!?;:]", target) and not target[0].isupper():
            return None
        slug = _simple_slug(target)
        if not slug:
            return None
        normalized.append(slug)
    return normalized


def _list_prose(item: re.Match[str], title: str) -> str:
    return "".join((item.group(1), item.group(2), item.group(3), title))
