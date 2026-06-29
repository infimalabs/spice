"""C# class member ranking via the tree-sitter seam."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Sequence

from spice.studies.walk import is_excluded_path

DEFAULT_MEMBER_LIMIT = 10

if TYPE_CHECKING:
    from spice.studies.treesitter import TreeSitterNode


@dataclass(frozen=True)
class CSharpMemberRecord:
    kind: str
    name: str
    line_start: int
    line_end: int
    length: int
    signature: str


@dataclass(frozen=True)
class CSharpClassRecord:
    path: str
    name: str
    line_start: int
    line_end: int
    length: int
    member_count: int
    members: tuple[CSharpMemberRecord, ...]


def collect_csharp_class_records(
    paths: list[Path],
    *,
    root: Path,
    class_name: str | None = None,
) -> list[CSharpClassRecord]:
    records: list[CSharpClassRecord] = []
    for rel_path in paths:
        if rel_path.suffix.lower() != ".cs" or is_excluded_path(
            rel_path, repo_root=root
        ):
            continue
        abs_path = root / rel_path
        if not abs_path.exists() or not abs_path.is_file():
            continue
        records.extend(
            _collect_file_class_records(
                rel_path, abs_path.read_bytes(), class_name=class_name
            )
        )
    return records


def _collect_file_class_records(
    rel_path: Path, source: bytes, *, class_name: str | None
) -> list[CSharpClassRecord]:
    from spice.studies import treesitter

    parsed = treesitter.parse_source(rel_path, source)
    if parsed is None or parsed.language != "csharp":
        return []
    records: list[CSharpClassRecord] = []
    for class_node in _iter_class_nodes(parsed.root):
        name = _class_name(source, class_node)
        if class_name and name != class_name:
            continue
        body = _class_body(class_node)
        if body is None:
            continue
        members = tuple(
            _member_record(source, child)
            for child in body.named_children
            if _is_member_declaration(child)
        )
        line_start = _start_line(class_node)
        line_end = _end_line(class_node)
        records.append(
            CSharpClassRecord(
                path=rel_path.as_posix(),
                name=name,
                line_start=line_start,
                line_end=line_end,
                length=line_end - line_start + 1,
                member_count=len(members),
                members=members,
            )
        )
    return records


def _node_text(source: bytes, node: "TreeSitterNode") -> str:
    return source[node.start_byte : node.end_byte].decode("utf-8", "replace")


def _iter_class_nodes(node: "TreeSitterNode") -> list["TreeSitterNode"]:
    found: list[TreeSitterNode] = []
    stack = [node]
    while stack:
        current = stack.pop()
        if current.type == "class_declaration":
            found.append(current)
        stack.extend(reversed(current.children))
    return found


def _first_identifier_text(source: bytes, node: "TreeSitterNode") -> str | None:
    for child in node.children:
        if child.type == "identifier":
            return _node_text(source, child)
    return None


def _class_name(source: bytes, node: "TreeSitterNode") -> str:
    return _first_identifier_text(source, node) or "<anonymous-class>"


def _class_body(node: "TreeSitterNode") -> "TreeSitterNode | None":
    for child in node.children:
        if child.type == "declaration_list":
            return child
    return None


def _is_member_declaration(node: "TreeSitterNode") -> bool:
    return node.type == "field_declaration" or node.type.endswith("_declaration")


def _field_name(source: bytes, node: "TreeSitterNode") -> str:
    for child in node.children:
        if child.type != "variable_declaration":
            continue
        for grandchild in child.children:
            if grandchild.type != "variable_declarator":
                continue
            name = _first_identifier_text(source, grandchild)
            if name:
                return name
    return "<field>"


def _member_name(source: bytes, node: "TreeSitterNode") -> str:
    if node.type == "field_declaration":
        return _field_name(source, node)
    return _first_identifier_text(source, node) or f"<{node.type}>"


def _member_signature(source: bytes, node: "TreeSitterNode") -> str:
    text = _node_text(source, node).strip()
    return text.splitlines()[0].strip() if text else ""


def _member_record(source: bytes, node: "TreeSitterNode") -> CSharpMemberRecord:
    line_start = _start_line(node)
    line_end = _end_line(node)
    return CSharpMemberRecord(
        kind=node.type,
        name=_member_name(source, node),
        line_start=line_start,
        line_end=line_end,
        length=line_end - line_start + 1,
        signature=_member_signature(source, node),
    )


def _start_line(node: "TreeSitterNode") -> int:
    point = node.start_point
    return point.row + 1


def _end_line(node: "TreeSitterNode") -> int:
    point = node.end_point
    return point.row + 1


def csharp_members_payload(records: list[CSharpClassRecord]) -> dict[str, Any]:
    return {
        "classCount": len(records),
        "classes": [
            {
                **asdict(record),
                "members": [asdict(member) for member in record.members],
            }
            for record in records
        ],
    }


def render_csharp_members_json(records: list[CSharpClassRecord]) -> str:
    return json.dumps(csharp_members_payload(records), indent=2)


def render_csharp_members_board(
    records: list[CSharpClassRecord], *, limit: int = DEFAULT_MEMBER_LIMIT
) -> str:
    if not records:
        return "csharp-members: no C# classes found"
    blocks = [f"csharp-members: {len(records)} class(es)"]
    for record in records:
        lines = [
            (
                f"{record.path}:{record.name} lines="
                f"{record.line_start}-{record.line_end} length={record.length} "
                f"members={record.member_count}"
            )
        ]
        kind_counts: dict[str, int] = {}
        for member in record.members:
            kind_counts[member.kind] = kind_counts.get(member.kind, 0) + 1
        if kind_counts:
            lines.append(
                "  kinds="
                + ", ".join(
                    f"{kind}:{count}" for kind, count in sorted(kind_counts.items())
                )
            )
        longest = sorted(
            record.members, key=lambda member: (-member.length, member.line_start)
        )[:limit]
        if longest:
            lines.append(f"  longest_top_{limit}=")
            lines.extend(_render_member_lines(longest))
        tail_count = min(limit, len(record.members))
        tail = record.members[-tail_count:]
        if tail:
            lines.append(f"  tail_{tail_count}=")
            lines.extend(_render_member_lines(tail))
        blocks.append("\n".join(lines))
    return "\n\n".join(blocks)


def _render_member_lines(members: Sequence[CSharpMemberRecord]) -> list[str]:
    return [
        (
            f"    {member.length:>4} lines  {member.kind}  {member.name}  "
            f"{member.line_start}-{member.line_end}  {member.signature}"
        )
        for member in members
    ]
