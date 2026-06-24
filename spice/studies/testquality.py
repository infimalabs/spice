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
            if not isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                continue
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
