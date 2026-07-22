"""Time cuts for the named board-diagram registry."""

from __future__ import annotations

from collections import Counter, defaultdict
from datetime import date, datetime

from spice.tasks.graphs import derive as D

CAMPAIGN_LIMIT = 10
INTEGRATION_LIMIT = 24


def campaign_gantt(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    index = D.by_handle(rows)
    children, _parents = D.lineage(rows)
    roots = D.lineage_roots(rows)[:CAMPAIGN_LIMIT]
    lines = [
        "gantt",
        "    title Longest-running origin campaigns",
        "    dateFormat YYYY-MM-DD HH:mm",
        "    axisFormat %m-%d",
    ]
    rendered = 0
    for root in roots:
        members = [index[node] for node in D.subtree(root, children) if node in index]
        starts = [stamp for row in members if (stamp := D.epoch(row, "entry"))]
        ends = [
            stamp
            for row in members
            if (stamp := D.epoch(row, "end") or D.epoch(row, "modified"))
        ]
        if not starts or not ends:
            continue
        lines.append(f"    section {D.label(root, 20)}")
        title = D.label(index[root].get("description", root), 38).replace(":", "-")
        lines.append(
            f"    {title} ({len(members)} tasks) :done, "
            f"{min(starts):%Y-%m-%d %H:%M}, {max(ends):%Y-%m-%d %H:%M}"
        )
        rendered += 1
    if not rendered:
        return D.empty("Campaign spans")
    return (
        "Campaign spans",
        "The ten largest origin families, from first filing to latest board activity.",
        "\n".join(lines),
    )


def era_timeline(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    per_day: dict[date, Counter[str]] = defaultdict(Counter)
    for row in rows:
        stamp = D.epoch(row, "entry")
        if stamp:
            project = str(row.get("project") or "(none)")
            per_day[stamp.date()][".".join(project.split(".")[:2])] += 1
    if not per_day:
        return D.empty("The fleet's agenda, day by day")
    weeks: dict[tuple[int, int], list[date]] = defaultdict(list)
    for day in sorted(per_day):
        calendar = day.isocalendar()
        weeks[(calendar.year, calendar.week)].append(day)
    lines = ["flowchart LR"]
    anchors: list[str] = []
    for index, ((year, week), days) in enumerate(weeks.items()):
        group = f"week_{year}_{week}"
        lines.extend((f'  subgraph {group}["Week {week}"]', "    direction TB"))
        prior = ""
        for day in days:
            node = f"day_{day:%Y_%m_%d}"
            themes = [f"{name} ×{count}" for name, count in per_day[day].most_common(3)]
            lines.append(
                f'    {node}["{day:%b %d}<br/>{D.label(" · ".join(themes), 58)}"]'
            )
            if prior:
                lines.append(f"    {prior} --> {node}")
            prior = node
        lines.append("  end")
        anchors.append(f"day_{days[0]:%Y_%m_%d}")
    lines.extend(
        f"  {left} ~~~ {right}"
        for left, right in zip(anchors, anchors[1:], strict=False)
    )
    return (
        "The fleet's agenda, day by day",
        "Top three projects filed each day, banded by week to enforce a readable aspect ratio.",
        "\n".join(lines),
    )


def integration_gitgraph(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    recent = sorted(
        (row for row in rows if D.epoch(row, "end") and row.get("done_merge_head")),
        key=lambda row: D.epoch(row, "end") or datetime.min,
    )[-INTEGRATION_LIMIT:]
    if not recent:
        return D.empty("Seven lanes braid into one trunk")
    lines = ["gitGraph", '    commit id: "main"']
    branches: set[str] = set()
    for row in recent:
        raw_branch = str(row.get("claim_branch") or D.lane(row))
        branch = D.slug(raw_branch)
        if branch == "main":
            continue
        if branch not in branches:
            lines.append(f"    branch {branch}")
            branches.add(branch)
        lines.extend(
            (
                f"    checkout {branch}",
                f'    commit id: "{D.label(D.handle(row), 32)}"',
                "    checkout main",
                f"    merge {branch}",
            )
        )
    if not branches:
        return D.empty("Seven lanes braid into one trunk")
    return (
        "Seven lanes braid into one trunk",
        f"Real completion order and lane branches for the last {len(recent)} integrations.",
        "\n".join(lines),
    )


BUILDERS = {
    "25-campaign-gantt": campaign_gantt,
    "26-era-timeline": era_timeline,
    "29-integration-gitgraph": integration_gitgraph,
}
