"""Differential reachability: identify modules only tests reach.

Walks the import graph from production entry points (cli, serve, hooks, agent
loop) to build the production-reachable module set. Separately walks imports
from the test suite. Modules in test-reachable but NOT production-reachable are
"test-only" — the exhaust of the agent loop that wrote code to satisfy a gate
without wiring it into any production path.

``scan_symbol_reachability`` does the same diff at function/class/method
granularity inside production-reachable modules. Static analysis resolves
*named* references; a symbol reached only through ``getattr(obj, name)`` with a
runtime-built ``name`` has no syntactic reference, so it would false-flag as
test-only. There are exactly two supported ways to keep a dynamically-reached
production symbol from being flagged, and the first is strongly preferred:

(a) **Registry convention (preferred, self-documenting).** Reference the symbol
    as a bare ``Name`` in a dict or list literal — a magic-string registry —
    in any production-reachable module:

        DISPATCH = {"sync": handle_sync, "drain": handle_drain}

    The scanner counts each literal value as a production reference (they are
    plain ``Name`` nodes), so registry-dispatched handlers stay live with no
    allowlist entry, and the registry doubles as the single source of truth for
    which string keys exist. Reach for this first.

(b) **Allowlist (escape hatch).** Only when a symbol is reached purely via
    ``getattr``/string and genuinely cannot be expressed as a registry, declare
    the exception in ``SYMBOL_REACHABILITY_ALLOWLIST`` (or the ``allowlist``
    argument). Prefer (a) wherever it is possible.

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
REACHABILITY_ALLOWLIST: tuple[str, ...] = (
    "spice.release",  # mounted command from [tool.spice.commands]
)

# Allowlist for production symbols that look test-only to static analysis but
# are reached dynamically in production (getattr/registry/string-key dispatch),
# which the AST scanner cannot see. Entries are either a dotted module path
# (exempts every symbol in that module) or a fully-qualified ``module.symbol``
# (exempts one function/class/``Class.method``). Empty by default: the clean
# repo scans to zero, so any entry is a declared, reviewed exception.
SYMBOL_REACHABILITY_ALLOWLIST: tuple[str, ...] = ()


@dataclass(frozen=True)
class ReachabilityFinding:
    module: str
    module_path: str
    only_test_imports: list[str]


@dataclass(frozen=True)
class SymbolReachabilityFinding:
    module: str
    module_path: str
    symbol: str
    kind: str
    only_test_imports: list[str]


@dataclass(frozen=True)
class _SymbolDefinition:
    module: str
    module_path: Path
    symbol: str
    kind: str


@dataclass(frozen=True)
class _SymbolRef:
    module: str
    symbol: str


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

    test_paths = list(test_dir.rglob("*.py"))
    test_reachable = _walk_imports(
        test_paths, pkg_root, package, include_root_modules=False
    )

    allowset = {*REACHABILITY_ALLOWLIST, *allowlist}
    findings: list[ReachabilityFinding] = []
    for module in sorted(test_reachable - prod_reachable):
        if module in allowset:
            continue
        mod_path = _module_to_path(module, pkg_root, package)
        if mod_path is None:
            continue
        importers = _find_importers(module, test_paths, pkg_root, package)
        findings.append(
            ReachabilityFinding(
                module=module,
                module_path=str(mod_path.relative_to(repo_root)),
                only_test_imports=sorted(importers),
            )
        )
    return findings


def scan_symbol_reachability(
    repo_root: Path,
    *,
    package: str = "spice",
    test_root: str = "tests",
    allowlist: Sequence[str] = SYMBOL_REACHABILITY_ALLOWLIST,
) -> list[SymbolReachabilityFinding]:
    """Return production-module symbols reachable from tests but not production."""
    pkg_root = repo_root / package
    test_dir = repo_root / test_root
    if not pkg_root.is_dir() or not test_dir.is_dir():
        return []

    root_paths = [repo_root / r for r in PRODUCTION_ROOTS if (repo_root / r).is_file()]
    prod_reachable = _walk_imports(root_paths, pkg_root, package)
    definitions = _collect_symbol_definitions(pkg_root, package, prod_reachable)
    prod_paths = [
        path
        for module in prod_reachable
        if (path := _module_to_path(module, pkg_root, package)) is not None
    ]
    test_paths = list(test_dir.rglob("*.py"))
    prod_refs, _prod_importers = _collect_symbol_refs(
        prod_paths, definitions, pkg_root=pkg_root, package=package
    )
    test_refs, test_importers = _collect_symbol_refs(
        test_paths, definitions, pkg_root=pkg_root, package=package
    )

    allowset = {*SYMBOL_REACHABILITY_ALLOWLIST, *allowlist}
    findings: list[SymbolReachabilityFinding] = []
    for ref in sorted(
        test_refs - prod_refs, key=lambda item: (item.module, item.symbol)
    ):
        definition = definitions.get(ref)
        if definition is None:
            continue
        if _symbol_is_allowed(ref, allowset):
            continue
        findings.append(
            SymbolReachabilityFinding(
                module=ref.module,
                module_path=str(definition.module_path.relative_to(repo_root)),
                symbol=ref.symbol,
                kind=definition.kind,
                only_test_imports=sorted(test_importers.get(ref, set())),
            )
        )
    return findings


def _symbol_is_allowed(ref: _SymbolRef, allowset: set[str]) -> bool:
    """A symbol is exempt if its module or its qualified name is allowlisted."""
    if ref.module in allowset:
        return True
    return f"{ref.module}.{ref.symbol}" in allowset


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


def render_symbol_reachability_board(
    findings: Sequence[SymbolReachabilityFinding],
    *,
    limit: int | None = None,
) -> list[str]:
    """Render a text board of test-only symbols in production modules."""
    rows: list[str] = []
    shown = list(findings)[:limit] if limit is not None else list(findings)
    if not shown:
        rows.append("symbol-reachability: no test-only symbols found")
        return rows
    rows.append(
        f"symbol-reachability: {len(findings)} test-only symbol(s)"
        + (f" (showing {len(shown)})" if limit and len(findings) > len(shown) else "")
    )
    for f in shown:
        rows.append(f"  {f.module_path}:{f.symbol}")
        rows.append(f"    symbol: {f.module}.{f.symbol} ({f.kind})")
        if f.only_test_imports:
            rows.append(f"    imported by: {', '.join(f.only_test_imports)}")
    return rows


def _walk_imports(
    roots: list[Path],
    pkg_root: Path,
    package: str,
    *,
    include_root_modules: bool = True,
) -> set[str]:
    """BFS import-graph walk; returns dotted package module names reachable."""
    visited: set[str] = set()
    queue: list[Path] = []
    for path in roots:
        mod = _path_to_module(path, pkg_root, package)
        if mod and include_root_modules and mod not in visited:
            visited.add(mod)
            queue.append(path)
        elif mod or not include_root_modules:
            queue.append(path)

    while queue:
        path = queue.pop()
        for imp in _direct_imports(path, pkg_root, package):
            if imp in visited:
                continue
            visited.add(imp)
            imp_path = _module_to_path(imp, pkg_root, package)
            if imp_path:
                queue.append(imp_path)
    return visited


def _direct_imports(path: Path, pkg_root: Path, package: str) -> list[str]:
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
            mod = _resolve_relative(path, node.module or "", node.level, package)
            if mod and (mod == package or mod.startswith(f"{package}.")):
                results.append(mod)
                for alias in node.names:
                    if alias.name == "*":
                        continue
                    candidate = f"{mod}.{alias.name}"
                    if _module_to_path(candidate, pkg_root, package):
                        results.append(candidate)
    return results


def _resolve_relative(
    source: Path, module: str, level: int, package: str
) -> str | None:
    """Resolve a relative import to its dotted module name."""
    if level == 0:
        return module
    parts = list(source.parent.parts)
    try:
        package_index = len(parts) - 1 - list(reversed(parts)).index(package)
    except ValueError:
        return None
    anchor = parts[package_index:]
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


def _find_importers(
    module: str, test_paths: list[Path], pkg_root: Path, package: str
) -> list[str]:
    """Return test file names that directly import module."""
    importers: list[str] = []
    for path in test_paths:
        imps = _direct_imports(path, pkg_root, package)
        if module in imps or any(imp.startswith(f"{module}.") for imp in imps):
            importers.append(path.name)
    return importers


def _collect_symbol_definitions(
    pkg_root: Path, package: str, modules: set[str]
) -> dict[_SymbolRef, _SymbolDefinition]:
    definitions: dict[_SymbolRef, _SymbolDefinition] = {}
    for module in modules:
        path = _module_to_path(module, pkg_root, package)
        if path is None:
            continue
        try:
            tree = ast.parse(path.read_bytes())
        except (SyntaxError, OSError):
            continue
        for node in tree.body:
            if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                ref = _SymbolRef(module, node.name)
                definitions[ref] = _SymbolDefinition(
                    module, path, node.name, "function"
                )
            elif isinstance(node, ast.ClassDef):
                ref = _SymbolRef(module, node.name)
                definitions[ref] = _SymbolDefinition(module, path, node.name, "class")
                for child in node.body:
                    if isinstance(child, ast.FunctionDef | ast.AsyncFunctionDef):
                        symbol = f"{node.name}.{child.name}"
                        method_ref = _SymbolRef(module, symbol)
                        definitions[method_ref] = _SymbolDefinition(
                            module, path, symbol, "method"
                        )
    return definitions


def _collect_symbol_refs(
    paths: list[Path],
    definitions: dict[_SymbolRef, _SymbolDefinition],
    *,
    pkg_root: Path,
    package: str,
) -> tuple[set[_SymbolRef], dict[_SymbolRef, set[str]]]:
    refs: set[_SymbolRef] = set()
    importers: dict[_SymbolRef, set[str]] = {}
    by_module: dict[str, set[str]] = {}
    class_symbols: set[tuple[str, str]] = set()
    for ref, definition in definitions.items():
        by_module.setdefault(ref.module, set()).add(ref.symbol)
        if definition.kind == "class":
            class_symbols.add((ref.module, ref.symbol))

    for path in paths:
        try:
            tree = ast.parse(path.read_bytes())
        except (SyntaxError, OSError):
            continue
        current_module = _path_to_module(path, pkg_root, package)
        path_refs = _symbol_refs_for_tree(
            tree,
            definitions,
            by_module,
            class_symbols,
            path,
            package,
            current_module,
        )
        refs.update(path_refs)
        display = path.name
        for ref in path_refs:
            importers.setdefault(ref, set()).add(display)
    return refs, importers


def _symbol_refs_for_tree(
    tree: ast.AST,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    by_module: dict[str, set[str]],
    class_symbols: set[tuple[str, str]],
    path: Path,
    package: str,
    current_module: str | None,
) -> set[_SymbolRef]:
    refs: set[_SymbolRef] = set()
    symbol_aliases: dict[str, _SymbolRef] = {}
    module_aliases: dict[str, str] = {}
    class_aliases: dict[str, tuple[str, str]] = {}
    instance_aliases: dict[str, tuple[str, str]] = {}
    external_base_aliases: set[str] = set()

    _seed_local_symbol_aliases(
        current_module,
        definitions,
        by_module,
        class_symbols,
        symbol_aliases,
        class_aliases,
    )
    _collect_import_symbol_refs(
        tree,
        definitions,
        by_module,
        class_symbols,
        path,
        package,
        refs,
        symbol_aliases,
        module_aliases,
        class_aliases,
        external_base_aliases,
    )
    _collect_usage_symbol_refs(
        tree,
        definitions,
        by_module,
        refs,
        symbol_aliases,
        module_aliases,
        class_aliases,
        instance_aliases,
    )
    if current_module is not None:
        refs.update(_refs_from_local_class_methods(tree, definitions, current_module))
        refs.update(
            _refs_from_external_override_methods(
                tree, definitions, current_module, external_base_aliases
            )
        )
    return refs


def _seed_local_symbol_aliases(
    current_module: str | None,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    by_module: dict[str, set[str]],
    class_symbols: set[tuple[str, str]],
    symbol_aliases: dict[str, _SymbolRef],
    class_aliases: dict[str, tuple[str, str]],
) -> None:
    if current_module is None:
        return
    for symbol in by_module.get(current_module, set()):
        if "." in symbol:
            continue
        ref = _SymbolRef(current_module, symbol)
        if ref not in definitions:
            continue
        symbol_aliases[symbol] = ref
        if (current_module, symbol) in class_symbols:
            class_aliases[symbol] = (current_module, symbol)


def _collect_import_symbol_refs(
    tree: ast.AST,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    by_module: dict[str, set[str]],
    class_symbols: set[tuple[str, str]],
    path: Path,
    package: str,
    refs: set[_SymbolRef],
    symbol_aliases: dict[str, _SymbolRef],
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    external_base_aliases: set[str],
) -> None:
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            _collect_import_aliases(
                node, by_module, package, module_aliases, external_base_aliases
            )
        elif isinstance(node, ast.ImportFrom):
            _collect_import_from_aliases(
                node,
                definitions,
                by_module,
                class_symbols,
                path,
                package,
                refs,
                symbol_aliases,
                module_aliases,
                class_aliases,
                external_base_aliases,
            )


def _collect_import_aliases(
    node: ast.Import,
    by_module: dict[str, set[str]],
    package: str,
    module_aliases: dict[str, str],
    external_base_aliases: set[str],
) -> None:
    for alias in node.names:
        asname = alias.asname or alias.name.split(".")[0]
        if alias.name in by_module:
            module_aliases[asname] = alias.name
        elif not alias.name.startswith(f"{package}."):
            external_base_aliases.add(asname)


def _collect_import_from_aliases(
    node: ast.ImportFrom,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    by_module: dict[str, set[str]],
    class_symbols: set[tuple[str, str]],
    path: Path,
    package: str,
    refs: set[_SymbolRef],
    symbol_aliases: dict[str, _SymbolRef],
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    external_base_aliases: set[str],
) -> None:
    module = _resolve_relative(path, node.module or "", node.level, package)
    if not module:
        return
    for alias in node.names:
        if alias.name == "*":
            continue
        asname = alias.asname or alias.name
        candidate_module = f"{module}.{alias.name}"
        if candidate_module in by_module:
            module_aliases[asname] = candidate_module
            continue
        ref = _SymbolRef(module, alias.name)
        if ref in definitions:
            refs.add(ref)
            symbol_aliases[asname] = ref
            if (ref.module, ref.symbol) in class_symbols:
                class_aliases[asname] = (ref.module, ref.symbol)
            continue
        if not (module == package or module.startswith(f"{package}.")):
            external_base_aliases.add(asname)


def _collect_usage_symbol_refs(
    tree: ast.AST,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    by_module: dict[str, set[str]],
    refs: set[_SymbolRef],
    symbol_aliases: dict[str, _SymbolRef],
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
) -> None:
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign):
            _collect_assignment_aliases(
                node, module_aliases, class_aliases, instance_aliases
            )
        elif isinstance(node, ast.Name) and node.id in symbol_aliases:
            refs.add(symbol_aliases[node.id])
        elif isinstance(node, ast.Attribute):
            refs.update(
                _refs_from_attribute(
                    node,
                    definitions,
                    by_module,
                    module_aliases,
                    class_aliases,
                    instance_aliases,
                )
            )


def _collect_assignment_aliases(
    node: ast.Assign,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
) -> None:
    class_ref = _class_ref_from_expr(node.value, module_aliases, class_aliases)
    if class_ref is None:
        return
    for target in node.targets:
        if isinstance(target, ast.Name):
            instance_aliases[target.id] = class_ref


def _refs_from_local_class_methods(
    tree: ast.AST,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    current_module: str,
) -> set[_SymbolRef]:
    refs: set[_SymbolRef] = set()
    if not isinstance(tree, ast.Module):
        return refs
    for class_node in tree.body:
        if not isinstance(class_node, ast.ClassDef):
            continue
        class_ref = _SymbolRef(current_module, class_node.name)
        if class_ref not in definitions:
            continue
        for child in class_node.body:
            if not isinstance(child, ast.FunctionDef | ast.AsyncFunctionDef):
                continue
            receiver_names = _method_receiver_names(child)
            if not receiver_names:
                continue
            for node in ast.walk(child):
                if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
                    if node.value.id not in receiver_names:
                        continue
                    ref = _SymbolRef(current_module, f"{class_node.name}.{node.attr}")
                    if ref in definitions:
                        refs.add(ref)
    return refs


def _refs_from_external_override_methods(
    tree: ast.AST,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    current_module: str,
    external_base_aliases: set[str],
) -> set[_SymbolRef]:
    override_refs: set[_SymbolRef] = set()
    if not isinstance(tree, ast.Module):
        return override_refs
    for class_node in tree.body:
        if not isinstance(class_node, ast.ClassDef):
            continue
        if not _class_uses_external_base(class_node, external_base_aliases):
            continue
        for child in class_node.body:
            if not isinstance(child, ast.FunctionDef | ast.AsyncFunctionDef):
                continue
            method_ref = _SymbolRef(current_module, f"{class_node.name}.{child.name}")
            if method_ref in definitions:
                override_refs.add(method_ref)
    return override_refs


def _class_uses_external_base(
    node: ast.ClassDef, external_base_aliases: set[str]
) -> bool:
    for base in node.bases:
        if isinstance(base, ast.Name) and base.id in external_base_aliases:
            return True
    return False


def _method_receiver_names(node: ast.FunctionDef | ast.AsyncFunctionDef) -> set[str]:
    positional = [*node.args.posonlyargs, *node.args.args]
    if not positional:
        return set()
    return {positional[0].arg}


def _refs_from_attribute(
    node: ast.Attribute,
    definitions: dict[_SymbolRef, _SymbolDefinition],
    by_module: dict[str, set[str]],
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
) -> set[_SymbolRef]:
    refs: set[_SymbolRef] = set()
    if isinstance(node.value, ast.Name):
        module = module_aliases.get(node.value.id)
        if module is not None:
            ref = _SymbolRef(module, node.attr)
            if ref in definitions:
                refs.add(ref)
        class_ref = class_aliases.get(node.value.id) or instance_aliases.get(
            node.value.id
        )
        if class_ref is not None:
            ref = _SymbolRef(class_ref[0], f"{class_ref[1]}.{node.attr}")
            if ref in definitions:
                refs.add(ref)
    elif isinstance(node.value, ast.Call):
        class_ref = _class_ref_from_expr(node.value.func, module_aliases, class_aliases)
        if class_ref is not None:
            ref = _SymbolRef(class_ref[0], f"{class_ref[1]}.{node.attr}")
            if ref in definitions:
                refs.add(ref)

    chain = _attribute_chain(node)
    if chain:
        refs.update(_refs_from_chain(chain, definitions, by_module, module_aliases))
    return refs


def _refs_from_chain(
    chain: list[str],
    definitions: dict[_SymbolRef, _SymbolDefinition],
    by_module: dict[str, set[str]],
    module_aliases: dict[str, str],
) -> set[_SymbolRef]:
    refs: set[_SymbolRef] = set()
    for prefix_len in range(1, len(chain)):
        prefix = ".".join(chain[:prefix_len])
        module = module_aliases.get(prefix) or prefix
        if module not in by_module:
            continue
        tail = chain[prefix_len:]
        if not tail:
            continue
        symbol = tail[0]
        ref = _SymbolRef(module, symbol)
        if ref in definitions:
            refs.add(ref)
        if len(tail) >= 2:
            method_ref = _SymbolRef(module, f"{symbol}.{tail[1]}")
            if method_ref in definitions:
                refs.add(method_ref)
    return refs


def _class_ref_from_expr(
    node: ast.AST,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
) -> tuple[str, str] | None:
    if isinstance(node, ast.Name):
        return class_aliases.get(node.id)
    if isinstance(node, ast.Attribute):
        if isinstance(node.value, ast.Name):
            module = module_aliases.get(node.value.id)
            if module is not None:
                return (module, node.attr)
        chain = _attribute_chain(node)
        if chain and len(chain) >= 2:
            module = ".".join(chain[:-1])
            return (module, chain[-1])
    return None


def _attribute_chain(node: ast.AST) -> list[str] | None:
    parts: list[str] = []
    current = node
    while isinstance(current, ast.Attribute):
        parts.append(current.attr)
        current = current.value
    if isinstance(current, ast.Name):
        parts.append(current.id)
        return list(reversed(parts))
    return None
