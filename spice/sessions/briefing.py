"""The briefing: rehydrate an agent (or its successor) from the transcript.

The `spice session briefing` output. It answers, mechanically, the questions
a freshly compacted or freshly renewed agent must not guess at: what was
asked, what was last delivered, what to keep doing, what the working set was,
and what steering is pending.
"""

from __future__ import annotations

import subprocess
import tempfile
import time
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

from spice.errors import SpiceError
from spice.mail.inbox import (
    INBOX_RESPONSE_ROW,
    collect_deadlettered_inbox_items,
    collect_inbox_items,
    format_relative_seconds,
    inbox_deadletter_context_rows,
    inbox_item_key,
    relative_time_for_path,
)
from spice.paths import repo_root_from_cwd
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    FILE_BYTE_LIMIT,
    FILE_LOC_LIMIT,
    MAGIC_BASELINE_REF,
    flex_limit,
)
from spice.sessions import records
from spice.sessions.meter import (
    collect_context_meter,
    context_meter_instruction,
)
from spice.sessions.records import (
    TurnRecord,
    collect_commit_records,
    collect_compactions,
    collect_turns,
    is_scaffolding_text,
)
from spice.studies import complexity, fileloc, magicnums
from spice.studies.walk import is_excluded_path

DEFAULT_RECENT_ASKS = 6
DEFAULT_RECENT_FINALS = 3
PREVIEW_CHARS = 200
RECENT_COMMITS_LIMIT = 5
COMMIT_PREVIEW_CHARS = 120
SWEEP_WINDOW_ASKS = 3
WORKING_SET_LIMIT = 10
DEFAULT_BRIEFING_MAX_LINES = 120
DEFAULT_BRIEFING_MAX_BYTES = 20_000
DIRTY_PRESSURE_PREVIEW_LIMIT = 6


@dataclass(frozen=True)
class DirtyComplexityRegression:
    path: str
    function_name: str
    metric: str
    value: int
    active_threshold: int
    baseline_value: int | None


def clip(text: str | None, limit: int = PREVIEW_CHARS) -> str:
    if not text:
        return "-"
    flat = " ".join(text.split())
    if len(flat) <= limit:
        return flat
    return flat[: limit - 1].rstrip() + "…"


def operator_asks(turns: list[TurnRecord]) -> list[tuple[str, str]]:
    """(start_ts, text) for every non-scaffolding user message, oldest first."""
    asks: list[tuple[str, str]] = []
    for turn in turns:
        for text in turn.user_messages:
            if not is_scaffolding_text(text):
                asks.append((turn.start_ts, text))
    return asks


