"""Topology and structure cuts for the named board-diagram registry."""

from __future__ import annotations

from collections import Counter, defaultdict, deque

from spice.tasks.graphs import derive as D

TASK_DESCRIPTION_LIMIT = 40
LARGE_NETWORK_VOLUME = 80
LARGE_REVIEW_VOLUME = 100


def project_tree(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    tree: dict[str, Counter[str]] = defaultdict(Counter)
    for row in rows:
        parts = str(row.get("project") or "(none)").lstrip(".").split(".")
        if parts[0] == "agent":
            continue
        tree[parts[0]][parts[1] if len(parts) > 1 else "bare"] += 1
    if not tree:
        return D.empty("The public project namespace")
    lines = ["mindmap", f"  root((spice board<br/>{len(rows)} tasks))"]
    for stem, children in sorted(tree.items(), key=lambda item: -sum(item[1].values())):
        lines.append(f"    {D.slug(stem)}[{D.label(stem)} · {sum(children.values())}]")
        for child, count in children.most_common(9):
            lines.append(f"      {D.slug(stem + child)}[{D.label(child)} · {count}]")
    return (
        "The public project namespace",
        "stem.child hierarchy with task counts; private agent scratch is omitted.",
        "\n".join(lines),
    )


def lifecycle_state(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    edges = D.phase_edges(rows)
    if not edges:
        return D.empty("The ladder as a machine")
    lines = ["stateDiagram-v2", "    direction LR"]
    for (source, target), count in edges.most_common():
        left = "[*]" if source == "(filed)" else D.slug(source)
        right = "[*]" if target == "(completed)" else D.slug(target)
        lines.append(f"    {left} --> {right} : {count}")
    return (
        "The ladder as a machine",
        "Observed transitions only; configured steps never reached are absent.",
        "\n".join(lines),
    )


def _task_node(
    handle: str,
    index: dict[str, D.TaskRow],
    limit: int = TASK_DESCRIPTION_LIMIT,
) -> str:
    description = D.label(index.get(handle, {}).get("description", ""), limit)
    return f'{D.slug(handle)}["{D.label(handle, 24)}<br/>{description}"]'


def _origin_family(rows: list[D.TaskRow], rank: int) -> tuple[str, str, str]:
    index = D.by_handle(rows)
    children, _parents = D.lineage(rows)
    roots = D.lineage_roots(rows)
    if len(roots) < rank:
        return D.empty(f"Origin family #{rank}")
    root = roots[rank - 1]
    nodes = D.subtree(root, children)
    direction = "TD" if len(nodes) <= 6 else "LR"
    lines = [f"flowchart {direction}", f"  {_task_node(root, index)}"]
    for parent in nodes:
        for child in children.get(parent, ()):
            lines.append(f"  {D.slug(parent)} --> {_task_node(child, index)}")
    lines.extend(
        (
            "  classDef root fill:#2a78d6,stroke:#2a78d6,color:#ffffff;",
            f"  class {D.slug(root)} root;",
        )
    )
    title = f"Origin family #{rank}"
    note = f"{root}; {len(nodes)} tasks and depth {D.depth(root, children)}."
    return title, note, "\n".join(lines)


def origin_family_one(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    return _origin_family(rows, 1)


def origin_family_two(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    return _origin_family(rows, 2)


def origin_family_three(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    return _origin_family(rows, 3)


def deepest_spines(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    index = D.by_handle(rows)
    children, _parents = D.lineage(rows)
    roots = sorted(
        D.lineage_roots(rows),
        key=lambda root: (-D.depth(root, children), -len(D.subtree(root, children))),
    )[:4]
    if not roots:
        return D.empty("The four deepest lineage spines")
    direction = "TD" if len(roots) < 3 else "LR"
    lines = [f"flowchart {direction}"]
    for number, root in enumerate(roots, start=1):
        path = [root]
        node = root
        while children.get(node):
            node = max(children[node], key=lambda child: D.depth(child, children))
            path.append(node)
        lines.append(
            f'  {D.slug(root)}["spine {number} · depth {len(path) - 1}<br/>{D.label(root)}"]'
        )
        for left, right in zip(path, path[1:], strict=False):
            lines.append(f"  {D.slug(left)} --> {_task_node(right, index, 32)}")
    return (
        "The four deepest lineage spines",
        "Longest root-to-leaf path per family.",
        "\n".join(lines),
    )


def _dependency_components(rows: list[D.TaskRow]) -> list[list[str]]:
    outgoing: dict[str, set[str]] = defaultdict(set)
    incoming: dict[str, set[str]] = defaultdict(set)
    for source, target in D.dependency_edges(rows):
        outgoing[source].add(target)
        incoming[target].add(source)
    seen: set[str] = set()
    components: list[list[str]] = []
    for first in sorted(set(outgoing) | set(incoming)):
        if first in seen:
            continue
        queue = deque([first])
        group: list[str] = []
        seen.add(first)
        while queue:
            node = queue.popleft()
            group.append(node)
            for neighbor in outgoing[node] | incoming[node]:
                if neighbor not in seen:
                    seen.add(neighbor)
                    queue.append(neighbor)
        components.append(group)
    return sorted(components, key=lambda group: (-len(group), group[0]))


def _dependency_component(rows: list[D.TaskRow], rank: int) -> tuple[str, str, str]:
    groups = _dependency_components(rows)
    title = f"Dependency component #{rank}"
    if len(groups) < rank:
        return D.empty(title)
    group = set(groups[rank - 1])
    index = D.by_handle(rows)
    direction = "TD" if len(group) <= 6 else "LR"
    lines = [f"flowchart {direction}"]
    for source, target in D.dependency_edges(rows):
        if source in group and target in group:
            lines.append(
                f"  {_task_node(source, index, 28)} -.blocks.-> {_task_node(target, index, 28)}"
            )
    return (
        title,
        f"{len(group)} tasks in this connected dependency component.",
        "\n".join(lines),
    )


def dependency_component_one(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    return _dependency_component(rows, 1)


def dependency_component_two(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    return _dependency_component(rows, 2)


def taskdoc_families(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    families: dict[str, list[D.TaskRow]] = defaultdict(list)
    for row in rows:
        parent = row.get("taskdoc_parent") or row.get("taskdoc_id")
        if parent:
            families[str(parent)].append(row)
    if not families:
        return D.empty("Task-document families")
    lines = ["flowchart LR"]
    anchors: list[str] = []
    for number, (parent, members) in enumerate(
        sorted(families.items(), key=lambda item: (-len(item[1]), item[0]))
    ):
        group = f"family_{number}"
        lines.extend(
            (
                f'  subgraph {group}["{D.label(parent, 30)} · {len(members)}"]',
                "    direction TB",
            )
        )
        previous = ""
        for row in members:
            node = D.slug(D.handle(row))
            lines.append(f'    {node}["{D.label(D.handle(row), 22)}"]')
            if previous:
                lines.append(f"    {previous} --> {node}")
            previous = node
        lines.append("  end")
        anchors.append(D.slug(D.handle(members[0])))
    lines.extend(
        f"  {left} ~~~ {right}"
        for left, right in zip(anchors, anchors[1:], strict=False)
    )
    count = sum(len(members) for members in families.values())
    return (
        "Task-document families",
        f"{count} tasks grouped into bounded vertical family columns.",
        "\n".join(lines),
    )


def review_network(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    lanes = D.reviewer_lanes(rows)
    pairs: Counter[tuple[str, str]] = Counter()
    for row in rows:
        reviewer = str(row.get("review_author") or "")
        author = str(row.get("origin_thread") or "")
        if reviewer and author:
            pairs[
                (lanes.get(reviewer, "(unplaced)"), lanes.get(author, "(unplaced)"))
            ] += 1
    if not pairs:
        return D.empty("Cross-agent review network")
    threshold = 8 if sum(pairs.values()) >= LARGE_NETWORK_VOLUME else 1
    self_reviews = Counter(
        {source: count for (source, target), count in pairs.items() if source == target}
    )
    nodes = sorted({name for pair in pairs for name in pair})
    direction = "TD" if len(nodes) <= 4 else "LR"
    lines = [f"flowchart {direction}"]
    lines.extend(
        f'  {D.slug(name)}["{D.label(name)}<br/>self-reviews: {self_reviews[name]}"]'
        for name in nodes
    )
    lines.extend(
        f"  {D.slug(source)} -->|{count}| {D.slug(target)}"
        for (source, target), count in pairs.most_common()
        if source != target and count >= threshold
    )
    return (
        "Cross-agent review network",
        f"Reviewer lane to author lane, cross edges of {threshold} or more.",
        "\n".join(lines),
    )


def stem_quadrant(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    total: Counter[str] = Counter()
    changed: Counter[str] = Counter()
    for row in rows:
        if not row.get("review_author"):
            continue
        key = D.stem(row)
        total[key] += 1
        if D.finding_bucket(row.get("review_finding")) != "clean":
            changed[key] += 1
    if not total:
        return D.empty("Difficulty and volume are different axes")
    minimum = 20 if sum(total.values()) >= LARGE_REVIEW_VOLUME else 1
    keys = [key for key in total if total[key] >= minimum]
    volume = max(total[key] for key in keys)
    bounce = max(changed[key] / total[key] for key in keys) or 1
    lines = [
        "quadrantChart",
        "    title Project surfaces: volume vs rework rate",
        "    x-axis Low volume --> High volume",
        "    y-axis Lands clean --> Bounces back",
        "    quadrant-1 Big and contentious",
        "    quadrant-2 Small and contentious",
        "    quadrant-3 Small and smooth",
        "    quadrant-4 Big and smooth",
    ]
    for key in sorted(keys):
        x = min(0.92, total[key] / volume)
        y = min(0.92, changed[key] / total[key] / bounce)
        lines.append(f"    {D.label(key, 18)}: [{x:.3f}, {y:.3f}]")
    return (
        "Difficulty and volume are different axes",
        f"Review volume and non-clean share for stems with at least {minimum} review(s).",
        "\n".join(lines),
    )


def friction_board(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    oops = [
        row
        for row in rows
        if "oops" in str(row.get("tags") or "")
        or D.stem(row).lstrip(".") == "oops"
        or row.get("oops_kind")
    ]
    if not oops:
        return D.empty("The fleet complaining about its own harness")
    kinds: dict[str, list[D.TaskRow]] = defaultdict(list)
    for row in oops:
        kind = str(row.get("oops_kind") or row.get("kind") or "tooling")
        kinds[kind].append(row)
    lines = ["flowchart LR", f'  oops(["oops triage board<br/>{len(oops)} filed"])']
    for kind, members in sorted(kinds.items(), key=lambda item: -len(item[1])):
        group = D.slug(f"kind_{kind}")
        lines.append(f'  oops --> {group}["{D.label(kind)}<br/>{len(members)}"]')
        for row in members[:5]:
            lines.append(
                f'  {group} --> {D.slug(D.handle(row))}["{D.label(row.get("description"), 40)}"]'
            )
        if len(members) > 5:
            lines.append(f'  {group} --> more_{group}["+{len(members) - 5} more"]')
    return (
        "The fleet complaining about its own harness",
        "Friction entries grouped by kind, with every fan capped and labeled.",
        "\n".join(lines),
    )


def record_schema(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    if not rows:
        return D.empty("One task record, exploded")
    clusters = {
        "IDENTITY": (
            "project",
            "incepted",
            "description",
            "status",
            "priority",
            "entry",
        ),
        "ORIGIN": ("origin", "origin_thread", "origin_worktree", "origin_branch"),
        "PHASE": ("phase", "phase_i", "phase_0", "phase_1", "phase_2"),
        "CLAIM": (
            "claim_by",
            "claim_at",
            "claim_until",
            "claim_branch",
            "claim_thread",
        ),
        "DONE": ("end", "done_ref", "done_head", "done_merge_head", "validation"),
        "REVIEW": ("review_author", "review_at", "review_finding", "review_note"),
        "PLAN": ("acceptance", "task_description", "depends"),
        "DOC": ("taskdoc_id", "taskdoc_parent"),
    }
    present = Counter(key for row in rows for key in row)
    lines = ["flowchart TB", f'  task["TASK<br/>{len(rows)} live records"]']
    cluster_items = list(clusters.items())
    anchors: list[str] = []
    for row_index, start in enumerate(range(0, len(cluster_items), 3)):
        lines.extend(
            (f'  subgraph record_row_{row_index}["record fields"]', "    direction LR")
        )
        row_anchors: list[str] = []
        for name, fields in cluster_items[start : start + 3]:
            node = D.slug(f"cluster_{name}")
            details = "<br/>".join(f"{field} · {present[field]}" for field in fields)
            lines.append(f'    {node}["{name}<br/>{details}"]')
            row_anchors.append(node)
        lines.extend(
            f"    {left} ~~~ {right}"
            for left, right in zip(row_anchors, row_anchors[1:], strict=False)
        )
        lines.append("  end")
        anchors.append(row_anchors[0])
    lines.append(f"  task --> {anchors[0]}")
    lines.extend(
        f"  {left} ~~~ {right}"
        for left, right in zip(anchors, anchors[1:], strict=False)
    )
    return (
        "One task record, exploded",
        f"Eight field clusters with occupancy counts over {len(rows)} live records.",
        "\n".join(lines),
    )


def board_glance(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    if not rows:
        return D.empty("Index diagram")
    origins = D.origin_edges(rows)
    dependencies = D.dependency_edges(rows)
    reviews = sum(bool(row.get("review_author")) for row in rows)
    taskdocs = sum(
        bool(row.get("taskdoc_id") or row.get("taskdoc_parent")) for row in rows
    )
    steps = D.phase_edges(rows)
    lines = [
        "flowchart LR",
        f'  board["spice task board<br/>{len(rows)} live records"]',
        f'  board --> origin["origin forest<br/>{len(origins)} edges"]',
        f'  board --> depends["dependency DAG<br/>{len(dependencies)} edges"]',
        f'  board --> review["review network<br/>{reviews} reviews"]',
        f'  board --> phase["phase ladder<br/>{sum(steps.values())} transitions"]',
        f'  board --> docs["taskdoc tree<br/>{taskdocs} tasks"]',
        "  classDef hub fill:#2a78d6,stroke:#2a78d6,color:#ffffff;",
        "  class board hub;",
    ]
    return (
        "Index diagram",
        "The board's overlaid relationship graphs and their live sizes.",
        "\n".join(lines),
    )


BUILDERS = {
    "03-project-tree-mindmap": project_tree,
    "06-lifecycle-state": lifecycle_state,
    "07-origin-family-1": origin_family_one,
    "07-origin-family-2": origin_family_two,
    "07-origin-family-3": origin_family_three,
    "08-origin-deepest-spines": deepest_spines,
    "10-dependency-component-1": dependency_component_one,
    "10-dependency-component-2": dependency_component_two,
    "11-taskdoc-families": taskdoc_families,
    "13-review-network": review_network,
    "17-stem-quadrant": stem_quadrant,
    "18-friction-oops-board": friction_board,
    "28-record-schema-er": record_schema,
    "32-board-at-a-glance": board_glance,
}
