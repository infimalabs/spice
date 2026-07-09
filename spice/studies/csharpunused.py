"""C# unused-code candidate report via the tree-sitter seam."""

from __future__ import annotations

import json
import re
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable, Sequence

from spice.errors import SpiceError
from spice.repocfg import policy_table
from spice.studies.walk import is_excluded_path

STATUS_CANDIDATE_UNUSED = "candidate-unused"
STATUS_RETAINED = "retained"
STATUS_USED = "used"
CSharpRetentionKind = str
CSharpRetentionNames = tuple[str, ...]
RETENTION_POLICY_KEY = "csharp_unused_retention"
RETENTION_BASE_TYPES_KEY = "base_types"
RETENTION_INTERFACES_KEY = "interfaces"
RETENTION_ATTRIBUTE_NAMES_KEY = "attribute_names"
RETENTION_CONFIG_CONTEXT = f"[tool.spice.policy.{RETENTION_POLICY_KEY}]"

PRIVATE_BLOCKING_MODIFIERS = frozenset({"public", "protected", "internal"})
TYPE_CONTEXT_DECLARATIONS = frozenset(
    {
        "class_declaration",
        "interface_declaration",
        "struct_declaration",
    }
)
_IDENTIFIER_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


@dataclass(frozen=True)
class CSharpUnusedEntry:
    path: str
    line: int
    kind: str
    name: str
    status: str
    reason: str
    reference_count: int


@dataclass(frozen=True)
class CSharpRetentionPolicy:
    base_types: CSharpRetentionNames = ()
    interfaces: CSharpRetentionNames = ()
    attribute_names: CSharpRetentionNames = ()


def collect_csharp_unused_entries(
    paths: Sequence[Path],
    *,
    root: Path,
) -> list[CSharpUnusedEntry]:
    entries: list[CSharpUnusedEntry] = []
    seen: set[Path] = set()
    retention_policy = csharp_retention_policy(root)
    for rel_path in sorted(paths):
        if rel_path in seen:
            continue
        seen.add(rel_path)
        if rel_path.suffix.lower() != ".cs" or is_excluded_path(
            rel_path, repo_root=root
        ):
            continue
        abs_path = root / rel_path
        if not abs_path.exists() or not abs_path.is_file():
            continue
        entries.extend(
            _collect_file_entries(
                rel_path, abs_path.read_bytes(), retention_policy=retention_policy
            )
        )
    return sorted(
        entries, key=lambda entry: (entry.path, entry.line, entry.kind, entry.name)
    )


def scan_csharp_unused_candidates(
    paths: Sequence[Path],
    *,
    root: Path,
) -> list[CSharpUnusedEntry]:
    return [
        entry
        for entry in collect_csharp_unused_entries(paths, root=root)
        if entry.status == STATUS_CANDIDATE_UNUSED
    ]


def csharp_unused_payload(entries: Sequence[CSharpUnusedEntry]) -> dict[str, Any]:
    counts = Counter(entry.status for entry in entries)
    return {
        "artifactKind": "spice.study.csharp-unused-candidates",
        "stats": {
            "candidateUnused": counts[STATUS_CANDIDATE_UNUSED],
            "retained": counts[STATUS_RETAINED],
            "used": counts[STATUS_USED],
        },
        "entries": [asdict(entry) for entry in entries],
    }


def render_csharp_unused_json(entries: Sequence[CSharpUnusedEntry]) -> str:
    return json.dumps(csharp_unused_payload(entries), indent=2)


def render_csharp_unused_board(
    entries: Sequence[CSharpUnusedEntry], *, limit: int | None = None
) -> str:
    payload = csharp_unused_payload(entries)
    stats = payload["stats"]
    candidate_count = stats["candidateUnused"]
    suffix = (
        f" showing={limit}"
        if limit and isinstance(candidate_count, int) and candidate_count > limit
        else ""
    )
    rows = [
        "csharp-unused-candidates: "
        f"candidateUnused={stats['candidateUnused']} "
        f"used={stats['used']} retained={stats['retained']}{suffix}"
    ]
    rows.extend(
        _status_rows(entries, STATUS_CANDIDATE_UNUSED, "Candidate Entries", limit=limit)
    )
    rows.extend(_status_rows(entries, STATUS_USED, "Used Entries"))
    rows.extend(_status_rows(entries, STATUS_RETAINED, "Retained Entries"))
    return "\n".join(rows)


