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

"""

from __future__ import annotations

import json
import os
import shlex
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Any, Callable, Sequence

from spice.errors import SpiceError
from spice.toolprocess import run_tool_command
from spice.configlayer import effective_table
from spice.studies.reachabilitypython import (
    _SymbolRef,
    _collect_symbol_definitions,
    _collect_symbol_refs,
    _find_importers,
    _module_to_path,
    _walk_imports,
)
from spice.studies.walk import configured_test_roots


PRODUCTION_ROOTS = (
    "spice/cli/entry.py",
    "spice/serve/app.py",
    "spice/hooks/precommit.py",
    "spice/hooks/commitmsg.py",
    "spice/hooks/refguard.py",
    "spice/hooks/install.py",
    "spice/agent/cli.py",
    "spice/agent/judgeadapter.py",  # spice-judge console script
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
    raw_providers = effective_table(repo_root, "policy").get(REACHABILITY_PROVIDERS_KEY)
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
    result = run_tool_command(
        list(provider.argv),
        policy="extension",
        operation=f"reachability provider {provider.name}",
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
