"""Markdown projection for task DAG import/export."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.tasks import config, create, identity, ops, tw

CANONICAL_FENCE = "spice.task-dag.v1"
MARKDOWN_ID_PREFIX = "markdown-id:"
_HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$")
_LIST_RE = re.compile(r"^(\s*)(?:[-*+]|\d+[.)])\s+(.+?)\s*$")
_ACCEPTANCE_RE = re.compile(r"^acceptance\s*:\s*(.+)$", re.IGNORECASE)
_FIELD_LINK_RE = re.compile(r"\[[^\]]+\]\([^)]+\)")
_WORD_RE = re.compile(r"[0-9a-z]+")


@dataclass(frozen=True)
class MarkdownTaskNode:
    id: str
    title: str
    project: str = ""
    priority: str = ""
    flow: tuple[str, ...] = ()
    acceptance: tuple[str, ...] = ()
    description: str = ""
    annotations: tuple[str, ...] = ()
    after: tuple[str, ...] = ()


@dataclass(frozen=True)
class MarkdownTaskDag:
    root: str
    nodes: tuple[MarkdownTaskNode, ...]


@dataclass
class _DraftNode:
    id: str
    title: str
    project: str = ""
    priority: str = ""
    flow: list[str] = field(default_factory=list)
    acceptance: list[str] = field(default_factory=list)
    description_lines: list[str] = field(default_factory=list)
    annotations: list[str] = field(default_factory=list)
    after: list[str] = field(default_factory=list)

    def freeze(self) -> MarkdownTaskNode:
        description = "\n".join(self.description_lines).strip()
        return MarkdownTaskNode(
            id=self.id,
            title=self.title,
            project=self.project,
            priority=self.priority,
            flow=tuple(self.flow),
            acceptance=tuple(self.acceptance),
            description=description,
            annotations=tuple(self.annotations),
            after=tuple(self.after),
        )


def render_ledger(handle: str) -> str:
    return render_canonical(export_task_dag(handle))


def export_task_dag(handle: str) -> MarkdownTaskDag:
    root = identity.resolve(handle)
    rows: dict[str, dict[str, Any]] = {}

    def collect(row: dict[str, Any]) -> None:
        uuid = identity.uuid_of(row)
        if uuid in rows:
            return
        rows[uuid] = row
        for dep_uuid in _dependency_uuids(row):
            dep_rows = tw.export([dep_uuid])
            if dep_rows:
                collect(dep_rows[0])

    collect(root)
    node_by_uuid = {uuid: _node_from_row(row) for uuid, row in rows.items()}
    resolved_nodes: list[MarkdownTaskNode] = []
    for uuid, row in rows.items():
        node = node_by_uuid[uuid]
        after = tuple(
            node_by_uuid[dep_uuid].id
            for dep_uuid in _dependency_uuids(row)
            if dep_uuid in node_by_uuid
        )
        resolved_nodes.append(_replace_node(node, after=after))
    return MarkdownTaskDag(
        root=node_by_uuid[identity.uuid_of(root)].id,
        nodes=tuple(resolved_nodes),
    )


def ingest_path(
    path: str | Path,
    *,
    project: str | None,
    priority: str = config.DEFAULT_PRIORITY,
    creation_surface: str | None = None,
) -> str:
    text = Path(path).read_text(encoding="utf-8")
    dag = parse_markdown(text, default_project=project, default_priority=priority)
    return create_task_dag(dag, creation_surface=creation_surface)


def create_task_dag(
    dag: MarkdownTaskDag, *, creation_surface: str | None = None
) -> str:
    _validate_dag(dag)
    nodes = {node.id: node for node in dag.nodes}
    created: dict[str, str] = {}
    order: list[str] = []
    visiting: set[str] = set()

    def create_node(node_id: str) -> str:
        if node_id in created:
            return created[node_id]
        if node_id in visiting:
            raise SpiceError(
                f"markdown task DAG contains a dependency cycle at {node_id}"
            )
        visiting.add(node_id)
        node = nodes[node_id]
        after_handles = [create_node(dep_id) for dep_id in node.after]
        project = node.project.strip()
        if not project:
            raise SpiceError(f"markdown node {node.id!r} is missing project")
        handle = create.add(
            node.title,
            project=project,
            description=node.description,
            priority=node.priority or config.DEFAULT_PRIORITY,
            flow=list(node.flow) or None,
            after=after_handles,
            acceptance=list(node.acceptance),
            creation_surface=creation_surface,
        )
        ops.note(handle, f"{MARKDOWN_ID_PREFIX} {node.id}")
        for annotation in node.annotations:
            ops.note(handle, annotation)
        created[node_id] = handle
        order.append(node_id)
        visiting.remove(node_id)
        return handle

    root_handle = create_node(dag.root)
    lines = [f"root {root_handle}"]
    lines.extend(f"created {node_id} {created[node_id]}" for node_id in order)
    return "\n".join(lines)


def parse_markdown(
    text: str,
    *,
    default_project: str | None = None,
    default_priority: str = config.DEFAULT_PRIORITY,
) -> MarkdownTaskDag:
    canonical = _canonical_payload(text)
    if canonical is not None:
        return _dag_from_payload(canonical, default_project)
    return parse_freeform_markdown(
        text, default_project=default_project, default_priority=default_priority
    )


def render_canonical(dag: MarkdownTaskDag) -> str:
    _validate_dag(dag)
    payload = _payload_from_dag(dag)
    body = json.dumps(payload, indent=2, sort_keys=True)
    return f"# Spice Task DAG\n\n```json {CANONICAL_FENCE}\n{body}\n```\n"


def parse_freeform_markdown(
    text: str,
    *,
    default_project: str | None = None,
    default_priority: str = config.DEFAULT_PRIORITY,
    root_title: str = "Markdown import",
) -> MarkdownTaskDag:
    nodes: list[_DraftNode] = []
    used_ids: set[str] = set()
    heading_stack: list[tuple[int, str]] = []
    list_stack: list[tuple[int, str]] = []
    parentless: list[str] = []
    current_id = ""
    fence_lines: list[str] | None = None

    def node_by_id(node_id: str) -> _DraftNode:
        return next(node for node in nodes if node.id == node_id)

    def add_node(title: str, parent_id: str = "") -> str:
        node = _DraftNode(
            id=_unique_id(title, used_ids),
            title=_clean_title(title),
            project=default_project or "",
            priority=default_priority,
        )
        nodes.append(node)
        if parent_id:
            node_by_id(parent_id).after.append(node.id)
        else:
            parentless.append(node.id)
        return node.id

    for raw_line in text.splitlines():
        line = raw_line.rstrip()
        stripped = line.strip()
        if fence_lines is not None:
            fence_lines.append(line)
            if stripped.startswith("```"):
                if current_id:
                    node_by_id(current_id).annotations.append("\n".join(fence_lines))
                fence_lines = None
            continue
        if stripped.startswith("```"):
            fence_lines = [line]
            continue
        if not stripped:
            if current_id:
                node_by_id(current_id).description_lines.append("")
            continue
        heading = _HEADING_RE.match(line)
        if heading:
            level = len(heading.group(1))
            while heading_stack and heading_stack[-1][0] >= level:
                heading_stack.pop()
            parent_id = heading_stack[-1][1] if heading_stack else ""
            current_id = add_node(heading.group(2), parent_id)
            heading_stack.append((level, current_id))
            list_stack.clear()
            continue
        item = _LIST_RE.match(line)
        if item:
            indent = len(item.group(1).replace("\t", "    "))
            while list_stack and list_stack[-1][0] >= indent:
                list_stack.pop()
            parent_id = (
                list_stack[-1][1] if list_stack else _current_heading(heading_stack)
            )
            current_id = add_node(item.group(2), parent_id)
            list_stack.append((indent, current_id))
            continue
        if not current_id:
            current_id = add_node(stripped)
        current = node_by_id(current_id)
        acceptance = _ACCEPTANCE_RE.match(stripped)
        if acceptance:
            current.acceptance.append(acceptance.group(1).strip())
        elif _annotation_line(stripped):
            current.annotations.append(line)
        else:
            current.description_lines.append(line)
    if fence_lines is not None and current_id:
        node_by_id(current_id).annotations.append("\n".join(fence_lines))
    if not nodes:
        raise SpiceError("markdown import found no task nodes")
    if len(parentless) == 1:
        root = parentless[0]
    else:
        root = add_node(root_title)
        root_node = node_by_id(root)
        root_node.after = [node_id for node_id in parentless if node_id != root]
    return MarkdownTaskDag(root=root, nodes=tuple(node.freeze() for node in nodes))


def _current_heading(heading_stack: list[tuple[int, str]]) -> str:
    return heading_stack[-1][1] if heading_stack else ""


def _annotation_line(stripped: str) -> bool:
    return (
        stripped.startswith(">")
        or stripped.startswith("|")
        or stripped.startswith("---")
        or bool(_FIELD_LINK_RE.search(stripped))
    )


def _canonical_payload(text: str) -> dict[str, Any] | None:
    lines = text.splitlines()
    for index, line in enumerate(lines):
        stripped = line.strip()
        if not stripped.startswith("```") or CANONICAL_FENCE not in stripped:
            continue
        body: list[str] = []
        for candidate in lines[index + 1 :]:
            if candidate.strip().startswith("```"):
                try:
                    payload = json.loads("\n".join(body))
                except json.JSONDecodeError as exc:
                    raise SpiceError(
                        f"invalid canonical markdown task DAG: {exc}"
                    ) from exc
                if not isinstance(payload, dict):
                    raise SpiceError(
                        "canonical markdown task DAG must be a JSON object"
                    )
                return payload
            body.append(candidate)
        raise SpiceError("canonical markdown task DAG fence is not closed")
    return None


def _dag_from_payload(
    payload: dict[str, Any],
    default_project: str | None,
) -> MarkdownTaskDag:
    if payload.get("version") != 1:
        raise SpiceError("canonical markdown task DAG version must be 1")
    raw_nodes = payload.get("nodes")
    if not isinstance(raw_nodes, list) or not raw_nodes:
        raise SpiceError("canonical markdown task DAG requires non-empty nodes")
    nodes = tuple(_node_from_payload(item, default_project) for item in raw_nodes)
    root = str(payload.get("root") or nodes[0].id)
    dag = MarkdownTaskDag(root=root, nodes=nodes)
    _validate_dag(dag)
    return dag


def _node_from_payload(item: Any, default_project: str | None) -> MarkdownTaskNode:
    if not isinstance(item, dict):
        raise SpiceError("canonical markdown task DAG nodes must be objects")
    node_id = _required_str(item, "id")
    title = _required_str(item, "title")
    return MarkdownTaskNode(
        id=node_id,
        title=title,
        project=str(item.get("project") or default_project or ""),
        priority=str(item.get("priority") or ""),
        flow=_string_tuple(item.get("flow"), "flow"),
        acceptance=_string_tuple(item.get("acceptance"), "acceptance"),
        description=str(item.get("description") or ""),
        annotations=_string_tuple(item.get("annotations"), "annotations"),
        after=_string_tuple(item.get("after"), "after"),
    )


def _payload_from_dag(dag: MarkdownTaskDag) -> dict[str, Any]:
    return {
        "version": 1,
        "root": dag.root,
        "nodes": [
            _payload_node(node) for node in sorted(dag.nodes, key=lambda n: n.id)
        ],
    }


def _payload_node(node: MarkdownTaskNode) -> dict[str, Any]:
    payload: dict[str, Any] = {"id": node.id, "title": node.title}
    for key in ("project", "priority", "description"):
        value = getattr(node, key)
        if value:
            payload[key] = value
    for key in ("flow", "acceptance", "annotations", "after"):
        value = tuple(getattr(node, key))
        if value:
            payload[key] = list(value)
    return payload


def _node_from_row(row: dict[str, Any]) -> MarkdownTaskNode:
    annotations = _annotation_descriptions(row)
    markdown_id = _markdown_id(annotations) or identity.render_handle(row)
    visible_annotations = tuple(
        text for text in annotations if not text.startswith(MARKDOWN_ID_PREFIX)
    )
    return MarkdownTaskNode(
        id=markdown_id,
        title=str(row.get("description") or ""),
        project=str(row.get("project") or ""),
        priority=str(row.get("priority") or ""),
        flow=tuple(ops.phases_of(row)),
        acceptance=_acceptance_items(str(row.get("acceptance") or "")),
        description=str(row.get("task_description") or ""),
        annotations=visible_annotations,
    )


def _dependency_uuids(row: dict[str, Any]) -> tuple[str, ...]:
    raw = row.get("depends") or []
    if isinstance(raw, str):
        return (raw,) if raw else ()
    return tuple(str(item) for item in raw if str(item))


def _annotation_descriptions(row: dict[str, Any]) -> tuple[str, ...]:
    annotations = row.get("annotations") or []
    return tuple(
        str(item.get("description") or "")
        for item in annotations
        if isinstance(item, dict) and str(item.get("description") or "")
    )


def _markdown_id(annotations: tuple[str, ...]) -> str:
    for annotation in annotations:
        if annotation.startswith(MARKDOWN_ID_PREFIX):
            return annotation[len(MARKDOWN_ID_PREFIX) :].strip()
    return ""


def _acceptance_items(raw: str) -> tuple[str, ...]:
    return tuple(part.strip() for part in raw.split(" | ") if part.strip())


def _validate_dag(dag: MarkdownTaskDag) -> None:
    if not dag.root:
        raise SpiceError("markdown task DAG requires a root id")
    ids = [node.id for node in dag.nodes]
    if len(ids) != len(set(ids)):
        raise SpiceError("markdown task DAG node ids must be unique")
    node_ids = set(ids)
    if dag.root not in node_ids:
        raise SpiceError(f"markdown task DAG root {dag.root!r} is not a node")
    for node in dag.nodes:
        if not node.id.strip():
            raise SpiceError("markdown task DAG node id must be non-empty")
        if not node.title.strip():
            raise SpiceError(f"markdown task DAG node {node.id!r} is missing title")
        missing = [dep for dep in node.after if dep not in node_ids]
        if missing:
            raise SpiceError(
                f"markdown task DAG node {node.id!r} depends on unknown ids: "
                + ", ".join(missing)
            )


def _replace_node(
    node: MarkdownTaskNode, *, after: tuple[str, ...]
) -> MarkdownTaskNode:
    return MarkdownTaskNode(
        id=node.id,
        title=node.title,
        project=node.project,
        priority=node.priority,
        flow=node.flow,
        acceptance=node.acceptance,
        description=node.description,
        annotations=node.annotations,
        after=after,
    )


def _required_str(item: dict[str, Any], key: str) -> str:
    value = str(item.get(key) or "").strip()
    if not value:
        raise SpiceError(f"canonical markdown task DAG node missing {key!r}")
    return value


def _string_tuple(value: Any, field_name: str) -> tuple[str, ...]:
    if value is None or value == "":
        return ()
    if isinstance(value, str):
        return (value,)
    if isinstance(value, list):
        return tuple(str(item) for item in value if str(item))
    raise SpiceError(f"canonical markdown task DAG field {field_name!r} must be a list")


def _unique_id(title: str, used_ids: set[str]) -> str:
    words = _WORD_RE.findall(title.lower())
    base = "-".join(words) or "task"
    candidate = base
    index = 2
    while candidate in used_ids:
        candidate = f"{base}-{index}"
        index += 1
    used_ids.add(candidate)
    return candidate


def _clean_title(title: str) -> str:
    return title.strip().strip("#").strip() or "Untitled task"
