"""Tree-sitter routine complexity measurements for Rust sources."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.studies import treesitter

_ROUTINES = frozenset({"function_item"})
_SCOPES = frozenset({"impl_item", "trait_item", "mod_item"})
_DECISIONS = frozenset(
    {
        "if_expression",
        "while_expression",
        "for_expression",
        "match_arm",
        "try_expression",
    }
)
_SHORT_CIRCUIT = frozenset({"&&", "||"})


@dataclass(frozen=True, order=True)
class ComplexityRoutine:
    """One Rust routine measured through the tree-sitter complexity contract."""

    path: str
    function_name: str
    ccn: int
    length: int


@dataclass
class _RoutineState:
    path: str
    function_name: str
    ccn: int
    length: int


def measure_complexity(path: Path | str, source: str) -> list[ComplexityRoutine]:
    """Measure every named routine declared by one Rust source file."""
    rendered_path = Path(path).as_posix()
    parsed = treesitter.parse_source(rendered_path, source)
    if parsed is None or parsed.language != "rust":
        raise SpiceError(f"Rust tree-sitter parse unavailable for {rendered_path}")
    if parsed.root.has_error:
        raise SpiceError(f"Rust tree-sitter parse failed for {rendered_path}")
    routines: list[_RoutineState] = []
    _visit(parsed.root, parsed, [], None, routines, rendered_path)
    measured = [
        ComplexityRoutine(
            path=routine.path,
            function_name=routine.function_name,
            ccn=routine.ccn,
            length=routine.length,
        )
        for routine in routines
    ]
    return sorted(measured)


def _visit(
    node: treesitter.TreeSitterNode,
    parsed: treesitter.ParsedTreeSitterSource,
    scope: list[str],
    current: int | None,
    routines: list[_RoutineState],
    path: str,
) -> None:
    current, pushed = _enter_node(node, parsed, scope, current, routines, path)
    for child in node.children:
        _visit(child, parsed, scope, current, routines, path)
    if pushed:
        scope.pop()


def _enter_node(
    node: treesitter.TreeSitterNode,
    parsed: treesitter.ParsedTreeSitterSource,
    scope: list[str],
    current: int | None,
    routines: list[_RoutineState],
    path: str,
) -> tuple[int | None, bool]:
    if node.type in _ROUTINES:
        name = _declared_name(node, parsed)
        if name is None:
            return current, False
        scope.append(name)
        routines.append(_RoutineState(path, "::".join(scope), 1, _span_lines(node)))
        return len(routines) - 1, True
    if node.type in _SCOPES:
        name = _declared_scope_name(node, parsed)
        if name is None:
            return current, False
        scope.append(name)
        return current, True
    if current is not None and _is_decision(node):
        routines[current].ccn += 1
    return current, False


def _is_decision(node: treesitter.TreeSitterNode) -> bool:
    if node.type in _DECISIONS:
        return True
    return node.type == "binary_expression" and any(
        child.type in _SHORT_CIRCUIT for child in node.children
    )


def _declared_name(
    node: treesitter.TreeSitterNode,
    parsed: treesitter.ParsedTreeSitterSource,
) -> str | None:
    return _declared_field(node, parsed, "name") or _declared_field(
        node, parsed, "type"
    )


def _declared_scope_name(
    node: treesitter.TreeSitterNode,
    parsed: treesitter.ParsedTreeSitterSource,
) -> str | None:
    name = _declared_name(node, parsed)
    if name is None or node.type != "impl_item":
        return name
    trait_name = _declared_field(node, parsed, "trait")
    return name if trait_name is None else f"<{name} as {trait_name}>"


def _declared_field(
    node: treesitter.TreeSitterNode,
    parsed: treesitter.ParsedTreeSitterSource,
    field: str,
) -> str | None:
    child = node.child_by_field_name(field)
    if child is None:
        return None
    value = parsed.source[child.start_byte : child.end_byte].decode("utf-8").strip()
    return value if value and "\n" not in value and "\r" not in value else None


def _span_lines(node: treesitter.TreeSitterNode) -> int:
    return node.end_point.row - node.start_point.row + 1
