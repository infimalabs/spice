"""JavaScript unused top-level symbol study via the tree-sitter seam."""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

from spice.studies.walk import is_excluded_path, is_test_path

STATUS_CANDIDATE_UNUSED = "candidate-unused"
STATUS_RETAINED = "retained"
STATUS_TEST_ONLY = "test-only"
STATUS_USED = "used"

_FINDING_STATUSES = frozenset({STATUS_CANDIDATE_UNUSED, STATUS_TEST_ONLY})
_IDENTIFIER_NODE_TYPES = frozenset({"identifier", "shorthand_property_identifier"})
# Test harnesses reach production symbols through a sandbox namespace
# (context.symbol), so test-side references must also match property access.
_TEST_REFERENCE_NODE_TYPES = _IDENTIFIER_NODE_TYPES | {"property_identifier"}
_TOP_LEVEL_DECLARATION_TYPES = frozenset(
    {
        "class_declaration",
        "function_declaration",
        "generator_function_declaration",
        "lexical_declaration",
        "variable_declaration",
    }
)
_VARIABLE_DECLARATION_TYPES = frozenset({"lexical_declaration", "variable_declaration"})


@dataclass(frozen=True)
class JavaScriptUnusedEntry:
    path: str
    line: int
    kind: str
    name: str
    status: str
    reason: str
    reference_count: int
    test_reference_count: int


@dataclass(frozen=True)
class _ParsedJavaScriptFile:
    relative_path: str
    source: bytes
    root: Any
    is_test: bool


def collect_javascript_unused_entries(
    paths: Sequence[Path],
    *,
    root: Path,
    allow_symbols: Iterable[str] = (),
) -> list[JavaScriptUnusedEntry]:
    parsed_files = _parse_javascript_files(paths, root=root)
    production_counts, test_counts = _reference_counts(parsed_files)
    retained_symbols = frozenset(allow_symbols)
    entries: list[JavaScriptUnusedEntry] = []
    for parsed_file in parsed_files:
        for node in _top_level_declarations(parsed_file.root):
            if node.type in _VARIABLE_DECLARATION_TYPES:
                entries.extend(
                    _variable_entries(
                        parsed_file,
                        node,
                        production_counts=production_counts,
                        test_counts=test_counts,
                        retained_symbols=retained_symbols,
                    )
                )
                continue
            entry = _declaration_entry(
                parsed_file,
                node,
                production_counts=production_counts,
                test_counts=test_counts,
                retained_symbols=retained_symbols,
            )
            if entry is not None:
                entries.append(entry)
    return sorted(
        entries, key=lambda entry: (entry.path, entry.line, entry.kind, entry.name)
    )


def scan_javascript_unused_symbols(
    paths: Sequence[Path],
    *,
    root: Path,
    allow_symbols: Iterable[str] = (),
    declaration_exemptions: Mapping[tuple[str, str], str] | None = None,
) -> list[JavaScriptUnusedEntry]:
    exemptions = _validated_declaration_exemptions(declaration_exemptions or {})
    return [
        entry
        for entry in collect_javascript_unused_entries(
            paths, root=root, allow_symbols=allow_symbols
        )
        if entry.status in _FINDING_STATUSES
        and (entry.path, entry.name) not in exemptions
    ]


def _validated_declaration_exemptions(
    exemptions: Mapping[tuple[str, str], str],
) -> frozenset[tuple[str, str]]:
    validated: set[tuple[str, str]] = set()
    for key, raw_reason in exemptions.items():
        if len(key) != 2 or not all(str(part).strip() for part in key):
            raise ValueError("javascript-unused exemption keys must be (path, symbol)")
        if not str(raw_reason).strip():
            raise ValueError(
                "javascript-unused exemption reasons must name why the declaration stays"
            )
        validated.add((Path(key[0]).as_posix(), key[1]))
    return frozenset(validated)


def render_javascript_unused_board(
    findings: Sequence[JavaScriptUnusedEntry],
    *,
    limit: int | None = None,
) -> str:
    shown = list(findings)[:limit] if limit is not None else list(findings)
    if not shown:
        return "javascript-unused: no candidate-unused or test-only top-level symbols found"
    candidate_count = sum(
        1 for finding in findings if finding.status == STATUS_CANDIDATE_UNUSED
    )
    test_only_count = sum(
        1 for finding in findings if finding.status == STATUS_TEST_ONLY
    )
    suffix = f" (showing {len(shown)})" if limit and len(findings) > len(shown) else ""
    rows = [
        f"javascript-unused: {candidate_count} candidate-unused and "
        f"{test_only_count} test-only top-level symbol(s) found{suffix}"
    ]
    for finding in shown:
        rows.append(
            f"  {finding.path}:{finding.line} {finding.kind} {finding.name} "
            f"status={finding.status} refs={finding.reference_count} "
            f"test_refs={finding.test_reference_count} reason={finding.reason}"
        )
    return "\n".join(rows)


def _parse_javascript_files(
    paths: Sequence[Path],
    *,
    root: Path,
) -> list[_ParsedJavaScriptFile]:
    from spice.studies import treesitter

    parsed_files: list[_ParsedJavaScriptFile] = []
    seen: set[Path] = set()
    for rel_path in sorted(paths):
        if rel_path in seen:
            continue
        seen.add(rel_path)
        if treesitter.language_for_path(rel_path) != "javascript":
            continue
        if is_excluded_path(rel_path, repo_root=root):
            continue
        abs_path = root / rel_path
        if not abs_path.exists() or not abs_path.is_file():
            continue
        source = abs_path.read_bytes()
        parsed = treesitter.parse_source(rel_path, source)
        if parsed is None:
            continue
        parsed_files.append(
            _ParsedJavaScriptFile(
                relative_path=rel_path.as_posix(),
                source=parsed.source,
                root=parsed.root,
                is_test=is_test_path(rel_path, root),
            )
        )
    return parsed_files