def render_briefing(
    files: list[Path],
    *,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    turn_ids: list[str] | None = None,
    tools: list[str] | None = None,
    max_lines: int | None = DEFAULT_BRIEFING_MAX_LINES,
    max_bytes: int | None = DEFAULT_BRIEFING_MAX_BYTES,
    explain_pruning: bool = False,
) -> str:
    turns = records.filter_turns(
        collect_turns(files),
        start=start,
        end=end,
        contains=contains,
        turn_ids=turn_ids,
        tools=tools,
    )
    compactions = _filter_compactions(
        collect_compactions(files), start=start, end=end, contains=contains
    )
    meter = collect_context_meter(files)
    commits = collect_commit_records(turns)
    asks = operator_asks(turns)
    finals = [(turn.start_ts, text) for turn in turns for text in turn.final_answers]
    lines: list[str] = []

    lines.append("Briefing")
    window_start = turns[0].start_ts if turns else "-"
    window_end = (
        (turns[-1].end_ts or turns[-1].last_activity_ts or turns[-1].start_ts)
        if turns
        else "-"
    )
    lines.append(
        f"  files={', '.join(Path(f).name for f in files)} turns={len(turns)} "
        f"window={window_start} -> {window_end}"
    )
    filter_lines = _active_filter_lines(
        start=start, end=end, contains=contains, turn_ids=turn_ids, tools=tools
    )
    if filter_lines:
        lines.append("Filters")
        lines.extend(filter_lines)

    lines.append("Guidance")
    snapshot = meter.latest_snapshot
    if snapshot is not None:
        lines.append(f"  keep_working={context_meter_instruction('available')}")
    else:
        lines.append(f"  keep_working={context_meter_instruction('unknown')}")

    lines.append("Latest Ask")
    lines.append(f"  {clip(asks[-1][1]) if asks else '-'}")

    if len(asks) > 1:
        lines.append("Recent Asks")
        for ts, text in asks[-DEFAULT_RECENT_ASKS:-1]:
            lines.append(f"  {ts} {clip(text)}")

    lines.append("Latest Final")
    lines.append(f"  {clip(finals[-1][1]) if finals else '-'}")
    if len(finals) > 1:
        lines.append("Recent Finals")
        for ts, text in finals[-DEFAULT_RECENT_FINALS - 1 : -1]:
            lines.append(f"  {ts} {clip(text)}")

    if compactions:
        latest = compactions[-1]
        lines.append("Recovery")
        lines.append(f"  latest_compaction={latest.ts}")
        lines.append(f"  assistant_before={clip(latest.last_assistant_before_text)}")
        lines.append(f"  user_after={clip(latest.first_user_after_text)}")

    lines.append("Activity")
    lines.append(
        "  commands={c} patches={p} errors={e} web_searches={w}".format(
            c=sum(turn.command_count for turn in turns),
            p=sum(turn.patch_count for turn in turns),
            e=sum(turn.error_count for turn in turns),
            w=sum(turn.web_search_count for turn in turns),
        )
    )
    working_set = active_file_order(turns)
    if working_set:
        lines.append("Working Set")
        for path, count in working_set[:WORKING_SET_LIMIT]:
            lines.append(f"  {path} touches={count}")

    if commits:
        lines.append("Recent Commits")
        for record in commits[-RECENT_COMMITS_LIMIT:]:
            lines.append(
                f"  {record.start_ts} {record.sha} {clip(record.line, COMMIT_PREVIEW_CHARS)}"
            )

    lines.extend(_git_posture_lines())
    lines.extend(_inbox_lines())
    return apply_output_budget(
        "\n".join(lines),
        max_lines=max_lines,
        max_bytes=max_bytes,
        explain=explain_pruning,
    )


def active_file_order(turns: list[TurnRecord]) -> list[tuple[str, int]]:
    """The current working set: most-recently-touched first, count attached.

    Recency outranks raw frequency — the file an agent touched last is the
    file it was working on, however many times an older file was edited.
    """
    counts: Counter[str] = Counter()
    last_index: dict[str, int] = {}
    for index, turn in enumerate(turns):
        for path, count in turn.touched_files.items():
            counts[path] += count
            last_index[path] = index
    return [
        (path, counts[path])
        for path in sorted(last_index, key=lambda p: last_index[p], reverse=True)
    ]


def _active_filter_lines(
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
    turn_ids: list[str] | None,
    tools: list[str] | None,
) -> list[str]:
    rows: list[str] = []
    if start:
        rows.append(f"  start={start}")
    if end:
        rows.append(f"  end={end}")
    if contains:
        rows.append(f"  contains={contains}")
    if turn_ids:
        rows.append(f"  turn_ids={', '.join(turn_ids)}")
    if tools:
        rows.append(f"  tools={', '.join(tools)}")
    return rows


def _filter_compactions(
    compactions: list[records.CompactionRecord],
    *,
    start: str | None,
    end: str | None,
    contains: str | None,
) -> list[records.CompactionRecord]:
    needle = (contains or "").lower()
    kept: list[records.CompactionRecord] = []
    for record in compactions:
        if start and record.ts < start:
            continue
        if end and record.ts > end:
            continue
        if needle:
            haystack = "\n".join(
                [
                    record.last_assistant_before_text or "",
                    record.first_user_after_text or "",
                ]
            ).lower()
            if needle not in haystack:
                continue
        kept.append(record)
    return kept