def csharp_retention_policy(repo_root: Path) -> CSharpRetentionPolicy:
    raw = policy_table(repo_root).get(RETENTION_POLICY_KEY)
    if raw is None:
        return CSharpRetentionPolicy()
    if not isinstance(raw, dict):
        raise SpiceError(f"{RETENTION_CONFIG_CONTEXT} must be a table")
    return CSharpRetentionPolicy(
        base_types=_retention_name_list(raw, RETENTION_BASE_TYPES_KEY),
        interfaces=_retention_name_list(raw, RETENTION_INTERFACES_KEY),
        attribute_names=_retention_name_list(raw, RETENTION_ATTRIBUTE_NAMES_KEY),
    )


def _retention_name_list(raw: dict[str, Any], key: str) -> CSharpRetentionNames:
    value = raw.get(key, [])
    if not isinstance(value, list):
        raise SpiceError(f"{RETENTION_CONFIG_CONTEXT}.{key} must be a list")
    names: list[str] = []
    for index, item in enumerate(value):
        if not isinstance(item, str) or not item.strip():
            raise SpiceError(
                f"{RETENTION_CONFIG_CONTEXT}.{key}[{index}] must be a non-empty string"
            )
        name = item.strip()
        if name not in names:
            names.append(name)
    return tuple(names)


def _collect_file_entries(
    rel_path: Path, source: bytes, *, retention_policy: CSharpRetentionPolicy
) -> list[CSharpUnusedEntry]:
    from spice.studies import treesitter

    parsed = treesitter.parse_source(rel_path, source)
    if parsed is None or parsed.language != "csharp":
        return []
    identifier_counts = _identifier_counts(source, parsed.root)
    entries: list[CSharpUnusedEntry] = []
    for node in _walk(parsed.root):
        if node.type == "using_directive":
            entry = _using_entry(source, rel_path.as_posix(), node, identifier_counts)
            if entry is not None:
                entries.append(entry)
        elif node.type == "method_declaration":
            entry = _method_entry(
                source,
                rel_path.as_posix(),
                node,
                identifier_counts,
                retention_policy=retention_policy,
            )
            if entry is not None:
                entries.append(entry)
        elif node.type == "field_declaration":
            entries.extend(
                _field_entries(
                    source,
                    rel_path.as_posix(),
                    node,
                    identifier_counts,
                    retention_policy=retention_policy,
                )
            )
    return entries


def _walk(node: Any) -> Iterable[Any]:
    yield node
    for child in node.children:
        yield from _walk(child)


def _named_descendants(node: Any, node_type: str) -> list[Any]:
    return [child for child in _walk(node) if child.type == node_type]


def _node_text(source: bytes, node: Any) -> str:
    return source[node.start_byte : node.end_byte].decode("utf-8", "replace")


def _node_line(node: Any) -> int:
    return node.start_point[0] + 1


def _first_identifier(source: bytes, node: Any) -> str | None:
    for child in node.children:
        if child.type == "identifier":
            return _node_text(source, child)
    return None


def _method_identifier(source: bytes, node: Any) -> str | None:
    before_parameters = []
    for child in node.children:
        if child.type == "parameter_list":
            break
        before_parameters.append(child)
    for child in reversed(before_parameters):
        if child.type == "identifier":
            return _node_text(source, child)
    return _first_identifier(source, node)


def _identifier_counts(source: bytes, root: Any) -> Counter[str]:
    counts: Counter[str] = Counter()
    for node in _walk(root):
        if node.type == "identifier":
            counts[_node_text(source, node)] += 1
    return counts


def _modifier_tokens(source: bytes, node: Any) -> set[str]:
    return {
        _node_text(source, child) for child in node.children if child.type == "modifier"
    }


def _member_is_private(source: bytes, node: Any) -> bool:
    modifiers = _modifier_tokens(source, node)
    if "private" in modifiers:
        return True
    type_context = _nearest_type_context(node)
    if type_context is not None and type_context.type == "interface_declaration":
        return False
    return not bool(modifiers & PRIVATE_BLOCKING_MODIFIERS)


