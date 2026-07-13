"""Python import-graph and symbol-reference analysis for reachability studies."""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class _SymbolDefinition:
    module: str
    module_path: Path
    symbol: str
    kind: str
    return_class: tuple[str, str] | None = None


@dataclass(frozen=True)
class _SymbolRef:
    module: str
    symbol: str


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
        local_classes = {
            node.name for node in tree.body if isinstance(node, ast.ClassDef)
        }
        for node in tree.body:
            if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                ref = _SymbolRef(module, node.name)
                definitions[ref] = _SymbolDefinition(
                    module,
                    path,
                    node.name,
                    "function",
                    _local_return_class(node.returns, module, local_classes),
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


def _local_return_class(
    annotation: ast.AST | None, module: str, local_classes: set[str]
) -> tuple[str, str] | None:
    if isinstance(annotation, ast.Name) and annotation.id in local_classes:
        return (module, annotation.id)
    return None


def _collect_symbol_refs(
    paths: list[Path],
    definitions: dict[_SymbolRef, _SymbolDefinition],
    *,
    pkg_root: Path,
    package: str,
    enhanced_aliases: bool,
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
            enhanced_aliases=enhanced_aliases,
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
    *,
    enhanced_aliases: bool,
) -> set[_SymbolRef]:
    refs: set[_SymbolRef] = set()
    symbol_aliases: dict[str, _SymbolRef] = {}
    module_aliases: dict[str, str] = {}
    class_aliases: dict[str, tuple[str, str]] = {}
    instance_aliases: dict[str, tuple[str, str]] = {}
    call_result_aliases: dict[str, tuple[str, str]] = {}
    external_base_aliases: set[str] = set()

    _seed_local_symbol_aliases(
        current_module,
        definitions,
        by_module,
        class_symbols,
        symbol_aliases,
        class_aliases,
        call_result_aliases,
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
        call_result_aliases,
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
        call_result_aliases,
        enhanced_aliases=enhanced_aliases,
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
    call_result_aliases: dict[str, tuple[str, str]],
) -> None:
    if current_module is None:
        return
    for symbol in by_module.get(current_module, set()):
        if "." in symbol:
            continue
        ref = _SymbolRef(current_module, symbol)
        if ref not in definitions:
            continue
        definition = definitions[ref]
        symbol_aliases[symbol] = ref
        if definition.return_class is not None:
            call_result_aliases[symbol] = definition.return_class
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
    call_result_aliases: dict[str, tuple[str, str]],
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
                call_result_aliases,
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
    call_result_aliases: dict[str, tuple[str, str]],
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
        definition = definitions.get(ref)
        if definition is not None:
            refs.add(ref)
            symbol_aliases[asname] = ref
            if definition.return_class is not None:
                call_result_aliases[asname] = definition.return_class
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
    call_result_aliases: dict[str, tuple[str, str]],
    *,
    enhanced_aliases: bool,
) -> None:
    _collect_usage_aliases(
        tree,
        module_aliases,
        class_aliases,
        instance_aliases,
        call_result_aliases,
        enhanced_aliases=enhanced_aliases,
    )
    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and node.id in symbol_aliases:
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
                    call_result_aliases,
                )
            )


def _collect_usage_aliases(
    tree: ast.AST,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
    call_result_aliases: dict[str, tuple[str, str]],
    *,
    enhanced_aliases: bool,
) -> None:
    if not enhanced_aliases:
        _collect_legacy_assignment_aliases(
            tree, module_aliases, class_aliases, instance_aliases
        )
        return
    changed = True
    while changed:
        changed = False
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                changed = (
                    _collect_assignment_aliases(
                        node,
                        module_aliases,
                        class_aliases,
                        instance_aliases,
                        call_result_aliases,
                    )
                    or changed
                )
            elif isinstance(node, ast.AnnAssign):
                changed = (
                    _collect_annotated_assignment_alias(
                        node,
                        module_aliases,
                        class_aliases,
                        instance_aliases,
                        call_result_aliases,
                    )
                    or changed
                )
            elif isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                changed = (
                    _collect_parameter_annotation_aliases(
                        node, module_aliases, class_aliases, instance_aliases
                    )
                    or changed
                )


def _collect_legacy_assignment_aliases(
    tree: ast.AST,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
) -> None:
    for node in ast.walk(tree):
        if not isinstance(node, ast.Assign):
            continue
        class_ref = _class_ref_from_expr(node.value, module_aliases, class_aliases)
        if class_ref is None:
            continue
        for target in node.targets:
            if isinstance(target, ast.Name):
                instance_aliases[target.id] = class_ref


def _collect_assignment_aliases(
    node: ast.Assign,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
    call_result_aliases: dict[str, tuple[str, str]],
) -> bool:
    class_ref = _class_ref_from_assignment_value(
        node.value, module_aliases, class_aliases, instance_aliases, call_result_aliases
    )
    if class_ref is None:
        return False
    changed = False
    for target in node.targets:
        if alias_key := _instance_alias_key(target):
            changed = (
                _set_instance_alias(instance_aliases, alias_key, class_ref) or changed
            )
    return changed


def _collect_annotated_assignment_alias(
    node: ast.AnnAssign,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
    call_result_aliases: dict[str, tuple[str, str]],
) -> bool:
    alias_key = _instance_alias_key(node.target)
    if alias_key is None:
        return False
    class_ref = _class_ref_from_annotation(
        node.annotation, module_aliases, class_aliases
    )
    if class_ref is None and node.value is not None:
        class_ref = _class_ref_from_assignment_value(
            node.value,
            module_aliases,
            class_aliases,
            instance_aliases,
            call_result_aliases,
        )
    if class_ref is not None:
        return _set_instance_alias(instance_aliases, alias_key, class_ref)
    return False