def _git_posture_lines() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return ["Git", "  repo=-"]
    branch = _git_read(repo_root, "branch", "--show-current") or "-"
    upstream = _git_read(
        repo_root, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"
    )
    ahead = behind = "0"
    if upstream:
        delta = _git_read(
            repo_root, "rev-list", "--left-right", "--count", "HEAD...@{u}"
        )
        parts = delta.split()
        if len(parts) == 2:
            ahead, behind = parts
    else:
        upstream = "-"
        ahead = behind = "-"
    dirty_pressure = _build_dirty_worktree_pressure(repo_root=repo_root)
    dirty_count = int(dirty_pressure.get("dirtyPathCount") or 0)
    dirty_text = "clean" if dirty_count == 0 else f"{dirty_count} path(s)"
    lines = [
        "Git",
        f"  branch={branch} upstream={upstream} ahead={ahead} behind={behind}",
        f"  dirty={dirty_text}",
    ]
    if dirty_count:
        lines.extend(_dirty_pressure_lines(dirty_pressure))
    from spice.hooks.install import drifted_hooks

    drifted = drifted_hooks(repo_root)
    if drifted:
        lines.append(
            f"  hooks=stale:{','.join(drifted)} (run spice agent activation to refresh)"
        )
    return lines


def _empty_dirty_worktree_pressure() -> dict[str, object]:
    return {
        "available": True,
        "dirtyPathCount": 0,
        "scannedPathCount": 0,
        "fileCountWithPressure": 0,
        "totalFindings": 0,
        "fileLocFindingCount": 0,
        "complexityRegressionCount": 0,
        "magicRegressionCount": 0,
        "severity": "none",
        "summary": [],
        "summaryOverflow": 0,
        "errors": [],
    }


def _build_dirty_worktree_pressure(*, repo_root: Path) -> dict[str, object]:
    dirty = _dirty_paths(repo_root)
    if not dirty:
        return _empty_dirty_worktree_pressure()
    relevant_paths = [
        path
        for path in dirty
        if not is_excluded_path(path, repo_root=repo_root)
        and (repo_root / path).exists()
    ]
    file_loc_findings, complexity_regressions, magic_regressions, errors = (
        _collect_dirty_pressure_findings(relevant_paths, repo_root=repo_root)
    )
    per_file_rules, ordered_summary = _dirty_pressure_summary(
        file_loc_findings,
        complexity_regressions,
        magic_regressions,
    )
    total_findings = (
        len(file_loc_findings) + len(complexity_regressions) + len(magic_regressions)
    )
    return {
        "available": True,
        "dirtyPathCount": len(dirty),
        "scannedPathCount": len(relevant_paths),
        "fileCountWithPressure": len(per_file_rules),
        "totalFindings": total_findings,
        "fileLocFindingCount": len(file_loc_findings),
        "complexityRegressionCount": len(complexity_regressions),
        "magicRegressionCount": len(magic_regressions),
        "severity": _dirty_pressure_severity(
            file_loc_findings=file_loc_findings,
            complexity_regressions=complexity_regressions,
            magic_regressions=magic_regressions,
            errors=errors,
        ),
        "summary": ordered_summary[:DIRTY_PRESSURE_PREVIEW_LIMIT],
        "summaryOverflow": max(0, len(ordered_summary) - DIRTY_PRESSURE_PREVIEW_LIMIT),
        "errors": errors,
        **_dirty_path_ages(dirty, repo_root=repo_root),
    }


def _dirty_paths(repo_root: Path) -> list[Path]:
    raw_paths: set[Path] = set()
    command_specs = (
        ("diff", "--name-only", "-z", "--diff-filter=ACMRD"),
        ("diff", "--cached", "--name-only", "-z", "--diff-filter=ACMRD"),
        ("ls-files", "--others", "--exclude-standard", "-z"),
    )
    for args in command_specs:
        for raw_path in _git_read_z(repo_root, *args):
            candidate = Path(raw_path)
            if candidate.parts:
                raw_paths.add(candidate)
    return sorted(raw_paths)


