"""Magic-number regressions: staged literals diffed against a HEAD baseline.

The scan finds bare integer literals buried in comparisons, slices, and
default arguments — positions where behaviour silently pivots on an unnamed
threshold. Literals in assignments, call arguments, and arithmetic are the
constant *being defined or passed*, not a hidden pivot, so they pass. The
gate is a ratchet, not an amnesty: only literals absent from the same file at
the baseline ref fail, so existing debt does not block unrelated commits
while new debt cannot land.

Library seam: target-repo tools may import the public finding dataclass,
scan/detect helpers, and `render_magic_board`; underscored names remain
private.
"""

from __future__ import annotations

import ast
import re
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.policy import (
    C_GRAMMAR_SUFFIXES,
    MAGIC_BASELINE_REF,
    MAGIC_EXAMINE_VALUE_THRESHOLD,
    MAGIC_SUFFIXES,
)
from spice.studies.walk import git_blob_text, is_excluded_path

EXAMINE_PARENT_KINDS = frozenset({"default_arg", "compare", "slice"})

_ALL_CAPS_RE = re.compile(r"^[A-Z][A-Z0-9_]*$")
_C_COMMENT_RE = re.compile(r"//.*$|/\*.*?\*/", re.DOTALL)
_C_NUMBER = r"-?\d[\d_]*(?:\.\d+)?"
# A number on either side of a comparison operator. `=>` (arrow), `>>`/`<<`
# (shifts), and `>=`-style compounds are carved out by the look-arounds.
_C_COMPARE_RE = re.compile(
    r"(?:(?<![=<>!])(?:<=|>=|===|!==|==|!=|<|>)\s*(" + _C_NUMBER + r")(?![\w.])"
    r"|(?<![\w.])(" + _C_NUMBER + r")\s*(?:<=|>=|===|!==|==|!=|<(?![<=])|>(?![>=])))"
)
_CS_COMPARISON_OPS = frozenset({"==", "!=", ">", "<", ">=", "<="})
_CS_INT_SUFFIX_RE = re.compile(r"[uUlL]+$", re.ASCII)
_JS_COMPARISON_OPS = frozenset({"==", "===", "!=", "!==", ">", "<", ">=", "<="})
_JS_BIGINT_SUFFIX_RE = re.compile(r"n$", re.ASCII)
_TREE_SITTER_LITERAL_QUERY_BY_LANGUAGE = {
    "csharp": "(integer_literal) @literal",
    "javascript": "(number) @literal",
}


@dataclass(frozen=True)
class MagicFinding:
    path: str
    line: int
    literal: str


def scan_paths_magic_numbers(
    paths: list[Path],
    *,
    root: Path,
    examine_threshold: int = MAGIC_EXAMINE_VALUE_THRESHOLD,
    suffixes: tuple[str, ...] = MAGIC_SUFFIXES,
    c_grammar_suffixes: tuple[str, ...] = C_GRAMMAR_SUFFIXES,
) -> list[MagicFinding]:
    findings: list[MagicFinding] = []
    for rel_path in paths:
        if rel_path.suffix not in suffixes or is_excluded_path(
            rel_path, repo_root=root
        ):
            continue
        abs_path = root / rel_path
        if not abs_path.exists():
            continue
        text = abs_path.read_text(encoding="utf-8", errors="replace")
        findings.extend(
            scan_text_magic_numbers(
                rel_path,
                text,
                examine_threshold=examine_threshold,
                c_grammar_suffixes=c_grammar_suffixes,
            )
        )
    return findings


def scan_text_magic_numbers(
    rel_path: Path,
    text: str,
    *,
    examine_threshold: int = MAGIC_EXAMINE_VALUE_THRESHOLD,
    c_grammar_suffixes: tuple[str, ...] = C_GRAMMAR_SUFFIXES,
) -> list[MagicFinding]:
    if rel_path.suffix == ".py":
        return _scan_python(rel_path, text, examine_threshold=examine_threshold)
    from spice.studies import treesitter

    if treesitter.language_for_path(rel_path) is not None:
        return _scan_tree_sitter(
            rel_path,
            text,
            examine_threshold=examine_threshold,
        )
    if rel_path.suffix not in c_grammar_suffixes:
        return []
    return _scan_c_grammar(rel_path, text, examine_threshold=examine_threshold)


