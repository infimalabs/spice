"""Test-quality studies: deterministic weak-test signals."""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence


@dataclass(frozen=True)
class AssertionFreeTestFinding:
    path: str
    test_name: str
    line: int


def test_paths(repo_root: Path, test_root: str = "tests") -> list[Path]:
    """Return repo-relative Python test files under ``test_root``."""
    root = repo_root / test_root
    if not root.is_dir():
        return []
    return sorted(path.relative_to(repo_root) for path in root.rglob("test*.py"))


def scan_assertion_free_tests(
    paths: Sequence[Path], *, root: Path
) -> list[AssertionFreeTestFinding]:
    """Return test functions that do not appear to constrain behavior."""
    findings: list[AssertionFreeTestFinding] = []
    for rel_path in sorted(paths):
        path = rel_path if rel_path.is_absolute() else root / rel_path
        if not _is_test_file(path):
            continue
        try:
            tree = ast.parse(path.read_bytes())
        except (SyntaxError, OSError):
            continue
        for node in tree.body:
            if not isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                continue
            if not node.name.startswith("test_"):
                continue
            if _function_has_assertion(node):
                continue
            findings.append(
                AssertionFreeTestFinding(
                    path=str(path.relative_to(root)),
                    test_name=node.name,
                    line=node.lineno,
                )
            )
    return findings


def render_assertion_free_board(
    findings: Sequence[AssertionFreeTestFinding],
    *,
    limit: int | None = None,
) -> str:
    """Render assertion-free test findings for CLI and pre-commit output."""
    shown = list(findings)[:limit] if limit is not None else list(findings)
    if not shown:
        return "assertion-free-tests: no assertion-free tests found"
    suffix = f" (showing {len(shown)})" if limit and len(findings) > len(shown) else ""
    rows = [f"assertion-free-tests: {len(findings)} test(s){suffix}"]
    for finding in shown:
        rows.append(f"  {finding.path}:{finding.line} {finding.test_name}")
    return "\n".join(rows)


def _is_test_file(path: Path) -> bool:
    return path.suffix == ".py" and path.name.startswith("test")


def _function_has_assertion(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    return any(_node_is_assertion(child) for child in _walk_test_body(node))


def _walk_test_body(node: ast.AST):
    for child in ast.iter_child_nodes(node):
        if isinstance(
            child,
            ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef | ast.Lambda,
        ):
            continue
        yield child
        yield from _walk_test_body(child)


def _node_is_assertion(node: ast.AST) -> bool:
    if isinstance(node, ast.Assert):
        return True
    if isinstance(node, ast.With | ast.AsyncWith):
        return any(_context_expr_is_assertion(item.context_expr) for item in node.items)
    if isinstance(node, ast.Call):
        return _call_is_assertion(node)
    return False


def _context_expr_is_assertion(expr: ast.AST) -> bool:
    return isinstance(expr, ast.Call) and _call_name(expr) in {
        "pytest.raises",
        "pytest.warns",
    }


def _call_is_assertion(node: ast.Call) -> bool:
    name = _call_name(node)
    leaf = name.rsplit(".", 1)[-1]
    return leaf.startswith("assert") or name == "pytest.fail"


def _call_name(node: ast.Call) -> str:
    parts: list[str] = []
    current: ast.AST = node.func
    while isinstance(current, ast.Attribute):
        parts.append(current.attr)
        current = current.value
    if isinstance(current, ast.Name):
        parts.append(current.id)
    return ".".join(reversed(parts))
