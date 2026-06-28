"""Test-quality studies: deterministic weak-test signals."""

from __future__ import annotations

import ast
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

from spice.errors import SpiceError
from spice.repocfg import policy_table

ASSERTION_HELPERS_KEY = "assertion_helpers"
_ASSERTION_HELPER_NAME_RE = re.compile(
    r"[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*\Z"
)


@dataclass(frozen=True)
class AssertionFreeTestFinding:
    path: str
    test_name: str
    line: int


@dataclass(frozen=True)
class PrivateInternalCouplingFinding:
    path: str
    test_name: str
    line: int
    kind: str
    target: str


def test_paths(repo_root: Path, test_root: str = "tests") -> list[Path]:
    """Return repo-relative Python test files under ``test_root``."""
    root = repo_root / test_root
    if not root.is_dir():
        return []
    found: set[Path] = set()
    found.update(root.rglob("test*.py"))
    found.update(root.rglob("*_test.py"))
    return sorted(path.relative_to(repo_root) for path in found)


def scan_assertion_free_tests(
    paths: Sequence[Path], *, root: Path
) -> list[AssertionFreeTestFinding]:
    """Return test functions that do not appear to constrain behavior."""
    findings: list[AssertionFreeTestFinding] = []
    helpers = _configured_assertion_helpers(root)
    for rel_path in sorted(paths):
        path = rel_path if rel_path.is_absolute() else root / rel_path
        if not _is_test_file(path):
            continue
        try:
            tree = ast.parse(path.read_bytes())
        except (SyntaxError, OSError):
            continue
        display_path = str(path.relative_to(root))
        for node in tree.body:
            if isinstance(node, ast.ClassDef) and node.name.startswith("Test"):
                for method in node.body:
                    if not isinstance(method, ast.FunctionDef | ast.AsyncFunctionDef):
                        continue
                    if not method.name.startswith("test_"):
                        continue
                    if _function_has_assertion(method, helpers):
                        continue
                    findings.append(
                        AssertionFreeTestFinding(
                            path=display_path,
                            test_name=f"{node.name}.{method.name}",
                            line=method.lineno,
                        )
                    )
            elif isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                if not node.name.startswith("test_"):
                    continue
                if _function_has_assertion(node, helpers):
                    continue
                findings.append(
                    AssertionFreeTestFinding(
                        path=display_path,
                        test_name=node.name,
                        line=node.lineno,
                    )
                )
    return findings


def scan_private_internal_coupling(
    paths: Sequence[Path],
    *,
    root: Path,
    packages: Sequence[str] = ("spice",),
) -> list[PrivateInternalCouplingFinding]:
    """Return tests coupled to private names from production packages."""
    findings: list[PrivateInternalCouplingFinding] = []
    package_set = set(packages)
    for rel_path in sorted(paths):
        path = rel_path if rel_path.is_absolute() else root / rel_path
        if not _is_test_file(path):
            continue
        try:
            tree = ast.parse(path.read_bytes())
        except (SyntaxError, OSError):
            continue
        display_path = str(path.relative_to(root))
        findings.extend(_private_import_findings(tree, display_path, package_set))
        for node in tree.body:
            if isinstance(node, ast.ClassDef) and node.name.startswith("Test"):
                for method in node.body:
                    if not isinstance(method, ast.FunctionDef | ast.AsyncFunctionDef):
                        continue
                    if not method.name.startswith("test_"):
                        continue
                    findings.extend(_private_assertion_findings(method, display_path))
            elif isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                if not node.name.startswith("test_"):
                    continue
                findings.extend(_private_assertion_findings(node, display_path))
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


def render_private_internal_board(
    findings: Sequence[PrivateInternalCouplingFinding],
    *,
    limit: int | None = None,
) -> str:
    """Render private-internal coupling findings."""
    shown = list(findings)[:limit] if limit is not None else list(findings)
    if not shown:
        return "private-internals: no private test coupling found"
    suffix = f" (showing {len(shown)})" if limit and len(findings) > len(shown) else ""
    rows = [f"private-internals: {len(findings)} coupling(s){suffix}"]
    for finding in shown:
        rows.append(
            f"  {finding.path}:{finding.line} {finding.test_name}: "
            f"{finding.kind} {finding.target}"
        )
    return "\n".join(rows)