def _scan_tree_sitter(
    rel_path: Path,
    text: str,
    *,
    examine_threshold: int,
) -> list[MagicFinding]:
    from spice.studies import treesitter

    parsed = treesitter.parse_source(rel_path, text)
    if parsed is None:
        raise SpiceError(f"magic-numbers: tree-sitter parse unavailable for {rel_path}")
    literal_nodes = _tree_sitter_literal_nodes(
        parsed.suffix, parsed.language, parsed.root
    )
    if parsed.language == "csharp":
        return _scan_csharp_tree(
            rel_path,
            parsed.root,
            parsed.source,
            literal_nodes,
            examine_threshold=examine_threshold,
        )
    if parsed.language == "javascript":
        return _scan_javascript_tree(
            rel_path,
            parsed.source,
            literal_nodes,
            examine_threshold=examine_threshold,
        )
    raise SpiceError(
        f"magic-numbers: unsupported tree-sitter language {parsed.language}"
    )


def _tree_sitter_literal_nodes(suffix: str, language: str, root: Any) -> list[Any]:
    from tree_sitter import QueryCursor

    from spice.studies import treesitter

    query_source = _TREE_SITTER_LITERAL_QUERY_BY_LANGUAGE.get(language)
    if query_source is None:
        raise SpiceError(f"magic-numbers: unsupported tree-sitter language {language}")
    query = treesitter.query_for_suffix(suffix, query_source)
    if query is None:
        raise SpiceError(f"magic-numbers: tree-sitter query unavailable for {suffix}")
    captures = QueryCursor(query).captures(root)
    return sorted(captures.get("literal", ()), key=lambda node: node.start_byte)


def _examine_value(value: float, *, threshold: int) -> bool:
    return abs(value) >= threshold


# ---- Python: ast parents classify the literal's position --------------------

_PY_PARENT_KIND_MAP: tuple[tuple[type, str], ...] = (
    (ast.arguments, "default_arg"),
    (ast.Compare, "compare"),
    (ast.Slice, "slice"),
)


def _scan_python(
    rel_path: Path,
    text: str,
    *,
    examine_threshold: int,
) -> list[MagicFinding]:
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return []
    exempt_ids = _python_exempt_constant_ids(tree)
    parent_map: dict[int, ast.AST] = {}
    for node in ast.walk(tree):
        for child in ast.iter_child_nodes(node):
            parent_map[id(child)] = node
    findings: list[MagicFinding] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Constant) or id(node) in exempt_ids:
            continue
        value = node.value
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            continue
        if not _examine_value(value, threshold=examine_threshold):
            continue
        if _python_parent_kind(node, parent_map) not in EXAMINE_PARENT_KINDS:
            continue
        findings.append(
            MagicFinding(
                path=rel_path.as_posix(), line=node.lineno, literal=repr(value)
            )
        )
    return findings


def _python_parent_kind(node: ast.AST, parent_map: dict[int, ast.AST]) -> str:
    parent = parent_map.get(id(node))
    if isinstance(parent, ast.UnaryOp):
        parent = parent_map.get(id(parent))
    if parent is None:
        return "other"
    for parent_type, kind in _PY_PARENT_KIND_MAP:
        if isinstance(parent, parent_type):
            return kind
    return "other"


def _python_exempt_constant_ids(tree: ast.Module) -> set[int]:
    """Constants inside module-level ALL_CAPS assignments — the named home."""
    exempt: set[int] = set()
    for stmt in tree.body:
        if isinstance(stmt, ast.Assign):
            targets: list[ast.expr] = list(stmt.targets)
            value: ast.expr = stmt.value
        elif isinstance(stmt, ast.AnnAssign) and stmt.value is not None:
            targets = [stmt.target]
            value = stmt.value
        else:
            continue
        if not all(
            isinstance(target, ast.Name) and _ALL_CAPS_RE.fullmatch(target.id)
            for target in targets
        ):
            continue
        for node in ast.walk(value):
            if isinstance(node, ast.Constant):
                exempt.add(id(node))
    return exempt


# ---- C# and JavaScript: tree-sitter parent classification -------------------


def _scan_csharp_tree(
    rel_path: Path,
    root: Any,
    source: bytes,
    literal_nodes: list[Any],
    *,
    examine_threshold: int,
) -> list[MagicFinding]:
    exempt_spans = _csharp_exempt_constant_spans(root, source)
    findings: list[MagicFinding] = []
    for node in literal_nodes:
        finding = _csharp_literal_finding(
            rel_path,
            node,
            source,
            exempt_spans=exempt_spans,
            examine_threshold=examine_threshold,
        )
        if finding is not None:
            findings.append(finding)
    return findings


def _scan_javascript_tree(
    rel_path: Path,
    source: bytes,
    literal_nodes: list[Any],
    *,
    examine_threshold: int,
) -> list[MagicFinding]:
    findings: list[MagicFinding] = []
    for node in literal_nodes:
        finding = _javascript_literal_finding(
            rel_path,
            node,
            source,
            examine_threshold=examine_threshold,
        )
        if finding is not None:
            findings.append(finding)
    return findings


