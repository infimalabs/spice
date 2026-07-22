"""Flow cuts for the named board-diagram registry."""

from __future__ import annotations

from collections import Counter

from spice.tasks.graphs import derive as D

LARGE_BOARD_SIZE = 40
LARGE_LINEAGE_SIZE = 20
FILTER_BOARD_SIZE = 30


def _name(value: object) -> str:
    return D.label(value).replace(",", ";")


def _sankey(edges: Counter[tuple[str, str]], minimum: int = 1) -> str:
    lines = ["sankey-beta", ""]
    lines.extend(
        f"{_name(source)},{_name(target)},{count}"
        for (source, target), count in edges.most_common()
        if count >= minimum
    )
    if len(lines) == 2:
        return 'flowchart LR\n  empty["no qualifying flows"]'
    return "\n".join(lines)


def phase_flow(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    edges = D.phase_edges(rows)
    if not edges:
        return D.empty("The lifecycle, at true proportion")
    return (
        "The lifecycle, at true proportion",
        "Observed phase transitions; pending tasks stop at their current phase.",
        _sankey(edges),
    )


def agent_to_stem(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    edges = Counter((D.lane(row), D.stem(row)) for row in rows)
    if not edges:
        return D.empty("Which lane owns which surface")
    minimum = 4 if len(rows) >= LARGE_BOARD_SIZE else 1
    return (
        "Which lane owns which surface",
        f"Originating worktree to project stem, edges of {minimum} or more.",
        _sankey(edges, minimum),
    )


def lineage_handoff(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    index = D.by_handle(rows)
    edges = Counter(
        (D.lane(index[parent]), D.lane(index[child]))
        for parent, child in D.origin_edges(rows)
        if parent in index and child in index
    )
    if not edges:
        return D.empty("Work crosses lanes")
    minimum = 2 if sum(edges.values()) >= LARGE_LINEAGE_SIZE else 1
    staged = Counter(
        {
            (f"{source} files", f"{target} follows"): count
            for (source, target), count in edges.items()
        }
    )
    return (
        "Work crosses lanes",
        f"Origin parent's worktree to child's worktree, edges of {minimum} or more.",
        _sankey(staged, minimum),
    )


def review_matrix(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    lanes = D.reviewer_lanes(rows)
    edges: Counter[tuple[str, str]] = Counter()
    for row in rows:
        reviewer = str(row.get("review_author") or "")
        author = str(row.get("origin_thread") or "")
        if reviewer and author:
            left = lanes.get(reviewer, "(unplaced)")
            right = lanes.get(author, "(unplaced)")
            edges[(f"{left} reviews", f"{right} authored")] += 1
    if not edges:
        return D.empty("Peer review is a real economy")
    return (
        "Peer review is a real economy",
        "Every reviewer-author lane pair, including self-review and without a threshold.",
        _sankey(edges),
    )


def priority_to_stem(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    labels = {"H": "High priority", "M": "Medium priority", "L": "Low priority"}
    edges = Counter(
        (labels.get(str(row.get("priority") or ""), "Unset priority"), D.stem(row))
        for row in rows
    )
    if not edges:
        return D.empty("How urgency distributes across surfaces")
    minimum = 3 if len(rows) >= FILTER_BOARD_SIZE else 1
    return (
        "How urgency distributes across surfaces",
        f"Priority to project stem, edges of {minimum} or more.",
        _sankey(edges, minimum),
    )


def _origin_kind(row: D.TaskRow) -> str:
    origin = str(row.get("origin") or "")
    if origin.startswith("task:"):
        return "derived from a parent task"
    if origin.startswith("ack:"):
        if origin[4:].startswith("2026"):
            return "seeded by operator (timestamp era)"
        return "seeded by operator steering"
    return "self-originated"


def origin_kind(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    edges = Counter((_origin_kind(row), D.stem(row)) for row in rows)
    if not edges:
        return D.empty("Where work comes from")
    minimum = 3 if len(rows) >= FILTER_BOARD_SIZE else 1
    return (
        "Where work comes from",
        f"Origin kind to project stem, edges of {minimum} or more.",
        _sankey(edges, minimum),
    )


BUILDERS = {
    "04-agent-to-stem-sankey": agent_to_stem,
    "05-phase-flow-sankey": phase_flow,
    "12-lineage-handoff-sankey": lineage_handoff,
    "13b-review-matrix-sankey": review_matrix,
    "19-priority-to-stem-sankey": priority_to_stem,
    "30-origin-kind-sankey": origin_kind,
}
