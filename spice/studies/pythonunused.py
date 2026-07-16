"""Unused production top-level Python symbol study.

The study deliberately reuses the fixed-point symbol engine behind
``symbol-reachability``. Its scope is module-level functions and classes;
methods, including Protocol implementations and framework overrides, remain
owned by the method-aware symbol-reachability policy.

Production references include ordinary Python references, literal CLI handler
tables (ordinary bare-name references), installed entry-point targets, and
repository command modules invoked through ``python -m``. A top-level symbol
reached only through opaque string dispatch such as ``getattr`` must have an
individually named exemption with a reason.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping, Sequence

from spice.configlayer import effective_commands
from spice.repocfg import read_pyproject
from spice.studies.reachability import PRODUCTION_ROOTS, _python_test_paths
from spice.studies.reachabilitypython import (
    _SymbolDefinition,
    _SymbolRef,
    _collect_production_and_test_symbol_refs,
    _collect_symbol_definitions,
    _module_to_path,
    _walk_imports,
)
from spice.studies.walk import configured_test_roots


STATUS_CANDIDATE_UNUSED = "candidate-unused"
STATUS_RETAINED = "retained"
STATUS_TEST_ONLY = "test-only"
STATUS_USED = "used"

REASON_NO_REFERENCES = "no production or test references"
REASON_TEST_ONLY = "references only in tests"
REASON_PRODUCTION_REFERENCE = "referenced by production Python"
REASON_CONFIGURED_ENTRY_POINT = "configured Python entry point"

_FINDING_STATUSES = frozenset({STATUS_CANDIDATE_UNUSED, STATUS_TEST_ONLY})
_TOP_LEVEL_KINDS = frozenset({"function", "class"})


@dataclass(frozen=True)
class PythonUnusedExemption:
    symbol: str
    reason: str


# Keep this list exact and reviewable. Every entry must name one fully-qualified
# top-level symbol plus the runtime mechanism that reaches it.
PYTHON_UNUSED_EXEMPTIONS: tuple[PythonUnusedExemption, ...] = ()


@dataclass(frozen=True)
class PythonUnusedEntry:
    module: str
    path: str
    line: int
    kind: str
    symbol: str
    status: str
    reason: str
    test_references: list[str]


@dataclass(frozen=True)
class _ConfiguredRuntimeBindings:
    root_paths: tuple[Path, ...]
    symbol_refs: frozenset[_SymbolRef]


def collect_python_unused_entries(
    repo_root: Path,
    *,
    package: str = "spice",
    exemptions: Sequence[PythonUnusedExemption] = PYTHON_UNUSED_EXEMPTIONS,
) -> list[PythonUnusedEntry]:
    """Classify production-reachable top-level Python functions and classes."""
    pkg_root = repo_root / package
    if not pkg_root.is_dir():
        return []

    configured_bindings = _configured_runtime_bindings(repo_root, pkg_root, package)
    root_paths = [
        repo_root / relative
        for relative in PRODUCTION_ROOTS
        if (repo_root / relative).is_file()
    ]
    root_paths.extend(configured_bindings.root_paths)
    production_modules = _walk_imports(root_paths, pkg_root, package)
    definitions = _collect_symbol_definitions(pkg_root, package, production_modules)
    production_paths = [
        path
        for module in production_modules
        if (path := _module_to_path(module, pkg_root, package)) is not None
    ]
    test_paths = _python_test_paths(tuple(configured_test_roots(repo_root)))
    analysis = _collect_production_and_test_symbol_refs(
        production_paths,
        test_paths,
        definitions,
        pkg_root=pkg_root,
        package=package,
    )
    exemption_reasons = _exemption_reasons(exemptions)

    entries = [
        _classify_definition(
            ref,
            definition,
            repo_root=repo_root,
            production_refs=analysis.production_refs,
            test_refs=analysis.test_refs,
            test_importers=analysis.test_importers,
            configured_refs=configured_bindings.symbol_refs,
            exemption_reasons=exemption_reasons,
        )
        for ref, definition in definitions.items()
        if definition.kind in _TOP_LEVEL_KINDS
    ]
    return sorted(entries, key=lambda entry: (entry.path, entry.line, entry.symbol))


def scan_python_unused_symbols(
    repo_root: Path,
    *,
    package: str = "spice",
    exemptions: Sequence[PythonUnusedExemption] = PYTHON_UNUSED_EXEMPTIONS,
) -> list[PythonUnusedEntry]:
    return [
        entry
        for entry in collect_python_unused_entries(
            repo_root, package=package, exemptions=exemptions
        )
        if entry.status in _FINDING_STATUSES
    ]


def render_python_unused_board(
    findings: Sequence[PythonUnusedEntry], *, limit: int | None = None
) -> str:
    shown = list(findings)[:limit] if limit is not None else list(findings)
    if not shown:
        return "python-unused: no candidate-unused or test-only top-level symbols found"
    candidate_count = sum(
        finding.status == STATUS_CANDIDATE_UNUSED for finding in findings
    )
    test_only_count = sum(finding.status == STATUS_TEST_ONLY for finding in findings)
    suffix = f" (showing {len(shown)})" if limit and len(findings) > len(shown) else ""
    rows = [
        f"python-unused: {candidate_count} candidate-unused and "
        f"{test_only_count} test-only top-level symbol(s) found{suffix}"
    ]
    for finding in shown:
        rows.append(
            f"  {finding.path}:{finding.line} {finding.kind} {finding.symbol} "
            f"status={finding.status} reason={finding.reason}"
        )
        if finding.test_references:
            rows.append(
                f"    referenced by tests: {', '.join(finding.test_references)}"
            )
    return "\n".join(rows)


def _classify_definition(
    ref: _SymbolRef,
    definition: _SymbolDefinition,
    *,
    repo_root: Path,
    production_refs: set[_SymbolRef],
    test_refs: set[_SymbolRef],
    test_importers: Mapping[_SymbolRef, set[str]],
    configured_refs: frozenset[_SymbolRef],
    exemption_reasons: Mapping[str, str],
) -> PythonUnusedEntry:
    qualified = f"{ref.module}.{ref.symbol}"
    if qualified in exemption_reasons:
        status = STATUS_RETAINED
        reason = f"named dynamic-dispatch exemption: {exemption_reasons[qualified]}"
    elif ref in configured_refs:
        status = STATUS_USED
        reason = REASON_CONFIGURED_ENTRY_POINT
    elif ref in production_refs:
        status = STATUS_USED
        reason = REASON_PRODUCTION_REFERENCE
    elif ref in test_refs:
        status = STATUS_TEST_ONLY
        reason = REASON_TEST_ONLY
    else:
        status = STATUS_CANDIDATE_UNUSED
        reason = REASON_NO_REFERENCES
    return PythonUnusedEntry(
        module=ref.module,
        path=str(definition.module_path.relative_to(repo_root)),
        line=definition.line,
        kind=definition.kind,
        symbol=definition.symbol,
        status=status,
        reason=reason,
        test_references=sorted(test_importers.get(ref, set())),
    )


def _exemption_reasons(
    exemptions: Iterable[PythonUnusedExemption],
) -> dict[str, str]:
    reasons: dict[str, str] = {}
    for exemption in exemptions:
        symbol = exemption.symbol.strip()
        reason = exemption.reason.strip()
        if not symbol or not reason:
            raise ValueError("python-unused exemptions require symbol and reason")
        if symbol in reasons:
            raise ValueError(f"duplicate python-unused exemption: {symbol}")
        reasons[symbol] = reason
    return reasons


def _configured_runtime_bindings(
    repo_root: Path, pkg_root: Path, package: str
) -> _ConfiguredRuntimeBindings:
    config = read_pyproject(repo_root)

    root_paths: set[Path] = set()
    symbol_refs: set[_SymbolRef] = set()
    project = config.get("project", {})
    if isinstance(project, dict):
        for table_name in ("scripts", "gui-scripts"):
            _collect_entry_point_table(
                project.get(table_name),
                pkg_root=pkg_root,
                package=package,
                root_paths=root_paths,
                symbol_refs=symbol_refs,
            )
        groups = project.get("entry-points")
        if isinstance(groups, dict):
            for table in groups.values():
                _collect_entry_point_table(
                    table,
                    pkg_root=pkg_root,
                    package=package,
                    root_paths=root_paths,
                    symbol_refs=symbol_refs,
                )

    for argv in effective_commands(repo_root).values():
        _collect_python_module_command(
            argv,
            pkg_root=pkg_root,
            package=package,
            root_paths=root_paths,
        )
    return _ConfiguredRuntimeBindings(tuple(sorted(root_paths)), frozenset(symbol_refs))


def _collect_entry_point_table(
    table: object,
    *,
    pkg_root: Path,
    package: str,
    root_paths: set[Path],
    symbol_refs: set[_SymbolRef],
) -> None:
    if not isinstance(table, dict):
        return
    for raw_target in table.values():
        if not isinstance(raw_target, str):
            continue
        target = raw_target.partition("[")[0].strip()
        module, separator, symbol = target.partition(":")
        path = _module_to_path(module, pkg_root, package)
        if path is None:
            continue
        root_paths.add(path)
        if separator and symbol:
            symbol_refs.add(_SymbolRef(module, symbol.partition(".")[0]))


def _collect_python_module_command(
    argv: object,
    *,
    pkg_root: Path,
    package: str,
    root_paths: set[Path],
) -> None:
    if not isinstance(argv, list):
        return
    for index, token in enumerate(argv[:-1]):
        if token != "-m" or not isinstance(argv[index + 1], str):
            continue
        module = argv[index + 1]
        path = _module_to_path(module, pkg_root, package)
        if path is not None:
            root_paths.add(path)
