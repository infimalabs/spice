"""Shared live-row derivations for canned board diagrams."""

from __future__ import annotations

from collections import Counter, defaultdict, deque
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from spice.tasks import identity

TaskRow = dict[str, Any]
LABEL_LIMIT = 46
TICK_LABEL_LIMIT = 28


def handle(row: TaskRow) -> str:
    return identity.render_handle(row)


def stem(row: TaskRow) -> str:
    project = str(row.get("project") or "").lstrip(".")
    return project.split(".")[0] if project else "(none)"


def lane(row: TaskRow) -> str:
    path = str(row.get("origin_worktree") or "")
    return Path(path).name if path else "(unplaced)"


def epoch(row: TaskRow, field: str) -> datetime | None:
    raw = row.get(field)
    if raw is None:
        return None
    try:
        return datetime.fromtimestamp(int(str(raw)))
    except (TypeError, ValueError, OSError):
        pass
    text = str(raw or "")
    try:
        stamp = datetime.strptime(text, "%Y%m%dT%H%M%SZ").replace(tzinfo=timezone.utc)
        return stamp.astimezone().replace(tzinfo=None)
    except ValueError:
        return None


def iso(row: TaskRow, field: str) -> datetime | None:
    raw = row.get(field)
    if not raw:
        return None
    try:
        return datetime.fromisoformat(str(raw).replace("Z", "+00:00")).astimezone()
    except (TypeError, ValueError):
        return None


def slug(text: str) -> str:
    cleaned = "".join(char if char.isalnum() or char == "_" else "_" for char in text)
    if not cleaned:
        return "empty"
    return cleaned if cleaned[0].isalpha() or cleaned[0] == "_" else f"n{cleaned}"


def label(text: object, limit: int = LABEL_LIMIT) -> str:
    flat = " ".join(str(text or "").split())
    flat = flat.replace('"', "'").replace("#", "&num;")
    return flat if len(flat) <= limit else flat[: limit - 1].rstrip() + "…"


def quoted(text: object, limit: int = TICK_LABEL_LIMIT) -> str:
    return f'"{label(text, limit)}"'


def by_handle(rows: list[TaskRow]) -> dict[str, TaskRow]:
    return {handle(row): row for row in rows if handle(row)}


def origin_edges(rows: list[TaskRow]) -> list[tuple[str, str]]:
    known = by_handle(rows)
    edges: list[tuple[str, str]] = []
    for row in rows:
        origin = str(row.get("origin") or "")
        child = handle(row)
        if origin.startswith("task:") and origin[5:] in known and child:
            edges.append((origin[5:], child))
    return edges


def dependency_edges(rows: list[TaskRow]) -> list[tuple[str, str]]:
    by_uuid = {str(row.get("uuid") or ""): handle(row) for row in rows}
    edges: list[tuple[str, str]] = []
    for row in rows:
        blocked = handle(row)
        for blocker_uuid in row.get("depends") or ():
            blocker = by_uuid.get(str(blocker_uuid), "")
            if blocker and blocked:
                edges.append((blocker, blocked))
    return edges


def lineage(rows: list[TaskRow]) -> tuple[dict[str, list[str]], dict[str, str]]:
    children: dict[str, list[str]] = defaultdict(list)
    parents: dict[str, str] = {}
    for parent, child in origin_edges(rows):
        children[parent].append(child)
        parents[child] = parent
    return children, parents


def root_of(node: str, parents: dict[str, str]) -> str:
    seen = {node}
    while node in parents and parents[node] not in seen:
        node = parents[node]
        seen.add(node)
    return node


def subtree(root: str, children: dict[str, list[str]]) -> list[str]:
    result: list[str] = []
    queue = deque([root])
    while queue:
        node = queue.popleft()
        if node in result:
            continue
        result.append(node)
        queue.extend(children.get(node, ()))
    return result


def depth(root: str, children: dict[str, list[str]]) -> int:
    best = 0
    queue = deque([(root, 0)])
    while queue:
        node, level = queue.popleft()
        best = max(best, level)
        queue.extend((child, level + 1) for child in children.get(node, ()))
    return best


def lineage_roots(rows: list[TaskRow]) -> list[str]:
    children, parents = lineage(rows)
    roots = {root_of(node, parents) for node in parents}
    return sorted(roots, key=lambda root: (-len(subtree(root, children)), root))


def reviewer_lanes(rows: list[TaskRow]) -> dict[str, str]:
    votes: dict[str, Counter[str]] = defaultdict(Counter)
    for row in rows:
        thread = str(row.get("origin_thread") or "")
        if thread:
            votes[thread][lane(row)] += 1
    return {thread: tally.most_common(1)[0][0] for thread, tally in votes.items()}


def finding_bucket(value: object) -> str:
    finding = str(value or "").strip()
    if finding == "clean":
        return "clean"
    if finding.startswith("changes"):
        return "changes requested"
    if finding in {"followup", "issue"}:
        return finding
    return "free-text finding" if finding else "(not reviewed)"


def phase_edges(rows: list[TaskRow]) -> Counter[tuple[str, str]]:
    edges: Counter[tuple[str, str]] = Counter()
    for row in rows:
        ladder = [
            str(row[f"phase_{index}"])
            for index in range(7)
            if row.get(f"phase_{index}")
        ]
        if not ladder:
            continue
        reached = min(len(ladder), max(1, int(row.get("phase_i") or 0) + 1))
        walked = ladder[:reached]
        edges[("(filed)", walked[0])] += 1
        edges.update(zip(walked, walked[1:], strict=False))
        if str(row.get("status") or "") == "completed":
            edges[(walked[-1], "(completed)")] += 1
    return edges


def empty(title: str) -> tuple[str, str, str]:
    return (
        title,
        "No matching rows in this snapshot.",
        'flowchart LR\n  empty["no data"]',
    )