def _has_attribute(node: Any) -> bool:
    return any(child.type == "attribute_list" for child in node.children)


def _attribute_names(source: bytes, node: Any) -> tuple[str, ...]:
    names: list[str] = []
    for attribute_list in (
        child for child in node.children if child.type == "attribute_list"
    ):
        for attribute in _named_descendants(attribute_list, "attribute"):
            text = _node_text(source, attribute).split("(", maxsplit=1)[0].strip()
            if text:
                names.append(text)
    return tuple(names)


def _nearest_type_context(node: Any) -> Any | None:
    current = node.parent
    while current is not None:
        if current.type in TYPE_CONTEXT_DECLARATIONS:
            return current
        current = current.parent
    return None


def _type_header(source: bytes, node: Any) -> str:
    text = _node_text(source, node)
    return text.split("{", 1)[0]


def _type_is_partial(source: bytes, node: Any | None) -> bool:
    return bool(
        node is not None and re.search(r"\bpartial\b", _type_header(source, node))
    )


def _retention_reason(
    source: bytes, node: Any, retention_policy: CSharpRetentionPolicy
) -> str | None:
    configured_reason = _configured_retention_reason(source, node, retention_policy)
    if configured_reason is not None:
        return configured_reason
    if _has_attribute(node):
        return "attribute_retained"
    if _type_is_partial(source, _nearest_type_context(node)):
        return "partial_declaration"
    return None


def _configured_retention_reason(
    source: bytes, node: Any, retention_policy: CSharpRetentionPolicy
) -> str | None:
    attribute = _matching_configured_attribute(source, node, retention_policy)
    if attribute is not None:
        return f"configured_attribute:{attribute}"
    type_context = _nearest_type_context(node)
    if type_context is None:
        return None
    base_type = _matching_configured_type(
        source, type_context, "base_type", retention_policy.base_types
    )
    if base_type is not None:
        return f"configured_base_type:{base_type}"
    interface = _matching_configured_type(
        source, type_context, "interface", retention_policy.interfaces
    )
    if interface is not None:
        return f"configured_interface:{interface}"
    return None


def _matching_configured_attribute(
    source: bytes, node: Any, retention_policy: CSharpRetentionPolicy
) -> str | None:
    return _matching_configured_name(
        _configured_attribute_name_forms(retention_policy.attribute_names),
        {
            form
            for attribute_name in _attribute_names(source, node)
            for form in _attribute_name_forms(attribute_name)
        },
    )


def _matching_configured_type(
    source: bytes,
    type_context: Any,
    kind: CSharpRetentionKind,
    configured_names: CSharpRetentionNames,
) -> str | None:
    return _matching_configured_name(
        _configured_type_name_forms(configured_names),
        {
            form
            for type_name in _base_type_names(source, type_context, kind=kind)
            for form in _type_name_forms(type_name)
        },
    )


def _matching_configured_name(
    configured_forms: dict[str, str], declared_forms: set[str]
) -> str | None:
    for form, configured_name in configured_forms.items():
        if form in declared_forms:
            return configured_name
    return None


def _configured_type_name_forms(names: CSharpRetentionNames) -> dict[str, str]:
    return {
        form: configured_name
        for configured_name in names
        for form in _type_name_forms(configured_name)
    }


def _configured_attribute_name_forms(names: CSharpRetentionNames) -> dict[str, str]:
    return {
        form: configured_name
        for configured_name in names
        for form in _attribute_name_forms(configured_name)
    }


def _base_type_names(
    source: bytes, type_context: Any, *, kind: CSharpRetentionKind
) -> tuple[str, ...]:
    base_list = next(
        (child for child in type_context.children if child.type == "base_list"), None
    )
    if base_list is None:
        return ()
    candidates = [
        _node_text(source, child)
        for child in base_list.children
        if child.type not in {":", ","}
    ]
    if kind == "base_type" and candidates:
        return tuple(candidates[:1])
    return tuple(candidates)


def _type_name_forms(name: str) -> tuple[str, ...]:
    compact = re.sub(r"\s+", "", name.strip())
    if not compact:
        return ()
    without_global = compact.removeprefix("global::")
    without_generic = re.sub(r"<.*>", "", without_global)
    simple = without_generic.rsplit(".", maxsplit=1)[-1]
    forms = [without_global, without_generic, simple]
    return tuple(dict.fromkeys(form for form in forms if form))


