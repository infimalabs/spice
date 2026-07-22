"""Task graph surface: the three graphs the board actually contains.

One set of task rows carries three independent graphs, and none of them is
visible from a task list:

* **origin forest** — ``origin: task:<handle>`` links a task to the task that
  caused it, so the board grows lineage families rooted at operator steering.
* **dependency DAG** — ``depends`` links a blocked task to its blockers, which
  is a plan skeleton and deliberately not the same shape as lineage.
* **review network** — ``review_author`` links a reviewer to the author whose
  task they reviewed, which is the only record of who checks whose work.

Every view is derived from exported task rows. Two sources are off limits by
construction: the ledger export self-warns that it is lossy and does not
round-trip, and the TaskChampion sqlite file is an internal detail of the
backend rather than a supported read surface.
"""

from __future__ import annotations

from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.tasks import identity, tw

TaskRow = dict[str, Any]

#: Views the surface can emit, in presentation order.
VIEWS = ("origin", "depends", "review", "phase")

#: Longest label a node carries before it is cut and ellipsed.
LABEL_LIMIT = 44

_TASK_ORIGIN_PREFIX = "task:"
_UNPLACED = "(unplaced)"


def live_rows(rows: list[TaskRow] | None = None, *, ceiling: str = "") -> list[TaskRow]:
    """Exported rows with deleted and post-ceiling ones dropped.

    Deleted rows are overwhelmingly smoke-test residue and would otherwise
    inflate every count in every view. The inception ceiling is inclusive and
    filters the shared row set before any view can derive edges from it.
    """
    source = tw.export() if rows is None else rows
    stamp = _ceiling_stamp(ceiling)
    return [
        row
        for row in source
        if str(row.get("status") or "") != "deleted"
        and (
            not stamp
            or (
                bool(row_stamp := str(row.get("incepted") or "")) and row_stamp <= stamp
            )
        )
    ]


def _ceiling_stamp(value: str) -> str:
    return identity.incepted_of_handle(value) if value else ""


def handle_of(row: TaskRow) -> str:
    return identity.render_handle(row)


def lane_of(row: TaskRow) -> str:
    """The agent-worktree that filed a task, by basename."""
    worktree = str(row.get("origin_worktree") or "")
    return Path(worktree).name if worktree else _UNPLACED


def origin_edges(rows: list[TaskRow]) -> list[tuple[str, str]]:
    """``(parent_handle, child_handle)`` for every resolvable task origin.

    An origin naming a task that is absent from ``rows`` is dropped rather than
    drawn against a phantom node.
    """
    known = {handle_of(row) for row in rows}
    edges = []
    for row in rows:
        origin = str(row.get("origin") or "")
        if not origin.startswith(_TASK_ORIGIN_PREFIX):
            continue
        parent = origin[len(_TASK_ORIGIN_PREFIX) :]
        child = handle_of(row)
        if parent in known and child:
            edges.append((parent, child))
    return edges


def dependency_edges(rows: list[TaskRow]) -> list[tuple[str, str]]:
    """``(blocker_handle, blocked_handle)`` from the uuid-keyed depends list."""
    by_uuid = {str(row.get("uuid") or ""): handle_of(row) for row in rows}
    edges = []
    for row in rows:
        blocked = handle_of(row)
        if not blocked:
            continue
        for blocker_uuid in row.get("depends") or ():
            blocker = by_uuid.get(str(blocker_uuid), "")
            if blocker:
                edges.append((blocker, blocked))
    return edges


def review_edges(rows: list[TaskRow]) -> Counter[tuple[str, str]]:
    """``(reviewer_lane, author_lane) -> count`` over every reviewed task.

    Reviewers are recorded as thread ids, so each thread is mapped to the lane
    it filed most of its own work from — a thread only ever runs in one
    worktree, and the majority vote absorbs rows filed before the field
    existed.
    """
    votes: dict[str, Counter[str]] = defaultdict(Counter)
    for row in rows:
        thread = str(row.get("origin_thread") or "")
        if thread:
            votes[thread][lane_of(row)] += 1
    lanes = {thread: tally.most_common(1)[0][0] for thread, tally in votes.items()}
    edges: Counter[tuple[str, str]] = Counter()
    for row in rows:
        reviewer = str(row.get("review_author") or "")
        if not reviewer:
            continue
        author = str(row.get("origin_thread") or "")
        edges[(lanes.get(reviewer, _UNPLACED), lanes.get(author, _UNPLACED))] += 1
    return edges


def phase_edges(rows: list[TaskRow]) -> Counter[tuple[str, str]]:
    """``(from_phase, to_phase) -> count`` over the ladder each task walked.

    The ladder is stored as ``phase_0``..``phase_N`` rather than as a history,
    so this is the route the allocator actually took, not the configured flow.
    """
    edges: Counter[tuple[str, str]] = Counter()
    for row in rows:
        ladder = []
        index = 0
        while (step := row.get(f"phase_{index}")) is not None:
            ladder.append(str(step))
            index += 1
        if not ladder:
            continue
        reached = ladder[: int(row.get("phase_i") or 0) + 1]
        edges[("(filed)", reached[0])] += 1
        for before, after in zip(reached, reached[1:], strict=False):
            edges[(before, after)] += 1
        if str(row.get("status") or "") == "completed":
            edges[(reached[-1], "(completed)")] += 1
    return edges


