"""Differential reachability: identify code only tests reach.

``scan_reachability`` dispatches through a provider registry. The built-in
``python`` provider walks the import graph from production entry points (cli,
serve, hooks, agent loop) to build the production-reachable module set, then
diffs that against imports from the test suite. Configured providers report
language-native findings through the same normalized board: provider, kind,
subject, path, and test importers.

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
import json
import os
import shlex
import subprocess
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Any, Callable, Sequence

from spice.errors import SpiceError
from spice.repocfg import policy_table
from spice.studies.walk import configured_test_roots


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

PYTHON_PROVIDER = "python"
REACHABILITY_PROVIDERS_KEY = "reachability_providers"
STAGED_PATHS_ENV = "SPICE_STAGED_PATHS"  # env-policy: allow

# Both reachability gates share one provider seam; a finding's ``kind`` routes it
# to exactly one gate by granularity. ``module`` is the coarse, whole-file gate;
# every other kind (function, class, method, ...) is a symbol and rides the
# finer symbol-reachability gate. No finding is counted by both.
MODULE_KIND = "module"


@dataclass(frozen=True)
class ReachabilityFinding:
    subject: str
    path: str
    only_test_imports: list[str]
    provider: str = PYTHON_PROVIDER
    kind: str = MODULE_KIND


@dataclass(frozen=True)
class SymbolReachabilityFinding:
    module: str
    module_path: str
    symbol: str
    kind: str
    only_test_imports: list[str]
    provider: str = PYTHON_PROVIDER


@dataclass(frozen=True)
class ReachabilityScanRequest:
    repo_root: Path
    package: str
    test_roots: tuple[Path, ...]
    allowlist: tuple[str, ...]


@dataclass(frozen=True)
class ReachabilityProvider:
    name: str
    scan: Callable[[ReachabilityScanRequest], list[ReachabilityFinding]]


@dataclass(frozen=True)
class _CommandReachabilityProvider:
    name: str
    argv: tuple[str, ...]
    staged_paths: tuple[Path, ...]


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


def scan_reachability(
    repo_root: Path,
    *,
    package: str = "spice",
    allowlist: Sequence[str] = REACHABILITY_ALLOWLIST,
    staged_paths: Sequence[Path] | None = None,
    providers: Sequence[ReachabilityProvider] | None = None,
) -> list[ReachabilityFinding]:
    """Return code reachable from tests but not from production roots."""
    request = ReachabilityScanRequest(
        repo_root=repo_root,
        package=package,
        test_roots=tuple(configured_test_roots(repo_root)),
        allowlist=tuple(allowlist),
    )
    active_providers = (
        list(providers)
        if providers is not None
        else reachability_provider_registry(repo_root, staged_paths=staged_paths)
    )
    findings: list[ReachabilityFinding] = []
    for provider in active_providers:
        provider_name = _provider_name(provider.name)
        findings.extend(
            _provider_named_finding(provider_name, finding)
            for finding in provider.scan(request)
            if finding.kind == MODULE_KIND
        )
    return sorted(findings, key=lambda f: (f.provider, f.path, f.kind, f.subject))


def reachability_provider_registry(
    repo_root: Path, *, staged_paths: Sequence[Path] | None = None
) -> list[ReachabilityProvider]:
    """Return the built-in Python provider plus configured command providers."""
    return [
        ReachabilityProvider(PYTHON_PROVIDER, _scan_python_reachability),
        *_configured_reachability_providers(repo_root, staged_paths=staged_paths),
    ]


def _scan_python_reachability(
    request: ReachabilityScanRequest,
) -> list[ReachabilityFinding]:
    """Return Python modules reachable from tests but not production roots."""
    repo_root = request.repo_root
    package = request.package
    test_roots = request.test_roots
    pkg_root = repo_root / package
    if not pkg_root.is_dir() or not test_roots:
        return []

    root_paths = [repo_root / r for r in PRODUCTION_ROOTS if (repo_root / r).is_file()]
    prod_reachable = _walk_imports(root_paths, pkg_root, package)

    test_paths = _python_test_paths(test_roots)
    test_reachable = _walk_imports(
        test_paths, pkg_root, package, include_root_modules=False
    )

    allowset = {*REACHABILITY_ALLOWLIST, *request.allowlist}
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
                subject=module,
                path=str(mod_path.relative_to(repo_root)),
                only_test_imports=sorted(importers),
            )
        )
    return findings


def _configured_reachability_providers(
    repo_root: Path, *, staged_paths: Sequence[Path] | None
) -> list[ReachabilityProvider]:
    raw_providers = policy_table(repo_root).get(REACHABILITY_PROVIDERS_KEY)
    if raw_providers is None:
        return []
    if not isinstance(raw_providers, list):
        raise SpiceError(
            f"[tool.spice.policy] {REACHABILITY_PROVIDERS_KEY} must be a list"
        )

    normalized_staged = _relative_staged_paths(repo_root, staged_paths)
    providers: list[ReachabilityProvider] = []
    seen_names = {PYTHON_PROVIDER}
    for index, raw in enumerate(raw_providers, start=1):
        context = f"{REACHABILITY_PROVIDERS_KEY}[{index}]"
        if not isinstance(raw, dict):
            raise SpiceError(f"[tool.spice.policy] {context} must be a provider table")
        command = _command_provider_from_table(raw, context=context)
        if command.name in seen_names:
            raise SpiceError(
                f"[tool.spice.policy] {context}: duplicate reachability provider "
                f"name {command.name!r}"
            )
        seen_names.add(command.name)
        when = _when_patterns_from_table(raw, context=context)
        provider_paths = _provider_staged_paths(normalized_staged, when)
        if provider_paths is None:
            continue
        command_provider = _CommandReachabilityProvider(
            name=command.name,
            argv=command.argv,
            staged_paths=provider_paths,
        )
        providers.append(_command_reachability_provider(command_provider))
    return providers


def _command_reachability_provider(
    command_provider: _CommandReachabilityProvider,
) -> ReachabilityProvider:
    def scan(request: ReachabilityScanRequest) -> list[ReachabilityFinding]:
        return _scan_command_reachability_provider(command_provider, request)

    return ReachabilityProvider(command_provider.name, scan)


def _command_provider_from_table(
    raw: dict[str, Any], *, context: str
) -> _CommandReachabilityProvider:
    return _CommandReachabilityProvider(
        name=_provider_name_from_table(raw, context=context),
        argv=_provider_run_argv(raw.get("run"), context=context),
        staged_paths=(),
    )


def _provider_name_from_table(raw: dict[str, Any], *, context: str) -> str:
    name = raw.get("name")
    if not isinstance(name, str) or not name.strip():
        raise SpiceError(f"{context}: name must be a non-empty string")
    return _provider_name(name)


def _provider_run_argv(raw: Any, *, context: str) -> tuple[str, ...]:
    if not isinstance(raw, list) or not all(isinstance(item, str) for item in raw):
        raise SpiceError(f"{context}: run must be a non-empty argv list")
    argv = tuple(item for item in raw if item)
    if len(argv) != len(raw) or not argv:
        raise SpiceError(f"{context}: run must be a non-empty argv list")
    return argv


def _when_patterns_from_table(raw: dict[str, Any], *, context: str) -> tuple[str, ...]:
    if "when" not in raw:
        return ()
    when = raw["when"]
    if not isinstance(when, list):
        raise SpiceError(f"{context}: when must be a non-empty glob list")
    patterns = tuple(
        item.strip() for item in when if isinstance(item, str) and item.strip()
    )
    if len(patterns) != len(when) or not patterns:
        raise SpiceError(f"{context}: when must be a non-empty glob list")
    return patterns


def _relative_staged_paths(
    repo_root: Path, staged_paths: Sequence[Path] | None
) -> tuple[Path, ...] | None:
    if staged_paths is None:
        return None
    paths: list[Path] = []
    for path in staged_paths:
        try:
            paths.append(path.relative_to(repo_root) if path.is_absolute() else path)
        except ValueError as exc:
            raise SpiceError(
                f"reachability staged path is outside repo: {path}"
            ) from exc
    return tuple(paths)


def _provider_staged_paths(
    staged_paths: tuple[Path, ...] | None, when: tuple[str, ...]
) -> tuple[Path, ...] | None:
    if staged_paths is None:
        return ()
    if not when:
        return staged_paths
    matches = tuple(
        path
        for path in staged_paths
        if any(_path_matches_when(path, pattern) for pattern in when)
    )
    return matches or None


def _path_matches_when(path: Path, pattern: str) -> bool:
    normalized_path = path.as_posix().strip().removeprefix("./")
    normalized_pattern = pattern.strip().replace("\\", "/").removeprefix("./")
    return fnmatchcase(normalized_path, normalized_pattern)


def _scan_command_reachability_provider(
    provider: _CommandReachabilityProvider, request: ReachabilityScanRequest
) -> list[ReachabilityFinding]:
    env = os.environ.copy()  # env-policy: allow
    env[STAGED_PATHS_ENV] = "\n".join(path.as_posix() for path in provider.staged_paths)
    result = subprocess.run(
        list(provider.argv),
        capture_output=True,
        env=env,
        text=True,
        cwd=request.repo_root,
        check=False,
    )
    if result.returncode != 0:
        output = "\n".join(
            part for part in (result.stdout.strip(), result.stderr.strip()) if part
        )
        message = (
            f"reachability provider {provider.name!r}: "
            f"{shlex.join(provider.argv)} exited {result.returncode}"
        )
        if output:
            message += ":\n" + output
        raise SpiceError(message)
    return _parse_provider_findings(provider.name, result.stdout)


def _parse_provider_findings(
    provider_name: str, stdout: str
) -> list[ReachabilityFinding]:
    payload = stdout.strip()
    if not payload:
        return []
    try:
        raw_findings = json.loads(payload)
    except json.JSONDecodeError as exc:
        raise SpiceError(
            f"reachability provider {provider_name!r} emitted invalid JSON: {exc.msg}"
        ) from exc
    if not isinstance(raw_findings, list):
        raise SpiceError(
            f"reachability provider {provider_name!r} must emit a JSON list"
        )
    return [
        _provider_finding_from_json(provider_name, raw, index)
        for index, raw in enumerate(raw_findings, start=1)
    ]


def _provider_finding_from_json(
    provider_name: str, raw: Any, index: int
) -> ReachabilityFinding:
    context = f"reachability provider {provider_name!r} finding {index}"
    if not isinstance(raw, dict):
        raise SpiceError(f"{context} must be a JSON object")
    return ReachabilityFinding(
        provider=provider_name,
        kind=_required_json_string(raw, "kind", context),
        subject=_required_json_string(raw, "subject", context),
        path=_required_json_string(raw, "path", context),
        only_test_imports=_required_json_string_list(raw, "imported_by", context),
    )


def _required_json_string(raw: dict[str, Any], field: str, context: str) -> str:
    value = raw.get(field)
    if not isinstance(value, str) or not value.strip():
        raise SpiceError(f"{context}: {field} must be a non-empty string")
    return value.strip()


def _required_json_string_list(
    raw: dict[str, Any], field: str, context: str
) -> list[str]:
    value = raw.get(field)
    if not isinstance(value, list) or not all(
        isinstance(item, str) and item.strip() for item in value
    ):
        raise SpiceError(f"{context}: {field} must be a string list")
    return [item.strip() for item in value]


def _provider_name(raw: str) -> str:
    name = raw.strip()
    if not name:
        raise SpiceError("reachability provider name must be non-empty")
    return name


def _provider_named_finding(
    provider_name: str, finding: ReachabilityFinding
) -> ReachabilityFinding:
    return ReachabilityFinding(
        provider=provider_name,
        kind=finding.kind,
        subject=finding.subject,
        path=finding.path,
        only_test_imports=finding.only_test_imports,
    )


def scan_symbol_reachability(
    repo_root: Path,
    *,
    package: str = "spice",
    allowlist: Sequence[str] = SYMBOL_REACHABILITY_ALLOWLIST,
    staged_paths: Sequence[Path] | None = None,
    providers: Sequence[ReachabilityProvider] | None = None,
) -> list[SymbolReachabilityFinding]:
    """Return test-only symbols, polyglot through the shared provider seam.

    The built-in Python AST scan supplies Python symbols; configured providers
    contribute their symbol (non-``module``) findings via the same
    ``reachability_providers`` registry the module gate uses. A provider's
    ``module`` findings belong to the coarse reachability gate and are excluded
    here, so no finding is gated twice.
    """
    findings = _scan_python_symbol_reachability(
        repo_root,
        package=package,
        test_roots=tuple(configured_test_roots(repo_root)),
        allowlist=allowlist,
    )
    request = ReachabilityScanRequest(
        repo_root=repo_root,
        package=package,
        test_roots=tuple(configured_test_roots(repo_root)),
        allowlist=tuple(allowlist),
    )
    active_providers = (
        list(providers)
        if providers is not None
        else reachability_provider_registry(repo_root, staged_paths=staged_paths)
    )
    for provider in active_providers:
        if provider.name == PYTHON_PROVIDER:
            continue
        provider_name = _provider_name(provider.name)
        findings.extend(
            _symbol_finding_from_reachability(provider_name, finding)
            for finding in provider.scan(request)
            if finding.kind != MODULE_KIND
        )
    return sorted(findings, key=lambda f: (f.provider, f.module, f.symbol, f.kind))


def _symbol_finding_from_reachability(
    provider_name: str, finding: ReachabilityFinding
) -> SymbolReachabilityFinding:
    """Normalize a provider's symbol finding onto the symbol-reachability board.

    The fully-qualified ``subject`` (e.g. ``Game.Enemy.UnusedTick``) splits into
    a module and a leaf symbol; a bare subject keeps an empty module.
    """
    module, _, symbol = finding.subject.rpartition(".")
    return SymbolReachabilityFinding(
        module=module,
        module_path=finding.path,
        symbol=symbol or finding.subject,
        kind=finding.kind,
        only_test_imports=list(finding.only_test_imports),
        provider=provider_name,
    )


def _scan_python_symbol_reachability(
    repo_root: Path,
    *,
    package: str,
    test_roots: tuple[Path, ...],
    allowlist: Sequence[str],
) -> list[SymbolReachabilityFinding]:
    """Return production-module symbols reachable from tests but not production."""
    pkg_root = repo_root / package
    if not pkg_root.is_dir() or not test_roots:
        return []

    root_paths = [repo_root / r for r in PRODUCTION_ROOTS if (repo_root / r).is_file()]
    prod_reachable = _walk_imports(root_paths, pkg_root, package)
    definitions = _collect_symbol_definitions(pkg_root, package, prod_reachable)
    prod_paths = [
        path
        for module in prod_reachable
        if (path := _module_to_path(module, pkg_root, package)) is not None
    ]
    test_paths = _python_test_paths(test_roots)
    prod_refs, _prod_importers = _collect_symbol_refs(
        prod_paths,
        definitions,
        pkg_root=pkg_root,
        package=package,
        enhanced_aliases=True,
    )
    test_refs, test_importers = _collect_symbol_refs(
        test_paths,
        definitions,
        pkg_root=pkg_root,
        package=package,
        enhanced_aliases=False,
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


def _python_test_paths(test_roots: tuple[Path, ...]) -> list[Path]:
    return sorted(path for test_root in test_roots for path in test_root.rglob("*.py"))


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
    """Render a text board of test-only reachability findings."""
    rows: list[str] = []
    shown = list(findings)[:limit] if limit is not None else list(findings)
    if not shown:
        rows.append("reachability: no test-only findings found")
        return rows
    rows.append(
        f"reachability: {len(findings)} test-only finding(s)"
        + (f" (showing {len(shown)})" if limit and len(findings) > len(shown) else "")
    )
    for f in shown:
        rows.append(f"  {f.path}")
        rows.append(f"    provider: {f.provider}")
        rows.append(f"    kind: {f.kind}")
        rows.append(f"    subject: {f.subject}")
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
        rows.append(f"    provider: {f.provider}")
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