def _collect_dirty_pressure_findings(
    relevant_paths: list[Path], *, repo_root: Path
) -> tuple[
    list[fileloc.LocFinding],
    list[DirtyComplexityRegression],
    list[magicnums.MagicFinding],
    list[str],
]:
    errors: list[str] = []
    file_loc_findings: list[fileloc.LocFinding] = []
    complexity_regressions: list[DirtyComplexityRegression] = []
    magic_regressions: list[magicnums.MagicFinding] = []

    try:
        file_loc_findings = fileloc.scan_loc_violations(
            relevant_paths,
            limit=FILE_LOC_LIMIT,
            byte_limit=FILE_BYTE_LIMIT,
            root=repo_root,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("file-loc", exc))

    try:
        complexity_regressions = _scan_dirty_complexity_pressure(
            relevant_paths,
            repo_root=repo_root,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("complexity", exc))

    try:
        magic_regressions = magicnums.detect_magic_regressions(
            relevant_paths,
            root=repo_root,
            baseline_ref=MAGIC_BASELINE_REF,
        )
    except (OSError, SpiceError) as exc:
        errors.append(_dirty_pressure_error("magic-numbers", exc))

    return file_loc_findings, complexity_regressions, magic_regressions, errors


def _scan_dirty_complexity_pressure(
    paths: list[Path], *, repo_root: Path
) -> list[DirtyComplexityRegression]:
    current_paths = [path for path in paths if (repo_root / path).exists()]
    if not current_paths:
        return []
    current_records = complexity.collect_complexity_records(
        current_paths, root=repo_root
    )
    with tempfile.TemporaryDirectory(prefix="spice-complexity-baseline-") as temp_dir:
        temp_root = Path(temp_dir)
        baseline_paths = _materialize_complexity_baseline_paths(
            current_paths,
            repo_root=repo_root,
            temp_root=temp_root,
        )
        baseline_records = complexity.collect_complexity_records(
            baseline_paths,
            root=temp_root,
        )
    return _detect_dirty_complexity_regressions(current_records, baseline_records)