def _attribute_name_forms(name: str) -> tuple[str, ...]:
    forms = list(_type_name_forms(name))
    for form in tuple(forms):
        if form.endswith("Attribute") and len(form) > len("Attribute"):
            forms.append(form[: -len("Attribute")])
        else:
            forms.append(f"{form}Attribute")
    return tuple(dict.fromkeys(forms))


def _reference_status(reference_count: int) -> tuple[str, str]:
    if reference_count > 1:
        return STATUS_USED, "identifier_referenced_outside_declaration"
    return STATUS_CANDIDATE_UNUSED, "no_references_outside_declaration"


def _method_entry(
    source: bytes,
    path: str,
    node: Any,
    identifier_counts: Counter[str],
    *,
    retention_policy: CSharpRetentionPolicy,
) -> CSharpUnusedEntry | None:
    if not _member_is_private(source, node):
        return None
    name = _method_identifier(source, node)
    if not name:
        return None
    reference_count = identifier_counts[name]
    retention_reason = _retention_reason(source, node, retention_policy)
    if retention_reason is not None:
        status, reason, kind = STATUS_RETAINED, retention_reason, "method"
    else:
        status, reason = _reference_status(reference_count)
        kind = "private_method"
    return CSharpUnusedEntry(
        path=path,
        line=_node_line(node),
        kind=kind,
        name=name,
        status=status,
        reason=reason,
        reference_count=reference_count,
    )


def _field_entries(
    source: bytes,
    path: str,
    node: Any,
    identifier_counts: Counter[str],
    *,
    retention_policy: CSharpRetentionPolicy,
) -> list[CSharpUnusedEntry]:
    if not _member_is_private(source, node):
        return []
    retention_reason = _retention_reason(source, node, retention_policy)
    entries: list[CSharpUnusedEntry] = []
    for declarator in _named_descendants(node, "variable_declarator"):
        name = _first_identifier(source, declarator)
        if not name:
            continue
        reference_count = identifier_counts[name]
        if retention_reason is not None:
            status, reason = STATUS_RETAINED, retention_reason
        else:
            status, reason = _reference_status(reference_count)
        entries.append(
            CSharpUnusedEntry(
                path=path,
                line=_node_line(declarator),
                kind="private_field",
                name=name,
                status=status,
                reason=reason,
                reference_count=reference_count,
            )
        )
    return entries


def _using_entry(
    source: bytes,
    path: str,
    node: Any,
    identifier_counts: Counter[str],
) -> CSharpUnusedEntry | None:
    text = _node_text(source, node).strip().rstrip(";")
    body = text.removeprefix("using").strip()
    if body.startswith("static "):
        return _retained_using(
            path, node, body, "static_import_requires_semantic_resolution"
        )
    if "=" not in body:
        return _retained_using(
            path, node, body, "namespace_import_requires_semantic_resolution"
        )
    alias = body.split("=", 1)[0].strip()
    if not _IDENTIFIER_RE.match(alias):
        return None
    reference_count = identifier_counts[alias]
    status, reason = _reference_status(reference_count)
    return CSharpUnusedEntry(
        path=path,
        line=_node_line(node),
        kind="using_directive",
        name=alias,
        status=status,
        reason=reason,
        reference_count=reference_count,
    )


def _retained_using(path: str, node: Any, name: str, reason: str) -> CSharpUnusedEntry:
    return CSharpUnusedEntry(
        path=path,
        line=_node_line(node),
        kind="using_directive",
        name=name,
        status=STATUS_RETAINED,
        reason=reason,
        reference_count=0,
    )


def _status_rows(
    entries: Sequence[CSharpUnusedEntry],
    status: str,
    title: str,
    *,
    limit: int | None = None,
) -> list[str]:
    rows = [title]
    matching = [entry for entry in entries if entry.status == status]
    shown = matching[:limit] if limit is not None else matching
    if not shown:
        rows.append("  none")
        return rows
    for entry in shown:
        rows.append(
            f"  {entry.path}:{entry.line} {entry.kind} {entry.name} "
            f"refs={entry.reference_count} reason={entry.reason}"
        )
    return rows