def _tree_sitter_walk(node: Any) -> Iterable[Any]:
    yield node
    for child in node.children:
        yield from _tree_sitter_walk(child)


def _tree_sitter_node_text(node: Any, source: bytes) -> str:
    return source[node.start_byte : node.end_byte].decode("utf-8", "replace")


def _csharp_exempt_constant_spans(root: Any, source: bytes) -> set[tuple[int, int]]:
    exempt: set[tuple[int, int]] = set()
    for node in _tree_sitter_walk(root):
        if node.type not in {"field_declaration", "local_declaration_statement"}:
            continue
        if not any(
            child.type == "modifier"
            and _tree_sitter_node_text(child, source) == "const"
            for child in node.children
        ):
            continue
        for declarator in _tree_sitter_walk(node):
            if declarator.type not in {
                "variable_declarator",
                "local_variable_declarator",
            }:
                continue
            name = next(
                (
                    _tree_sitter_node_text(child, source)
                    for child in declarator.children
                    if child.type == "identifier"
                ),
                "",
            )
            if _ALL_CAPS_RE.fullmatch(name) is None:
                continue
            for literal in _tree_sitter_walk(declarator):
                if literal.type == "integer_literal":
                    exempt.add((literal.start_byte, literal.end_byte))
    return exempt


def _csharp_literal_finding(
    rel_path: Path,
    node: Any,
    source: bytes,
    *,
    exempt_spans: set[tuple[int, int]],
    examine_threshold: int,
) -> MagicFinding | None:
    if (node.start_byte, node.end_byte) in exempt_spans:
        return None
    value = _csharp_int_value(_tree_sitter_node_text(node, source))
    if value is None:
        return None
    parent_kind, sign = _csharp_parent_kind_and_sign(node)
    value *= sign
    if parent_kind not in EXAMINE_PARENT_KINDS or not _examine_value(
        value, threshold=examine_threshold
    ):
        return None
    return MagicFinding(
        path=rel_path.as_posix(), line=node.start_point[0] + 1, literal=str(value)
    )


def _csharp_int_value(text: str) -> int | None:
    normalized = _CS_INT_SUFFIX_RE.sub("", text).replace("_", "")
    try:
        if normalized.startswith(("0x", "0X")):
            return int(normalized, 16)
        if normalized.startswith(("0b", "0B")):
            return int(normalized, 2)
        return int(normalized)
    except ValueError:
        return None


def _csharp_parent_kind_and_sign(node: Any) -> tuple[str, int]:
    parent = node.parent
    if parent is None:
        return "other", 1
    if parent.type == "prefix_unary_expression":
        sign = -1 if any(child.type == "-" for child in parent.children) else 1
        grandparent = parent.parent
        return (
            _csharp_parent_kind(grandparent) if grandparent is not None else "other",
            sign,
        )
    return _csharp_parent_kind(parent), 1


def _csharp_parent_kind(parent: Any) -> str:
    if parent.type == "parameter":
        return "default_arg"
    if parent.type == "binary_expression":
        child_types = {child.type for child in parent.children}
        return "compare" if child_types & _CS_COMPARISON_OPS else "binop"
    if parent.type == "argument":
        grandparent = parent.parent
        if grandparent is not None and grandparent.type == "bracketed_argument_list":
            return "subscript"
        return "call_arg"
    if parent.type == "range_expression":
        return "slice"
    if parent.type == "attribute_argument":
        return "call_arg"
    if parent.type == "constant_pattern":
        return "compare"
    if parent.type in {"variable_declarator", "local_variable_declarator"}:
        return "assign"
    return "other"


def _javascript_literal_finding(
    rel_path: Path,
    node: Any,
    source: bytes,
    *,
    examine_threshold: int,
) -> MagicFinding | None:
    value = _javascript_int_value(_tree_sitter_node_text(node, source))
    if value is None:
        return None
    parent_kind, sign = _javascript_parent_kind_and_sign(node)
    value *= sign
    if parent_kind not in EXAMINE_PARENT_KINDS or not _examine_value(
        value, threshold=examine_threshold
    ):
        return None
    return MagicFinding(
        path=rel_path.as_posix(), line=node.start_point[0] + 1, literal=str(value)
    )


def _javascript_int_value(text: str) -> int | None:
    normalized = _JS_BIGINT_SUFFIX_RE.sub("", text).replace("_", "")
    try:
        if normalized.startswith(("0x", "0X")):
            return int(normalized, 16)
        if normalized.startswith(("0b", "0B")):
            return int(normalized, 2)
        if normalized.startswith(("0o", "0O")):
            return int(normalized, 8)
        if any(char in normalized for char in (".", "e", "E")):
            return None
        return int(normalized)
    except ValueError:
        return None