def _materialize_complexity_baseline_paths(
    paths: list[Path], *, repo_root: Path, temp_root: Path
) -> list[Path]:
    materialized: list[Path] = []
    for path in paths:
        result = subprocess.run(
            [
                "git",
                "-C",
                str(repo_root),
                "show",
                f"{MAGIC_BASELINE_REF}:{path.as_posix()}",
            ],
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            continue
        target = temp_root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(result.stdout)
        materialized.append(path)
    return materialized


def _detect_dirty_complexity_regressions(
    current_records: list[complexity.ComplexityRecord],
    baseline_records: list[complexity.ComplexityRecord],
) -> list[DirtyComplexityRegression]:
    baseline_index = _complexity_record_index(baseline_records)
    ccn_flex = flex_limit(COMPLEXITY_MAX_CCN)
    length_flex = flex_limit(COMPLEXITY_MAX_LENGTH)
    regressions: list[DirtyComplexityRegression] = []
    for record in sorted(
        current_records,
        key=lambda item: (item.path, item.function_name),
    ):
        baseline = baseline_index.get(record.key)
        if record.ccn > ccn_flex and (baseline is None or record.ccn > baseline.ccn):
            regressions.append(
                DirtyComplexityRegression(
                    path=record.path,
                    function_name=record.function_name,
                    metric="ccn",
                    value=record.ccn,
                    active_threshold=ccn_flex,
                    baseline_value=baseline.ccn if baseline is not None else None,
                )
            )
        if record.length > length_flex:
            regressions.append(
                DirtyComplexityRegression(
                    path=record.path,
                    function_name=record.function_name,
                    metric="length",
                    value=record.length,
                    active_threshold=length_flex,
                    baseline_value=baseline.length if baseline is not None else None,
                )
            )
    return regressions


def _complexity_record_index(
    records: list[complexity.ComplexityRecord],
) -> dict[tuple[str, str], complexity.ComplexityRecord]:
    index: dict[tuple[str, str], complexity.ComplexityRecord] = {}
    for record in records:
        incumbent = index.get(record.key)
        if incumbent is None or (record.ccn, record.length) > (
            incumbent.ccn,
            incumbent.length,
        ):
            index[record.key] = record
    return index


def _dirty_pressure_summary(
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
) -> tuple[dict[str, set[str]], list[str]]:
    per_file_rules, file_loc_index, complexity_index, magic_index = (
        _index_dirty_pressure_rules(
            file_loc_findings,
            complexity_regressions,
            magic_regressions,
        )
    )
    ordered_summary = [
        f"{path} [{' ,'.join(sorted(labels))}]".replace(" ,", ",")
        for path, labels in sorted(
            per_file_rules.items(),
            key=lambda item: _dirty_pressure_severity_key(
                item[0],
                item[1],
                file_loc_index=file_loc_index,
                complexity_index=complexity_index,
                magic_index=magic_index,
            ),
        )
    ]
    return per_file_rules, ordered_summary


def _index_dirty_pressure_rules(
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
) -> tuple[
    dict[str, set[str]],
    dict[str, list[fileloc.LocFinding]],
    dict[str, list[DirtyComplexityRegression]],
    dict[str, list[magicnums.MagicFinding]],
]:
    per_file_rules: dict[str, set[str]] = {}
    file_loc_index: dict[str, list[fileloc.LocFinding]] = {}
    complexity_index: dict[str, list[DirtyComplexityRegression]] = {}
    magic_index: dict[str, list[magicnums.MagicFinding]] = {}

    def mark(path: str, label: str) -> None:
        per_file_rules.setdefault(path, set()).add(label)

    for finding in file_loc_findings:
        file_loc_index.setdefault(finding.path, []).append(finding)
        mark(finding.path, "file-loc")
    for regression in complexity_regressions:
        complexity_index.setdefault(regression.path, []).append(regression)
        mark(regression.path, f"complexity-{regression.metric}")
    for finding in magic_regressions:
        magic_index.setdefault(finding.path, []).append(finding)
        mark(finding.path, "magic")
    return per_file_rules, file_loc_index, complexity_index, magic_index


def _dirty_pressure_severity_key(
    path: str,
    labels: set[str],
    *,
    file_loc_index: dict[str, list[fileloc.LocFinding]],
    complexity_index: dict[str, list[DirtyComplexityRegression]],
    magic_index: dict[str, list[magicnums.MagicFinding]],
) -> tuple[object, ...]:
    loc_findings = file_loc_index.get(path, [])
    complexity_findings = complexity_index.get(path, [])
    magic_findings = magic_index.get(path, [])
    max_line_over = max(
        (
            max(0, finding.line_count - finding.line_limit)
            for finding in loc_findings
            if finding.over_line_limit
        ),
        default=0,
    )
    max_byte_over = max(
        (
            max(0, finding.byte_count - finding.byte_limit)
            for finding in loc_findings
            if finding.over_byte_limit
        ),
        default=0,
    )
    max_complexity_over = max(
        (
            max(0, regression.value - regression.active_threshold)
            for regression in complexity_findings
        ),
        default=0,
    )
    max_magic_value = max(
        (_magic_literal_abs(finding.literal) for finding in magic_findings),
        default=0.0,
    )
    return (
        -len(loc_findings),
        -len(labels),
        -max_line_over,
        -len(complexity_findings),
        -max_complexity_over,
        -len(magic_findings),
        -max_magic_value,
        -max_byte_over,
        path,
    )


def _dirty_pressure_severity(
    *,
    file_loc_findings: list[fileloc.LocFinding],
    complexity_regressions: list[DirtyComplexityRegression],
    magic_regressions: list[magicnums.MagicFinding],
    errors: list[str],
) -> str:
    if errors:
        return "unknown"
    if file_loc_findings or complexity_regressions:
        return "high"
    if magic_regressions:
        return "medium"
    return "none"


def _dirty_path_ages(paths: list[Path], *, repo_root: Path) -> dict[str, object]:
    now = time.time()
    rows: list[tuple[float, str]] = []
    for path in paths:
        try:
            mtime = (repo_root / path).stat().st_mtime
        except OSError:
            continue
        rows.append((max(0.0, now - mtime), path.as_posix()))
    if not rows:
        return {}
    oldest_age, oldest_path = max(rows, key=lambda row: (row[0], row[1]))
    newest_age, newest_path = min(rows, key=lambda row: (row[0], row[1]))
    return {
        "oldestDirtyAgeSeconds": int(oldest_age),
        "oldestDirtyPath": oldest_path,
        "newestDirtyAgeSeconds": int(newest_age),
        "newestDirtyPath": newest_path,
    }


def _dirty_pressure_lines(pressure: dict[str, object]) -> list[str]:
    if not pressure.get("available"):
        return ["  pressure=unavailable"]
    dirty_paths = int(pressure.get("dirtyPathCount") or 0)
    scanned_paths = int(pressure.get("scannedPathCount") or 0)
    lines = [
        "  pressure "
        f"severity={pressure.get('severity') or 'unknown'} "
        f"findings={int(pressure.get('totalFindings') or 0)} "
        f"files={int(pressure.get('fileCountWithPressure') or 0)} "
        f"scanned={scanned_paths}/{dirty_paths} "
        f"file-loc={int(pressure.get('fileLocFindingCount') or 0)} "
        f"complexity={int(pressure.get('complexityRegressionCount') or 0)} "
        f"magic-numbers={int(pressure.get('magicRegressionCount') or 0)}"
    ]
    age_line = _dirty_pressure_age_line(pressure)
    if age_line:
        lines.append(f"  {age_line}")
    lines.extend(
        f"  pressure_error={error}" for error in pressure.get("errors", []) if error
    )
    lines.extend(
        f"  pressure_file={summary}"
        for summary in pressure.get("summary", [])[:3]
        if isinstance(summary, str)
    )
    overflow = int(pressure.get("summaryOverflow") or 0)
    if overflow:
        lines.append(
            f"  pressure_more={overflow} additional dirty files carry findings"
        )
    return lines


def _dirty_pressure_age_line(pressure: dict[str, object]) -> str | None:
    oldest = pressure.get("oldestDirtyAgeSeconds")
    newest = pressure.get("newestDirtyAgeSeconds")
    if oldest is None or newest is None:
        return None
    return (
        "dirty_age="
        f"oldest={pressure.get('oldestDirtyPath') or '-'}:"
        f"{_format_dirty_age(oldest)} "
        f"newest={pressure.get('newestDirtyPath') or '-'}:"
        f"{_format_dirty_age(newest)}"
    )


def _format_dirty_age(raw_seconds: object) -> str:
    try:
        seconds = float(raw_seconds)
    except (TypeError, ValueError):
        return "unknown"
    return format_relative_seconds(seconds).removesuffix(" ago")


def _dirty_pressure_error(label: str, exc: BaseException) -> str:
    return f"{label}: {clip(str(exc), 120)}"


def _magic_literal_abs(literal: str) -> float:
    try:
        return abs(float(literal.replace("_", "")))
    except ValueError:
        return 0.0


def _git_read(repo_root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
        text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else ""


def _git_read_z(repo_root: Path, *args: str) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return []
    raw = result.stdout.decode("utf-8", errors="surrogateescape")
    return [part for part in raw.split("\0") if part]


def apply_output_budget(
    text: str,
    *,
    max_lines: int | None,
    max_bytes: int | None,
    explain: bool,
) -> str:
    lines = text.splitlines()
    original_lines = len(lines)
    original_bytes = len(text.encode("utf-8"))
    pruned = False
    line_budget = max_lines if max_lines and max_lines > 0 else None
    byte_budget = max_bytes if max_bytes and max_bytes > 0 else None
    reserve = 1 if explain else 0
    if line_budget and len(lines) > line_budget:
        keep = max(1, line_budget - reserve)
        lines = lines[:keep]
        pruned = True

    def pruning_note(retained_lines: int, retained_bytes: int) -> str:
        return (
            "Pruning "
            f"original_lines={original_lines} original_bytes={original_bytes} "
            f"max_lines={line_budget or '-'} max_bytes={byte_budget or '-'} "
            f"retained_lines={retained_lines} retained_content_bytes={retained_bytes}"
        )

    def rendered(with_note: bool) -> str:
        out = list(lines)
        if with_note:
            text_without_note = "\n".join(out)
            retained_bytes = len(text_without_note.encode("utf-8"))
            out.append(pruning_note(len(lines) + 1, retained_bytes))
        return "\n".join(out)

    if byte_budget:
        while lines and len(rendered(explain and pruned).encode("utf-8")) > byte_budget:
            lines.pop()
            pruned = True
        if (
            not lines
            and len(rendered(explain and pruned).encode("utf-8")) > byte_budget
        ):
            return _truncate_to_bytes(rendered(explain and pruned), byte_budget)
    if pruned and explain:
        return rendered(True)
    return "\n".join(lines)


def _truncate_to_bytes(text: str, max_bytes: int) -> str:
    return text.encode("utf-8")[: max(0, max_bytes)].decode("utf-8", errors="ignore")


def _inbox_lines() -> list[str]:
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return []
    items = collect_inbox_items(str(repo_root))
    deadletters = collect_deadlettered_inbox_items(str(repo_root))
    lines = ["Inbox", f"  pending={len(items)}"]
    for item in items:
        lines.append(
            f"  key={inbox_item_key(item.name)} "
            f"age={relative_time_for_path(item.source_path)}"
        )
    if items:
        lines.append(f"  {INBOX_RESPONSE_ROW}")
    if deadletters:
        lines.append(f"  deadlettered={len(deadletters)}")
        lines.extend(f"  {line}" for line in inbox_deadletter_context_rows(deadletters))
    return lines


def render_sweep(
    files: list[Path],
    *,
    count: int,
    start: str | None = None,
    end: str | None = None,
    contains: str | None = None,
    turn_ids: list[str] | None = None,
    tools: list[str] | None = None,
) -> str:
    """Briefings across the last `count` compaction windows, newest last.

    Each window is the span between two compactions: the asks that opened it
    and the final that closed it. A renewed agent reads these to recover not
    just the latest state but the trajectory.
    """
    turns = records.filter_turns(
        collect_turns(files),
        start=start,
        end=end,
        contains=contains,
        turn_ids=turn_ids,
        tools=tools,
    )
    compactions = _filter_compactions(
        collect_compactions(files), start=start, end=end, contains=contains
    )
    boundaries = [record.ts for record in compactions][-max(0, count) :]
    if not boundaries:
        return render_briefing(
            files,
            start=start,
            end=end,
            contains=contains,
            turn_ids=turn_ids,
            tools=tools,
        )
    lines: list[str] = ["Sweep", f"  windows={len(boundaries) + 1} files={len(files)}"]
    edges = ["", *boundaries, "￿"]
    for index in range(len(edges) - 1):
        window_start, window_end = edges[index], edges[index + 1]
        window_turns = [
            turn
            for turn in turns
            if (not window_start or turn.start_ts >= window_start)
            and turn.start_ts < window_end
        ]
        label = window_start or "session start"
        lines.append(f"Window {index} (from {label})")
        asks = operator_asks(window_turns)
        for ts, text in asks[-SWEEP_WINDOW_ASKS:]:
            lines.append(f"  ask {ts} {clip(text)}")
        finals = [
            (turn.start_ts, text)
            for turn in window_turns
            for text in turn.final_answers
        ]
        if finals:
            lines.append(f"  final {finals[-1][0]} {clip(finals[-1][1])}")
        if not asks and not finals:
            lines.append("  (no dialogue in this window)")
    return "\n".join(lines)
