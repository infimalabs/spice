"""Single-pass task-document line classification and attachment."""

from __future__ import annotations

import re

from spice.tasks.config import APPROVED_PHASES
from spice.tasks.markdown.dialect import (
    CODE_INDENT_COLS,
    FIELD_LABELS,
    Doc,
    Node,
    dedent_content,
    indent_width,
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
        self.prev_blank = True

    def warn(self, line: int, code: str, message: str) -> None:
        self.warnings.append((line, code, message))

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
            return
        self.attach_description(line)
        self.prev_blank = False

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
        annotation = (
            stripped.startswith((">", "|"))
            or _LINKDEF_RE.match(stripped)
            or (
                _INLINE_LINK_RE.search(stripped)
                and len(_link_residue(stripped)) <= _LINK_RESIDUE_CHARS
            )
        )
        if not annotation:
            return False
        target = self.attach_target_for(line)
        target.annotations.append(dedent_content(line, target.content_col))
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
        edges = [
            (node.idx, child, "containment")
            for node in self.nodes
            for child in node.children
        ]
        edges.extend(self.sequence_edges)
        return Doc(
            nodes=self.nodes,
            root=0 if self.nodes else -1,
            edges=edges,
            refusals=self.refusals,
            warnings=self.warnings,
        )


def parse(text: str) -> Doc:
    parser = Parser()
    parser.feed(text.removeprefix("\ufeff"))
    return parser.document()


def _link_residue(stripped: str) -> str:
    residue = _INLINE_LINK_RE.sub("", stripped)
    return re.sub(r"[\s\W]+", " ", residue).strip()


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


def _simple_slug(title: str) -> str:
    linked = _INLINE_LINK_RE.sub(lambda match: match.group(1), title)
    return "-".join(re.findall(r"[a-z0-9]+", linked.lower()))


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