def _is_test_file(path: Path) -> bool:
    return path.suffix == ".py" and (
        path.stem.startswith("test") or path.stem.endswith("_test")
    )


def _configured_assertion_helpers(repo_root: Path) -> frozenset[str]:
    raw = policy_table(repo_root).get(ASSERTION_HELPERS_KEY)
    if raw is None:
        return frozenset()
    if not isinstance(raw, list):
        raise SpiceError(
            f"[tool.spice.policy] {ASSERTION_HELPERS_KEY} must be a list of "
            "callable names"
        )

    helpers: list[str] = []
    for index, item in enumerate(raw, start=1):
        if not isinstance(item, str):
            raise SpiceError(
                f"[tool.spice.policy] {ASSERTION_HELPERS_KEY}[{index}] must be "
                "a callable name string"
            )
        name = item.strip()
        if not name or not _ASSERTION_HELPER_NAME_RE.fullmatch(name):
            raise SpiceError(
                f"[tool.spice.policy] {ASSERTION_HELPERS_KEY}[{index}] must be "
                f"a leaf or dotted callable name: {item!r}"
            )
        if name not in helpers:
            helpers.append(name)
    return frozenset(helpers)


def _function_has_assertion(
    node: ast.FunctionDef | ast.AsyncFunctionDef, assertion_helpers: frozenset[str]
) -> bool:
    return any(
        _node_is_assertion(child, assertion_helpers) for child in _walk_test_body(node)
    )


def _walk_test_body(node: ast.AST):
    for child in ast.iter_child_nodes(node):
        if isinstance(
            child,
            ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef | ast.Lambda,
        ):
            continue
        yield child
        yield from _walk_test_body(child)


def _node_is_assertion(node: ast.AST, assertion_helpers: frozenset[str]) -> bool:
    if isinstance(node, ast.Assert):
        return not _expr_is_static_truth(node.test)
    if isinstance(node, ast.With | ast.AsyncWith):
        return any(_context_expr_is_assertion(item.context_expr) for item in node.items)
    if isinstance(node, ast.Call):
        return _call_is_assertion(node, assertion_helpers)
    return False


def _context_expr_is_assertion(expr: ast.AST) -> bool:
    return isinstance(expr, ast.Call) and _call_name(expr) in {
        "pytest.raises",
        "pytest.warns",
    }


def _call_is_assertion(
    node: ast.Call, assertion_helpers: frozenset[str] = frozenset()
) -> bool:
    name = _call_name(node)
    leaf = name.rsplit(".", 1)[-1]
    if _call_is_static_truth_assertion(node, leaf):
        return False
    return (
        leaf.startswith("assert")
        or name
        in {
            "pytest.fail",
            "pytest.raises",
            "pytest.warns",
        }
        or _call_matches_assertion_helper(name, leaf, assertion_helpers)
    )


def _call_matches_assertion_helper(
    name: str, leaf: str, assertion_helpers: frozenset[str]
) -> bool:
    return any(
        helper == name if "." in helper else helper == leaf
        for helper in assertion_helpers
    )


def _expr_is_static_truth(node: ast.AST) -> bool:
    return _expr_static_bool(node) is True


def _expr_static_bool(node: ast.AST) -> bool | None:
    literal_truth = _literal_truth(node)
    if literal_truth is not None:
        return literal_truth
    if not isinstance(node, ast.Compare) or len(node.ops) != 1:
        return None
    if len(node.comparators) != 1:
        return None
    left = _literal_value(node.left)
    right = _literal_value(node.comparators[0])
    if left is _UNKNOWN_LITERAL or right is _UNKNOWN_LITERAL:
        return None
    op = node.ops[0]
    try:
        if isinstance(op, ast.Eq):
            return left == right
        if isinstance(op, ast.NotEq):
            return left != right
        if isinstance(op, ast.Is):
            return left is right
        if isinstance(op, ast.IsNot):
            return left is not right
    except TypeError:
        return None
    return None


def _call_is_static_truth_assertion(node: ast.Call, leaf: str) -> bool:
    if not node.args:
        return False
    truth = _expr_static_bool(node.args[0])
    return (leaf == "assertTrue" and truth is True) or (
        leaf == "assertFalse" and truth is False
    )


_UNKNOWN_LITERAL = object()


