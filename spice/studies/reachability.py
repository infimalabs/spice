"""Differential reachability: identify modules only tests reach.

Walks the import graph from production entry points (cli, serve, hooks, agent
loop) to build the production-reachable module set. Separately walks imports
from the test suite. Modules in test-reachable but NOT production-reachable are
"test-only" — the exhaust of the agent loop that wrote code to satisfy a gate
without wiring it into any production path.

Library seam: public dataclasses and scan/render helpers are importable by
target-repo tools; underscored names remain private.
"""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence


PRODUCTION_ROOTS = (
    "spice/cli/entry.py",
    "spice/serve/app.py",
    "spice/hooks/precommit.py",
    "spice/hooks/commitmsg.py",
    "spice/hooks/refguard.py",
    "spice/hooks/install.py",
    "spice/agent/cli.py",
)

# Allowlist for modules that are only test-reachable but are legitimately
# dead-tested (e.g., backwards-compat stubs, dynamic dispatch via string keys).
# Entries are dotted module paths within the spice package.
REACHABILITY_ALLOWLIST: tuple[str, ...] = ()


@dataclass(frozen=True)
class ReachabilityFinding:
    module: str
    module_path: str
    only_test_imports: list[str]


def scan_reachability(
    repo_root: Path,
    *,
    package: str = "spice",
    test_root: str = "tests",
    allowlist: Sequence[str] = REACHABILITY_ALLOWLIST,
) -> list[ReachabilityFinding]:
    """Return modules reachable from tests but not from production roots."""
    pkg_root = repo_root / package
    test_dir = repo_root / test_root
    if not pkg_root.is_dir() or not test_dir.is_dir():
        return []

    root_paths = [repo_root / r for r in PRODUCTION_ROOTS if (repo_root / r).is_file()]
    prod_reachable = _walk_imports(root_paths, pkg_root, package)

    test_paths = list(test_dir.glob("*.py"))
    test_reachable = _walk_imports(test_paths, pkg_root, package)

    allowset = set(allowlist)
    findings: list[ReachabilityFinding] = []
    for module in sorted(test_reachable - prod_reachable):
        if module in allowset:
            continue
        mod_path = _module_to_path(module, pkg_root, package)
        if mod_path is None:
            continue
        importers = _find_importers(module, test_paths, package)
        findings.append(
            ReachabilityFinding(
                module=module,
                module_path=str(mod_path.relative_to(repo_root)),
                only_test_imports=sorted(importers),
            )
        )
    return findings


def render_reachability_board(
    findings: Sequence[ReachabilityFinding],
    *,
    limit: int | None = None,
) -> list[str]:
    """Render a text board of test-only modules."""
    rows: list[str] = []
    shown = list(findings)[:limit] if limit is not None else list(findings)
    if not shown:
        rows.append("reachability: no test-only modules found")
        return rows
    rows.append(
        f"reachability: {len(findings)} test-only module(s)"
        + (f" (showing {len(shown)})" if limit and len(findings) > len(shown) else "")
    )
    for f in shown:
        rows.append(f"  {f.module_path}")
        rows.append(f"    module: {f.module}")
        if f.only_test_imports:
            rows.append(f"    imported by: {', '.join(f.only_test_imports)}")
    return rows


def _walk_imports(roots: list[Path], pkg_root: Path, package: str) -> set[str]:
    """BFS import-graph walk; returns dotted package module names reachable."""
    visited: set[str] = set()
    queue: list[Path] = []
    for path in roots:
        mod = _path_to_module(path, pkg_root, package)
        if mod and mod not in visited:
            visited.add(mod)
            queue.append(path)

    while queue:
        path = queue.pop()
        for imp in _direct_imports(path, package):
            if imp in visited:
                continue
            visited.add(imp)
            imp_path = _module_to_path(imp, pkg_root, package)
            if imp_path:
                queue.append(imp_path)
    return visited


def _direct_imports(path: Path, package: str) -> list[str]:
    """Extract dotted module names imported directly by path that are in package."""
    try:
        tree = ast.parse(path.read_bytes())
    except (SyntaxError, OSError):
        return []
    results: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name == package or alias.name.startswith(f"{package}."):
                    results.append(alias.name)
        elif isinstance(node, ast.ImportFrom):
            if node.module is None:
                continue
            mod = _resolve_relative(path, node.module, node.level, package)
            if mod and (mod == package or mod.startswith(f"{package}.")):
                results.append(mod)
    return results


def _resolve_relative(
    source: Path, module: str, level: int, package: str
) -> str | None:
    """Resolve a relative import to its dotted module name."""
    if level == 0:
        return module
    parts = source.parent.parts
    pkg_parts: list[str] = []
    for part in reversed(parts):
        if part == package or pkg_parts:
            pkg_parts.insert(0, part)
    if not pkg_parts:
        return None
    # Go up `level` packages from the source
    anchor = list(pkg_parts)
    for _ in range(level - 1):
        if anchor:
            anchor.pop()
    if not anchor:
        return None
    return ".".join(anchor) + ("." + module if module else "")


def _path_to_module(path: Path, pkg_root: Path, package: str) -> str | None:
    try:
        rel = path.relative_to(pkg_root.parent)
    except ValueError:
        return None
    parts = list(rel.with_suffix("").parts)
    if parts and parts[-1] == "__init__":
        parts = parts[:-1]
    if not parts or parts[0] != package:
        return None
    return ".".join(parts)


def _module_to_path(module: str, pkg_root: Path, package: str) -> Path | None:
    if module == package:
        candidate = pkg_root / "__init__.py"
        return candidate if candidate.is_file() else None
    if not module.startswith(f"{package}."):
        return None
    rel = module[len(package) + 1 :].replace(".", "/")
    # Try as module file
    candidate = pkg_root / (rel + ".py")
    if candidate.is_file():
        return candidate
    # Try as package __init__
    candidate = pkg_root / rel / "__init__.py"
    if candidate.is_file():
        return candidate
    return None


def _find_importers(module: str, test_paths: list[Path], package: str) -> list[str]:
    """Return test file names that directly import module."""
    importers: list[str] = []
    for path in test_paths:
        imps = _direct_imports(path, package)
        if module in imps or any(imp.startswith(f"{module}.") for imp in imps):
            importers.append(path.name)
    return importers