def _javascript_parent_kind_and_sign(node: Any) -> tuple[str, int]:
    parent = node.parent
    if parent is None:
        return "other", 1
    if parent.type == "unary_expression":
        sign = -1 if any(child.type == "-" for child in parent.children) else 1
        grandparent = parent.parent
        return (
            _javascript_parent_kind(grandparent)
            if grandparent is not None
            else "other",
            sign,
        )
    return _javascript_parent_kind(parent), 1


def _javascript_parent_kind(parent: Any) -> str:
    if parent.type == "binary_expression":
        child_types = {child.type for child in parent.children}
        return "compare" if child_types & _JS_COMPARISON_OPS else "binop"
    if parent.type == "assignment_pattern":
        grandparent = parent.parent
        if grandparent is not None and grandparent.type == "formal_parameters":
            return "default_arg"
    return "other"


# ---- C-grammar family: comparison-adjacent literals by regex ----------------
# Every non-Python language in MAGIC_SUFFIXES shares `//`/`/* */` comments and
# C comparison syntax. Without a parser, comparisons are the one
# examine-position a regex can hold reliably; default args and slices pass
# here, comparisons do not.


def _scan_c_grammar(
    rel_path: Path,
    text: str,
    *,
    examine_threshold: int,
) -> list[MagicFinding]:
    findings: list[MagicFinding] = []
    in_template = False
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        line, in_template = _strip_template_literals(raw_line, in_template)
        line = _C_COMMENT_RE.sub("", line)
        for match in _C_COMPARE_RE.finditer(line):
            literal = (match.group(1) or match.group(2)).replace("_", "")
            try:
                value = float(literal) if "." in literal else int(literal)
            except ValueError:
                continue
            if not _examine_value(value, threshold=examine_threshold):
                continue
            findings.append(
                MagicFinding(
                    path=rel_path.as_posix(), line=line_number, literal=literal
                )
            )
    return findings


def _strip_template_literals(line: str, in_template: bool) -> tuple[str, bool]:
    """Blank out template-literal content; markup strings are not numbers."""
    kept: list[str] = []
    for char in line:
        if char == "`":
            in_template = not in_template
            continue
        if not in_template:
            kept.append(char)
    return "".join(kept), in_template


def detect_magic_regressions(
    paths: list[Path],
    *,
    root: Path,
    baseline_ref: str = MAGIC_BASELINE_REF,
    examine_threshold: int = MAGIC_EXAMINE_VALUE_THRESHOLD,
    suffixes: tuple[str, ...] = MAGIC_SUFFIXES,
    c_grammar_suffixes: tuple[str, ...] = C_GRAMMAR_SUFFIXES,
) -> list[MagicFinding]:
    """Findings in the working copies that are absent from the baseline blobs.

    Baseline membership is per (path, literal): moving an existing literal
    around a file is not a regression; introducing a new one is.
    """
    regressions: list[MagicFinding] = []
    for rel_path in paths:
        if rel_path.suffix not in suffixes or is_excluded_path(
            rel_path, repo_root=root
        ):
            continue
        abs_path = root / rel_path
        if not abs_path.exists():
            continue
        current = scan_text_magic_numbers(
            rel_path,
            abs_path.read_text(encoding="utf-8", errors="replace"),
            examine_threshold=examine_threshold,
            c_grammar_suffixes=c_grammar_suffixes,
        )
        if not current:
            continue
        baseline_text = git_blob_text(root, baseline_ref, rel_path)
        baseline_literals = (
            {
                finding.literal
                for finding in scan_text_magic_numbers(
                    rel_path,
                    baseline_text,
                    examine_threshold=examine_threshold,
                    c_grammar_suffixes=c_grammar_suffixes,
                )
            }
            if baseline_text is not None
            else set()
        )
        regressions.extend(
            finding for finding in current if finding.literal not in baseline_literals
        )
    return regressions


def render_magic_board(
    findings: list[MagicFinding], *, baseline_ref: str = MAGIC_BASELINE_REF
) -> str:
    if not findings:
        return f"magic-numbers: ok (baseline {baseline_ref})"
    lines = [
        f"magic-numbers: {len(findings)} regression(s) vs {baseline_ref}; "
        "name each value as a module-level constant"
    ]
    for finding in findings:
        lines.append(f"  FAIL  {finding.path}:{finding.line}: {finding.literal}")
    return "\n".join(lines)