def _literal_truth(node: ast.AST) -> bool | None:
    value = _literal_value(node)
    if value is _UNKNOWN_LITERAL:
        return None
    return bool(value)


def _literal_value(node: ast.AST):
    try:
        return ast.literal_eval(node)
    except (ValueError, SyntaxError):
        return _UNKNOWN_LITERAL


def _call_name(node: ast.Call) -> str:
    parts: list[str] = []
    current: ast.AST = node.func
    while isinstance(current, ast.Attribute):
        parts.append(current.attr)
        current = current.value
    if isinstance(current, ast.Name):
        parts.append(current.id)
    return ".".join(reversed(parts))


def _private_import_findings(
    tree: ast.Module, path: str, packages: set[str]
) -> list[PrivateInternalCouplingFinding]:
    findings: list[PrivateInternalCouplingFinding] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                private = _private_package_target(alias.name, packages)
                if private:
                    findings.append(
                        PrivateInternalCouplingFinding(
                            path=path,
                            test_name="<module>",
                            line=node.lineno,
                            kind="private import",
                            target=private,
                        )
                    )
        elif isinstance(node, ast.ImportFrom) and node.module:
            private = _private_package_target(node.module, packages)
            if private:
                findings.append(
                    PrivateInternalCouplingFinding(
                        path=path,
                        test_name="<module>",
                        line=node.lineno,
                        kind="private import",
                        target=private,
                    )
                )
            if _package_root(node.module, packages):
                for alias in node.names:
                    if _is_private_name(alias.name):
                        findings.append(
                            PrivateInternalCouplingFinding(
                                path=path,
                                test_name="<module>",
                                line=node.lineno,
                                kind="private import",
                                target=f"{node.module}.{alias.name}",
                            )
                        )
    return findings


def _private_assertion_findings(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    path: str,
) -> list[PrivateInternalCouplingFinding]:
    findings: list[PrivateInternalCouplingFinding] = []
    seen: set[tuple[int, str, str]] = set()
    for child in _walk_test_body(node):
        if not isinstance(child, ast.Assert):
            continue
        for assertion_node in _walk_test_body(child):
            if isinstance(assertion_node, ast.Attribute) and _is_private_name(
                assertion_node.attr
            ):
                _append_private_assertion_finding(
                    findings,
                    seen,
                    path=path,
                    test_name=node.name,
                    line=assertion_node.lineno,
                    kind="private attribute assertion",
                    target=assertion_node.attr,
                )
            elif (
                isinstance(assertion_node, ast.Subscript)
                and isinstance(assertion_node.slice, ast.Constant)
                and isinstance(assertion_node.slice.value, str)
                and _is_private_name(assertion_node.slice.value)
            ):
                _append_private_assertion_finding(
                    findings,
                    seen,
                    path=path,
                    test_name=node.name,
                    line=assertion_node.lineno,
                    kind="private key assertion",
                    target=assertion_node.slice.value,
                )
            elif isinstance(assertion_node, ast.Dict):
                for key in assertion_node.keys:
                    if (
                        isinstance(key, ast.Constant)
                        and isinstance(key.value, str)
                        and _is_private_name(key.value)
                    ):
                        _append_private_assertion_finding(
                            findings,
                            seen,
                            path=path,
                            test_name=node.name,
                            line=key.lineno,
                            kind="private key assertion",
                            target=key.value,
                        )
    return findings


def _append_private_assertion_finding(
    findings: list[PrivateInternalCouplingFinding],
    seen: set[tuple[int, str, str]],
    *,
    path: str,
    test_name: str,
    line: int,
    kind: str,
    target: str,
) -> None:
    key = (line, kind, target)
    if key in seen:
        return
    seen.add(key)
    findings.append(
        PrivateInternalCouplingFinding(
            path=path,
            test_name=test_name,
            line=line,
            kind=kind,
            target=target,
        )
    )


def _private_package_target(module: str, packages: set[str]) -> str | None:
    parts = module.split(".")
    if not parts or parts[0] not in packages:
        return None
    for index, part in enumerate(parts[1:], start=1):
        if _is_private_name(part):
            return ".".join(parts[: index + 1])
    return None


def _package_root(module: str, packages: set[str]) -> bool:
    return bool(module.split(".", 1)[0] in packages)


def _is_private_name(name: str) -> bool:
    return name.startswith("_") and not (name.startswith("__") and name.endswith("__"))
