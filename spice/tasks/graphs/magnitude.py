"""Magnitude and proportion cuts for the named board-diagram registry."""

from __future__ import annotations

from collections import Counter, defaultdict
from collections.abc import Callable, Sequence
from datetime import date

from spice.tasks.graphs import derive as D

LARGE_REVIEW_VOLUME = 100


def _xy(
    title: str,
    labels: Sequence[object],
    values: Sequence[object],
    axis: str,
    ceiling: float,
    *,
    second: Sequence[object] | None = None,
    second_kind: str = "line",
) -> str:
    ticks = ", ".join(D.quoted(item) for item in labels)
    lines = [
        "xychart-beta",
        f'    title "{D.label(title, 72)}"',
        f"    x-axis [{ticks}]",
        f'    y-axis "{D.label(axis)}" 0 --> {max(1, ceiling):g}',
        "    bar [" + ", ".join(str(item) for item in values) + "]",
    ]
    if second is not None:
        lines.append(
            f"    {second_kind} [" + ", ".join(str(item) for item in second) + "]"
        )
    return "\n".join(lines)


def project_stems(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    counts = Counter(D.stem(row) for row in rows)
    if not counts:
        return D.empty("Where the work lives")
    ordered = counts.most_common()
    body = _xy(
        "Tasks by project stem",
        [name for name, _count in ordered],
        [count for _name, count in ordered],
        "tasks",
        ordered[0][1] * 1.1,
    )
    return "Where the work lives", "Every live task, bucketed by project stem.", body


def agent_worktrees(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    counts = Counter(D.lane(row) for row in rows)
    if not counts:
        return D.empty("Who creates the work")
    ordered = counts.most_common()
    body = _xy(
        "Tasks by originating agent lane",
        [name for name, _count in ordered],
        [count for _name, count in ordered],
        "tasks",
        ordered[0][1] * 1.1,
    )
    return "Who creates the work", "origin_worktree basename for every live task.", body


def ack_seeding(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    seeded = Counter()
    for row in rows:
        origin = str(row.get("origin") or "")
        if origin.startswith("ack:"):
            seeded[origin[4:]] += 1
    if not seeded:
        return D.empty("Steering keys that seeded the most work")
    top = seeded.most_common(14)
    body = _xy(
        "Tasks seeded by one steering key",
        [key[:8] for key, _count in top],
        [count for _key, count in top],
        "tasks seeded",
        top[0][1] * 1.15,
    )
    note = (
        f"Top 14 of {len(seeded)} distinct ack origins; every bar is directly labeled."
    )
    return "Steering keys that seeded the most work", note, body


def review_findings(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    counts = Counter(D.finding_bucket(row.get("review_finding")) for row in rows)
    if not counts:
        return D.empty("What review actually returns")
    lines = ["pie showData", f"title Review outcomes (n={len(rows)})"]
    lines.extend(
        f'    "{D.label(name)}" : {count}' for name, count in counts.most_common()
    )
    return (
        "What review actually returns",
        "review_finding normalized into stable outcome buckets.",
        "\n".join(lines),
    )


def _review_rates(
    rows: list[D.TaskRow], key_of: Callable[[D.TaskRow], str], minimum: int = 1
) -> tuple[Counter[str], Counter[str], list[str]]:
    total: Counter[str] = Counter()
    changed: Counter[str] = Counter()
    for row in rows:
        if not row.get("review_author"):
            continue
        key = key_of(row)
        total[key] += 1
        if D.finding_bucket(row.get("review_finding")) != "clean":
            changed[key] += 1
    order = sorted(
        (key for key in total if total[key] >= minimum),
        key=lambda key: (-changed[key] / total[key], key),
    )
    return total, changed, order


def reviewer_strictness(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    lanes = D.reviewer_lanes(rows)
    total, changed, order = _review_rates(
        rows, lambda row: lanes.get(str(row.get("review_author") or ""), "(unplaced)")
    )
    if not order:
        return D.empty("Not every reviewer is the same reviewer")
    rates = [round(100 * changed[key] / total[key]) for key in order]
    labels = [f"{key} {changed[key]}/{total[key]}" for key in order]
    body = _xy(
        "Percent of reviews sent back", labels, rates, "percent", max(rates) * 1.15
    )
    return (
        "Not every reviewer is the same reviewer",
        "Non-clean share per reviewing lane; ticks carry the raw fraction.",
        body,
    )


def stem_difficulty(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    reviewed = sum(1 for row in rows if row.get("review_author"))
    minimum = 20 if reviewed >= LARGE_REVIEW_VOLUME else 1
    total, changed, order = _review_rates(rows, D.stem, minimum)
    if not order:
        return D.empty("Difficulty is not the same thing as volume")
    rates = [round(100 * changed[key] / total[key]) for key in order]
    labels = [f"{key} {changed[key]}/{total[key]}" for key in order]
    body = _xy(
        "Reviews returning changes by stem", labels, rates, "percent", max(rates) * 1.15
    )
    return (
        "Difficulty is not the same thing as volume",
        f"Non-clean share per stem with at least {minimum} review(s).",
        body,
    )


def _daily(rows: list[D.TaskRow]) -> tuple[list[date], Counter[date], Counter[date]]:
    filed: Counter[date] = Counter()
    done: Counter[date] = Counter()
    for row in rows:
        if stamp := D.epoch(row, "entry"):
            filed[stamp.date()] += 1
        if stamp := D.epoch(row, "end"):
            done[stamp.date()] += 1
    return sorted(set(filed) | set(done)), filed, done


def daily_throughput(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    days, filed, done = _daily(rows)
    if not days:
        return D.empty("Filing and closing run together")
    peak = max([*filed.values(), *done.values(), 1])
    body = _xy(
        "Daily tasks filed (bar) and completed (line)",
        [day.strftime("%-m/%-d") for day in days],
        [filed[day] for day in days],
        "tasks",
        peak * 1.15,
        second=[done[day] for day in days],
    )
    return (
        "Filing and closing run together",
        f"{len(days)} days, {sum(filed.values())} filed and {sum(done.values())} completed.",
        body,
    )


def cumulative_burnup(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    days, filed, done = _daily(rows)
    if not days:
        return D.empty("Burnup")
    filed_total = done_total = 0
    filed_series: list[int] = []
    done_series: list[int] = []
    for day in days:
        filed_total += filed[day]
        done_total += done[day]
        filed_series.append(filed_total)
        done_series.append(done_total)
    body = _xy(
        "Cumulative filed (bar) and completed (line)",
        [day.strftime("%-m/%-d") for day in days],
        filed_series,
        "tasks",
        max(filed_total, done_total) * 1.08,
        second=done_series,
    )
    return "Burnup", "The gap between the two cumulative curves is live work.", body


def lane_concurrency(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    active: dict[date, set[str]] = defaultdict(set)
    for row in rows:
        stamp = D.epoch(row, "end") or D.epoch(row, "entry")
        if stamp:
            active[stamp.date()].add(D.lane(row))
    if not active:
        return D.empty("Fleet width over time")
    days = sorted(active)
    values = [len(active[day]) for day in days]
    body = _xy(
        "Distinct agent lanes active each day",
        [day.strftime("%-m/%-d") for day in days],
        values,
        "lanes",
        max(values) + 1,
    )
    return (
        "Fleet width over time",
        "Distinct originating worktrees touching each day.",
        body,
    )


def hour_of_day(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    hours = Counter(stamp.hour for row in rows if (stamp := D.epoch(row, "end")))
    if not hours:
        return D.empty("The fleet is nocturnal, and it is not awake alone")
    dial = [(6 + offset) % 24 for offset in range(24)]
    evening = [hours[hour] if hour >= 6 else 0 for hour in dial]
    morning = [hours[hour] if hour < 6 else 0 for hour in dial]
    body = _xy(
        "Completion hour; second series begins at midnight",
        [f"{hour:02d}" for hour in dial],
        evening,
        "tasks completed",
        max(hours.values()) * 1.12,
        second=morning,
        second_kind="bar",
    )
    return (
        "The fleet is nocturnal, and it is not awake alone",
        f"Operator-local completion hour across {sum(hours.values())} completions.",
        body,
    )


_HIST_EDGES = (0, 5, 10, 20, 30, 45, 60, 90, 120, 180, 360, 720, 10**9)
_HIST_NAMES = (
    "<5m",
    "5-10m",
    "10-20m",
    "20-30m",
    "30-45m",
    "45-60m",
    "1-1.5h",
    "1.5-2h",
    "2-3h",
    "3-6h",
    "6-12h",
    ">12h",
)


def _minutes(row: D.TaskRow, start: str) -> float | None:
    began = D.iso(row, start) if start == "claim_at" else D.epoch(row, start)
    ended = D.epoch(row, "end")
    if began and began.tzinfo:
        began = began.replace(tzinfo=None)
    if not began or not ended or ended <= began:
        return None
    return (ended - began).total_seconds() / 60


def _duration(
    rows: list[D.TaskRow], start: str, title: str, note: str
) -> tuple[str, str, str]:
    histogram: Counter[str] = Counter()
    for row in rows:
        value = _minutes(row, start)
        if value is None:
            continue
        for index, name in enumerate(_HIST_NAMES):
            if _HIST_EDGES[index] <= value < _HIST_EDGES[index + 1]:
                histogram[name] += 1
                break
    if not histogram:
        return D.empty(title)
    values = [histogram[name] for name in _HIST_NAMES]
    body = _xy(title, list(_HIST_NAMES), values, "tasks", max(values) * 1.12)
    return title, note, body


def cycle_time(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    return _duration(
        rows, "entry", "How long a task lives", "Filing to completion wall clock."
    )


def final_turnaround(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    return _duration(
        rows,
        "claim_at",
        "The review phase is nearly instantaneous",
        "claim_at is re-stamped each phase, so this measures final-phase turnaround.",
    )


def task_verbs(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    verbs = Counter(
        words[0]
        for row in rows
        if (words := str(row.get("description") or "").split())
        and words[0][:1].isupper()
    )
    if not verbs:
        return D.empty("The imperative mood of the board")
    top = verbs.most_common(16)
    body = _xy(
        "Opening verb of each task title",
        [verb for verb, _count in top],
        [count for _verb, count in top],
        "tasks",
        top[0][1] * 1.12,
    )
    return (
        "The imperative mood of the board",
        "The sixteen most common opening verbs.",
        body,
    )


def title_length(rows: list[D.TaskRow]) -> tuple[str, str, str]:
    edges = (0, 40, 60, 80, 100, 140, 200, 300, 10**9)
    names = ("<40", "40-60", "60-80", "80-100", "100-140", "140-200", "200-300", ">300")
    histogram: Counter[str] = Counter()
    for row in rows:
        length = len(str(row.get("description") or ""))
        for index, name in enumerate(names):
            if edges[index] <= length < edges[index + 1]:
                histogram[name] += 1
                break
    if not histogram:
        return D.empty("Titles are sentences, not labels")
    values = [histogram[name] for name in names]
    body = _xy(
        "Task title length in characters",
        list(names),
        values,
        "tasks",
        max(values) * 1.12,
    )
    return (
        "Titles are sentences, not labels",
        "Character-count distribution of live task titles.",
        body,
    )


BUILDERS = {
    "01-project-stems-xy": project_stems,
    "02-agent-worktrees-xy": agent_worktrees,
    "09-ack-seeding-fanout": ack_seeding,
    "14-review-findings-pie": review_findings,
    "15-reviewer-strictness-xy": reviewer_strictness,
    "16-stem-difficulty-xy": stem_difficulty,
    "20-daily-throughput-xy": daily_throughput,
    "21-cumulative-burnup-xy": cumulative_burnup,
    "22-lane-concurrency-xy": lane_concurrency,
    "23-hour-of-day-xy": hour_of_day,
    "24-cycle-time-xy": cycle_time,
    "24b-final-phase-turnaround-xy": final_turnaround,
    "27-task-verbs-xy": task_verbs,
    "31-title-length-xy": title_length,
}
