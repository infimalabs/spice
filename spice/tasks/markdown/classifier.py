"""Single-pass task-document line classification and attachment."""

from __future__ import annotations

import re

from spice.tasks.markdown.dialect import (
    CODE_INDENT_COLS,
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
_ESCAPED_PROSE_RE = re.compile(
    r"^(?:\\[-*+#>|`=~_\[<]|\d+\\[.)]|[A-Za-z][A-Za-z ]{0,30}?\\:)"
)
_LINK_RESIDUE_CHARS = 12


class Parser:
    """Top-to-bottom first-match classifier with attachment state."""

    def __init__(self) -> None:
        self.nodes: list[Node] = []
        self.preamble = Node(idx=-1, kind="preamble", title="", line=0)
        self.current: Node | None = None
        self.heading_stack: list[tuple[int, Node]] = []
        self.list_stack: list[tuple[int, int, Node, bool]] = []
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
        self.last_desc = None

    def handle_line(self, line: str, index: int) -> None:
        stripped = line.strip()
        line_number = index + 1
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

    def handle_heading(self, level: int, title: str, line_number: int) -> None:
        while self.heading_stack and self.heading_stack[-1][0] >= level:
            self.heading_stack.pop()
        parent = self.heading_stack[-1][1] if self.heading_stack else None
        node = self.mint("heading", title, line_number, level, parent)
        self.heading_stack.append((level, node))
        self.list_stack.clear()

    def handle_list_item(self, item: re.Match[str], line_number: int) -> None:
        indent = indent_width(item.group(1))
        marker = item.group(2)
        content_col = indent + len(marker) + len(item.group(3))
        while self.list_stack and self.list_stack[-1][0] >= indent:
            self.list_stack.pop()
        parent = (
            self.list_stack[-1][2]
            if self.list_stack
            else (self.heading_stack[-1][1] if self.heading_stack else None)
        )
        node = self.mint(
            "item",
            item.group(4),
            line_number,
            indent,
            parent,
            content_col=content_col,
        )
        self.list_stack.append((indent, content_col, node, marker[0].isdigit()))

    def code_threshold(self) -> int:
        if self.list_stack:
            return self.list_stack[-1][1] + CODE_INDENT_COLS
        return CODE_INDENT_COLS

    def code_indented(self, line: str) -> bool:
        return indent_width(line) >= self.code_threshold()

    def attach_target_for(self, line: str) -> Node:
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
        target.desc.append(text)
        self.last_desc = (target, target.desc[-1])
        self.last_attach = target

    def document(self) -> Doc:
        edges = [
            (node.idx, child, "containment")
            for node in self.nodes
            for child in node.children
        ]
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