def _reference_counts(
    parsed_files: Sequence[_ParsedJavaScriptFile],
) -> tuple[Counter[str], Counter[str]]:
    production_counts: Counter[str] = Counter()
    test_counts: Counter[str] = Counter()
    for parsed_file in parsed_files:
        if parsed_file.is_test:
            counts, node_types = test_counts, _TEST_REFERENCE_NODE_TYPES
        else:
            counts, node_types = production_counts, _IDENTIFIER_NODE_TYPES
        for node in _walk(parsed_file.root):
            if node.type in node_types:
                counts[_node_text(parsed_file.source, node)] += 1
    return production_counts, test_counts


def _walk(node: Any) -> Iterable[Any]:
    yield node
    for child in node.children:
        yield from _walk(child)


def _node_text(source: bytes, node: Any) -> str:
    return source[node.start_byte : node.end_byte].decode("utf-8", "replace")


def _node_line(node: Any) -> int:
    return node.start_point[0] + 1


def _top_level_declarations(root: Any) -> Iterable[Any]:
    for child in root.children:
        if child.type in _TOP_LEVEL_DECLARATION_TYPES:
            yield child
        elif child.type == "export_statement":
            for exported_child in child.children:
                if exported_child.type in _TOP_LEVEL_DECLARATION_TYPES:
                    yield exported_child


def _first_direct_identifier(source: bytes, node: Any) -> str | None:
    for child in node.children:
        if child.type == "identifier":
            return _node_text(source, child)
    return None


def _declaration_keyword(source: bytes, node: Any) -> str:
    return _node_text(source, node).lstrip().split(maxsplit=1)[0]


def _variable_kind(source: bytes, node: Any) -> str:
    keyword = _declaration_keyword(source, node)
    if keyword == "const":
        return "constant"
    if keyword == "let":
        return "let"
    return "variable"


def _entry_status(
    name: str,
    *,
    production_references: int,
    test_references: int,
    retained_symbols: frozenset[str],
    declared_in_test: bool,
) -> tuple[str, str]:
    if name in retained_symbols:
        return STATUS_RETAINED, "intentional_global_allowlist"
    if declared_in_test:
        if production_references + test_references > 1:
            return STATUS_USED, "identifier_referenced_outside_declaration"
        return STATUS_CANDIDATE_UNUSED, "no_references_outside_declaration"
    if production_references > 1:
        return STATUS_USED, "identifier_referenced_outside_declaration"
    if test_references > 0:
        return STATUS_TEST_ONLY, "references_only_in_tests"
    return STATUS_CANDIDATE_UNUSED, "no_references_outside_declaration"


def _symbol_entry(
    *,
    path: str,
    line: int,
    kind: str,
    name: str,
    production_counts: Counter[str],
    test_counts: Counter[str],
    retained_symbols: frozenset[str],
    declared_in_test: bool,
) -> JavaScriptUnusedEntry:
    production_references = production_counts[name]
    test_references = test_counts[name]
    status, reason = _entry_status(
        name,
        production_references=production_references,
        test_references=test_references,
        retained_symbols=retained_symbols,
        declared_in_test=declared_in_test,
    )
    return JavaScriptUnusedEntry(
        path=path,
        line=line,
        kind=kind,
        name=name,
        status=status,
        reason=reason,
        reference_count=production_references,
        test_reference_count=test_references,
    )


def _variable_entries(
    parsed_file: _ParsedJavaScriptFile,
    node: Any,
    *,
    production_counts: Counter[str],
    test_counts: Counter[str],
    retained_symbols: frozenset[str],
) -> list[JavaScriptUnusedEntry]:
    entries: list[JavaScriptUnusedEntry] = []
    kind = _variable_kind(parsed_file.source, node)
    for child in node.children:
        if child.type != "variable_declarator":
            continue
        name = _first_direct_identifier(parsed_file.source, child)
        if name is None:
            continue
        entries.append(
            _symbol_entry(
                path=parsed_file.relative_path,
                line=_node_line(child),
                kind=kind,
                name=name,
                production_counts=production_counts,
                test_counts=test_counts,
                retained_symbols=retained_symbols,
                declared_in_test=parsed_file.is_test,
            )
        )
    return entries


def _declaration_entry(
    parsed_file: _ParsedJavaScriptFile,
    node: Any,
    *,
    production_counts: Counter[str],
    test_counts: Counter[str],
    retained_symbols: frozenset[str],
) -> JavaScriptUnusedEntry | None:
    if node.type in _VARIABLE_DECLARATION_TYPES:
        return None
    name = _first_direct_identifier(parsed_file.source, node)
    if name is None:
        return None
    kind = "class" if node.type == "class_declaration" else "function"
    return _symbol_entry(
        path=parsed_file.relative_path,
        line=_node_line(node),
        kind=kind,
        name=name,
        production_counts=production_counts,
        test_counts=test_counts,
        retained_symbols=retained_symbols,
        declared_in_test=parsed_file.is_test,
    )