def node_id(handle: str) -> str:
    """A mermaid-legal node id.

    Mermaid ids take word characters only and may not lead with a digit, so a
    handle carrying a dash or a leading stamp digit has to be rewritten.
    """
    cleaned = "".join(c if c.isalnum() or c == "_" else "_" for c in handle)
    if not cleaned:
        raise SpiceError("cannot build a mermaid node id from an empty handle")
    return cleaned if cleaned[0].isalpha() or cleaned[0] == "_" else f"n{cleaned}"


def node_label(text: str, limit: int = LABEL_LIMIT) -> str:
    """Label text safe inside a mermaid double-quoted node.

    Mermaid has no escape form inside a quoted label, so a double quote is
    replaced rather than backslashed, and the pound sign is written as its
    HTML entity because mermaid reads ``#`` as the start of one.
    """
    flat = " ".join(str(text or "").split())
    flat = flat.replace('"', "'").replace("#", "&num;")
    if len(flat) > limit:
        flat = flat[: limit - 1].rstrip() + "…"
    return flat


def _comment(title: str, note: str) -> list[str]:
    """Mermaid-native annotation.

    An HTML comment ahead of the header defeats mermaid's diagram-type
    detection outright, so annotations must use ``%%``.
    """
    return [f"%% {title}", f"%% {note}"]


def render_origin(rows: list[TaskRow]) -> str:
    edges = origin_edges(rows)
    index = {handle_of(row): row for row in rows}
    lines = _comment(
        "Origin forest: which task caused which",
        f"{len(edges)} parent->child links across {len(rows)} tasks.",
    )
    lines.append("flowchart LR")
    for parent, child in edges:
        for handle in (parent, child):
            description = node_label(index.get(handle, {}).get("description", ""))
            lines.append(f'  {node_id(handle)}["{handle}<br/>{description}"]')
        lines.append(f"  {node_id(parent)} --> {node_id(child)}")
    if not edges:
        lines.append('  empty["no task-derived origins on this board"]')
    return "\n".join(lines)


def render_depends(rows: list[TaskRow]) -> str:
    edges = dependency_edges(rows)
    index = {handle_of(row): row for row in rows}
    lines = _comment(
        "Dependency DAG: which task blocks which",
        f"{len(edges)} blocker->blocked links across {len(rows)} tasks.",
    )
    lines.append("flowchart LR")
    for blocker, blocked in edges:
        for handle in (blocker, blocked):
            description = node_label(index.get(handle, {}).get("description", ""))
            lines.append(f'  {node_id(handle)}["{handle}<br/>{description}"]')
        lines.append(f"  {node_id(blocker)} -.blocks.-> {node_id(blocked)}")
    if not edges:
        lines.append('  empty["no dependencies on this board"]')
    return "\n".join(lines)


def render_review(rows: list[TaskRow]) -> str:
    edges = review_edges(rows)
    lines = _comment(
        "Review network: who reviews whose work",
        f"{sum(edges.values())} reviews across {len(edges)} lane pairs.",
    )
    if not edges:
        return "\n".join(
            [*lines, "flowchart LR", '  empty["no reviews on this board"]']
        )
    lines.extend(("sankey-beta", ""))
    for (reviewer, author), count in edges.most_common():
        lines.append(f"{reviewer} reviews,{author} authored,{count}")
    return "\n".join(lines)


def render_phase(rows: list[TaskRow]) -> str:
    edges = phase_edges(rows)
    lines = _comment(
        "Phase ladder: the route tasks actually walked",
        f"{sum(edges.values())} transitions across {len(rows)} tasks.",
    )
    if not edges:
        return "\n".join(
            [*lines, "flowchart LR", '  empty["no phase history on this board"]']
        )
    lines.extend(("sankey-beta", ""))
    for (before, after), count in edges.most_common():
        lines.append(f"{before},{after},{count}")
    return "\n".join(lines)


_RENDERERS = {
    "origin": render_origin,
    "depends": render_depends,
    "review": render_review,
    "phase": render_phase,
}


def render(view: str, rows: list[TaskRow] | None = None, *, ceiling: str = "") -> str:
    """Mermaid source for one view, ready to render unmodified."""
    renderer = _RENDERERS.get(view)
    if renderer is None:
        raise SpiceError(f"unknown graph view {view!r}; choose from {', '.join(VIEWS)}")
    stamp = _ceiling_stamp(ceiling)
    rendered = renderer(live_rows(rows, ceiling=stamp))
    if not stamp:
        return rendered
    lines = rendered.splitlines()
    lines.insert(2, f"%% Snapshot ceiling: incepted <= {stamp}")
    return "\n".join(lines)