def _collect_parameter_annotation_aliases(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
) -> bool:
    changed = False
    args = [
        *node.args.posonlyargs,
        *node.args.args,
        *node.args.kwonlyargs,
    ]
    if node.args.vararg is not None:
        args.append(node.args.vararg)
    if node.args.kwarg is not None:
        args.append(node.args.kwarg)
    for arg in args:
        if arg.annotation is None:
            continue
        class_ref = _class_ref_from_annotation(
            arg.annotation, module_aliases, class_aliases
        )
        if class_ref is not None:
            changed = (
                _set_instance_alias(instance_aliases, arg.arg, class_ref) or changed
            )
    return changed


def _set_instance_alias(
    instance_aliases: dict[str, tuple[str, str]],
    alias_key: str,
    class_ref: tuple[str, str],
) -> bool:
    existing = instance_aliases.get(alias_key)
    if existing is not None:
        return False
    instance_aliases[alias_key] = class_ref
    return True


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
    call_result_aliases: dict[str, tuple[str, str]],
) -> set[_SymbolRef]:
    refs: set[_SymbolRef] = set()
    if isinstance(node.value, ast.Name):
        module = module_aliases.get(node.value.id)
        if module is not None:
            ref = _SymbolRef(module, node.attr)
            if ref in definitions:
                refs.add(ref)
        class_ref = class_aliases.get(node.value.id)
        if class_ref is None:
            class_ref = _class_ref_from_instance_expr(node.value, instance_aliases)
        if class_ref is not None:
            ref = _SymbolRef(class_ref[0], f"{class_ref[1]}.{node.attr}")
            if ref in definitions:
                refs.add(ref)
    elif isinstance(node.value, ast.Attribute):
        class_ref = _class_ref_from_instance_expr(node.value, instance_aliases)
        if class_ref is not None:
            ref = _SymbolRef(class_ref[0], f"{class_ref[1]}.{node.attr}")
            if ref in definitions:
                refs.add(ref)
    elif isinstance(node.value, ast.Call):
        class_ref = _class_ref_from_call(
            node.value, module_aliases, class_aliases, call_result_aliases
        )
        if class_ref is not None:
            ref = _SymbolRef(class_ref[0], f"{class_ref[1]}.{node.attr}")
            if ref in definitions:
                refs.add(ref)

    chain = _attribute_chain(node)
    if chain:
        refs.update(_refs_from_chain(chain, definitions, by_module, module_aliases))
    return refs


def _class_ref_from_assignment_value(
    node: ast.AST,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    instance_aliases: dict[str, tuple[str, str]],
    call_result_aliases: dict[str, tuple[str, str]],
) -> tuple[str, str] | None:
    if isinstance(node, ast.Call):
        return _class_ref_from_call(
            node, module_aliases, class_aliases, call_result_aliases
        )
    class_ref = _class_ref_from_expr(node, module_aliases, class_aliases)
    if class_ref is not None:
        return class_ref
    return _class_ref_from_instance_expr(node, instance_aliases)


def _class_ref_from_call(
    node: ast.Call,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
    call_result_aliases: dict[str, tuple[str, str]],
) -> tuple[str, str] | None:
    return _class_ref_from_expr(
        node.func, module_aliases, class_aliases
    ) or _class_ref_from_call_result(node.func, call_result_aliases)


def _class_ref_from_call_result(
    node: ast.AST, call_result_aliases: dict[str, tuple[str, str]]
) -> tuple[str, str] | None:
    if alias_key := _instance_alias_key(node):
        return call_result_aliases.get(alias_key)
    return None


def _class_ref_from_instance_expr(
    node: ast.AST, instance_aliases: dict[str, tuple[str, str]]
) -> tuple[str, str] | None:
    if alias_key := _instance_alias_key(node):
        return instance_aliases.get(alias_key)
    return None


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


def _class_ref_from_annotation(
    node: ast.AST,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
) -> tuple[str, str] | None:
    if class_ref := _class_ref_from_expr(node, module_aliases, class_aliases):
        return class_ref
    if isinstance(node, ast.Subscript):
        wrapper = _annotation_wrapper_name(node.value)
        if wrapper in {"Optional", "Annotated"}:
            return _class_ref_from_annotation(node.slice, module_aliases, class_aliases)
        if wrapper == "Union":
            return _class_ref_from_union_members(
                node.slice, module_aliases, class_aliases
            )
        return None
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        return _class_ref_from_annotation(
            node.left, module_aliases, class_aliases
        ) or _class_ref_from_annotation(node.right, module_aliases, class_aliases)
    if isinstance(node, ast.Tuple):
        for item in node.elts:
            if class_ref := _class_ref_from_annotation(
                item, module_aliases, class_aliases
            ):
                return class_ref
    return None


def _class_ref_from_union_members(
    node: ast.AST,
    module_aliases: dict[str, str],
    class_aliases: dict[str, tuple[str, str]],
) -> tuple[str, str] | None:
    if isinstance(node, ast.Tuple):
        for item in node.elts:
            if class_ref := _class_ref_from_annotation(
                item, module_aliases, class_aliases
            ):
                return class_ref
        return None
    return _class_ref_from_annotation(node, module_aliases, class_aliases)


def _annotation_wrapper_name(node: ast.AST) -> str:
    if isinstance(node, ast.Name):
        return node.id
    chain = _attribute_chain(node)
    return chain[-1] if chain else ""


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


def _instance_alias_key(node: ast.AST) -> str | None:
    if isinstance(node, ast.Name):
        return node.id
    chain = _attribute_chain(node)
    return ".".join(chain) if chain else None
