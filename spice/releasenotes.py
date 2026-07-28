"""Release range collection and human-readable note rendering."""

from __future__ import annotations

import re
from collections import OrderedDict
from collections.abc import Callable
from dataclasses import dataclass
from subprocess import CompletedProcess

from spice.process.tool import run_tool_command
from spice.tasks import config as task_config

Runner = Callable[..., CompletedProcess[str]]
AncestorCheck = Callable[[str, str], bool]
PROJECT_HEADINGS = {
    "cli": "CLI",
    "ui": "UI",
}
TASK_PHASE_SUBJECT_PREFIX_RE = re.compile(
    r"^(?:design|plan|todo|verify|review)\([^)]+\):\s*",
    re.IGNORECASE,
)
TASK_PHASE_SUBJECT_SUFFIX_RE = re.compile(
    r"\s+\((?:design|plan|verify|review)\)$",
    re.IGNORECASE,
)
REVERT_TARGET_RE = re.compile(r"This reverts commit ([0-9a-f]{7,40})\b")


@dataclass(frozen=True)
class ReleaseRecord:
    commit: str
    subject: str
    project: str
    task_key: str = ""


def tag_ref(tag: str) -> str:
    # Address the tag by its full ref so a same-named branch or shadow ref can
    # never mask the real tag when computing a release range.
    return f"refs/tags/{tag}"


def render_release_range(
    *,
    version: str,
    release_short: str,
    current_tag: str,
    previous_tag: str,
    records: list[ReleaseRecord],
) -> str:
    if previous_tag:
        span = f"{tag_ref(previous_tag)}..{release_short}"
    else:
        span = f"latest first-parent commits ending at {release_short}"
    lines = [
        f"Release range for {version}",
        f"Range: {span}",
        f"Release tag: {current_tag}",
        f"Landed commits: {len(records)}",
        "",
    ]
    if records:
        width = max(len(release_project_key(record.project)) for record in records)
        for record in records:
            key = release_project_key(record.project)
            lines.append(
                f"{shortish_commit(record.commit)}  {key.ljust(width)}  {record.subject}"
            )
    else:
        lines.append("No non-release commits found.")
    lines.append("")
    return "\n".join(lines)


def is_ancestor(candidate: str, commit: str) -> bool:
    """True iff `candidate` is an ancestor of (or equal to) `commit`."""
    result = run_tool_command(
        ["git", "merge-base", "--is-ancestor", candidate, commit],
        policy="release",
        operation="check release ancestry",
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def commit_records(
    previous_tag: str,
    release_commit: str,
    *,
    run: Runner,
    is_ancestor: AncestorCheck = is_ancestor,
) -> list[ReleaseRecord]:
    format_arg = (
        "--format=%H%x1f%s%x1f%(trailers:key=Task-Project,valueonly)"
        "%x1f%(trailers:key=Task-Key,valueonly)%x1f%b%x1e"
    )
    if previous_tag:
        args = [
            "log",
            "--first-parent",
            "--reverse",
            format_arg,
            f"{tag_ref(previous_tag)}..{release_commit}",
        ]
    else:
        args = [
            "log",
            "--first-parent",
            "--reverse",
            "-n",
            "5",
            format_arg,
            release_commit,
        ]

    raw = run(["git", *args], capture=True).stdout
    rows: list[tuple[str, str, str, str, str]] = []
    for raw_record in raw.split("\x1e"):
        raw_record = raw_record.strip("\n")
        if not raw_record:
            continue
        commit, subject, project, task_key, body = (
            raw_record.split("\x1f", 4) + ["", "", "", "", ""]
        )[:5]
        if subject.startswith("release: bump to "):
            continue
        rows.append(
            (commit, subject, project.strip() or "general", task_key.strip(), body)
        )

    # A revert commit and the (first-parent) commit that introduced the work
    # it reverts both landing in this same range is a net no-op for this
    # release; suppress the pair rather than claim credit for shipping
    # something that got undone before it shipped. The revert body names the
    # raw commit it undoes, which usually merged in on a side branch, so find
    # the first-parent commit whose history contains it instead of matching
    # commit hashes directly.
    suppressed_commits: set[str] = set()
    for revert_commit, _subject, _project, _task_key, body in rows:
        match = REVERT_TARGET_RE.search(body)
        if not match:
            continue
        target = match.group(1)
        introduced_by = next(
            (
                commit
                for commit, *_rest in rows
                if commit != revert_commit and is_ancestor(target, commit)
            ),
            None,
        )
        if introduced_by is None:
            continue
        suppressed_commits.add(revert_commit)
        suppressed_commits.add(introduced_by)

    records: list[ReleaseRecord] = []
    latest_index_by_task_key: dict[str, int] = {}
    for commit, subject, project, task_key, _body in rows:
        if commit in suppressed_commits:
            continue
        record = ReleaseRecord(
            commit=commit, subject=subject, project=project, task_key=task_key
        )
        # A task's todo-phase and review-phase merges carry the same
        # Task-Key; keep one highlight per task, at its first position, with
        # the latest (most final) subject.
        if task_key and task_key in latest_index_by_task_key:
            records[latest_index_by_task_key[task_key]] = record
            continue
        if task_key:
            latest_index_by_task_key[task_key] = len(records)
        records.append(record)
    return records


def render_release_notes(
    *,
    version: str,
    release_commit: str,
    release_short: str,
    current_tag: str,
    previous_tag: str,
    records: list[ReleaseRecord],
) -> str:
    groups: OrderedDict[str, OrderedDict[str, list[str]]] = OrderedDict()
    for record in records:
        project_subjects = groups.setdefault(
            release_project_key(record.project), OrderedDict()
        )
        project_subjects.setdefault(
            edited_release_highlight(
                release_note_subject(record.subject, record.task_key, record.project)
            ),
            [],
        ).append(shortish_commit(record.commit))

    lines = [
        "> [!IMPORTANT]",
        "> **Draft release notes — curate Highlights before publishing.** Replace",
        "> the placeholder under _Highlights_ with a short summary, then delete this",
        "> banner. The generated task inventory is already wrapped in the collapsed",
        "> _Task-level changes_ section below; keep that section intact. Omit from",
        "> Highlights any feature that was added and then functionally reverted",
        "> within this same release window — a net-zero change is not a highlight.",
        "",
        "## Highlights",
        "",
        "_Replace this line with a short, curated set of highlights folded from "
        "the changes below._",
        "",
        "<details>",
        "<summary>Task-level changes</summary>",
        "",
        "## Changes by project",
        "",
    ]
    if groups:
        for project, subjects in groups.items():
            lines.extend([f"### {release_project_heading(project)}", ""])
            for highlight, commits in subjects.items():
                # GitHub release pages turn bare repository SHAs into commit links.
                refs = ", ".join(commits)
                lines.append(f"- {highlight} ({refs})")
            lines.append("")
    else:
        lines.extend(["- No non-release commits found.", ""])

    lines.extend(
        [
            "</details>",
            "",
            "## Package Notes",
            "",
            f"- PyPI release: `spice-harness=={version}`",
            f"- Release commit: `{release_short}`",
        ]
    )
    if previous_tag:
        lines.append(f"- Commit range: `{previous_tag}..{release_short}`")
    else:
        lines.append(
            f"- Commit range: latest first-parent commits ending at `{release_short}`"
        )
    lines.append(
        "- Commit source: first-parent history grouped by `Task-Project` metadata"
    )
    if current_tag:
        lines.append(f"- Release tag: `{current_tag}`")
    lines.append("")
    return "\n".join(lines)


def edited_release_highlight(subject: str) -> str:
    raw = " ".join(subject.split()).strip()
    if not raw:
        return "Updated the release."
    replacements = (
        ("fix ", "Fixed "),
        ("prefer ", "Improved "),
        ("add ", "Added "),
        ("expose ", "Added "),
        ("remove ", "Removed "),
        ("update ", "Updated "),
        ("track ", "Tracked "),
        ("document ", "Documented "),
        ("restore ", "Restored "),
        ("clean ", "Cleaned "),
        ("wire ", "Wired "),
        ("make ", "Made "),
    )
    lower = raw.lower()
    for prefix, replacement in replacements:
        if lower.startswith(prefix):
            return punctuate(replacement + raw[len(prefix) :])
    return punctuate(capitalize_first(raw))


def release_note_subject(subject: str, task_key: str = "", project: str = "") -> str:
    trimmed = TASK_PHASE_SUBJECT_PREFIX_RE.sub("", subject, count=1)
    if project:
        project_prefix = f"{task_config.project_stem(project)}: "
        if trimmed.casefold().startswith(project_prefix.casefold()):
            trimmed = trimmed[len(project_prefix) :]
            trimmed = TASK_PHASE_SUBJECT_SUFFIX_RE.sub("", trimmed, count=1)
    if task_key:
        head, sep, last = trimmed.rpartition(" ")
        if sep and head and last.endswith(f"-{task_key}"):
            # Drop the trailing KEY-INCEPTED handle: GitHub already renders each
            # entry's bare short SHA as a commit link, so the handle token is
            # redundant. Keyed on this commit's own Task-Key, never a guess.
            trimmed = head
    return trimmed


def release_project_heading(project: str) -> str:
    if project in PROJECT_HEADINGS:
        return PROJECT_HEADINGS[project]
    parts = [
        segment
        for dotted in project.replace("_", "-").split(".")
        for segment in dotted.split("-")
        if segment
    ]
    if not parts:
        return "General"
    return " ".join(PROJECT_HEADINGS.get(part, part.title()) for part in parts)


def release_project_key(project: str) -> str:
    key = project.strip().lower()
    if not key or key.startswith("agent."):
        return "general"
    return key


def capitalize_first(text: str) -> str:
    first = text[:1]
    return f"{first.upper()}{text[1:]}" if first.islower() else text


def punctuate(text: str) -> str:
    return text if text.endswith((".", "!", "?")) else f"{text}."


def shortish_commit(commit: str) -> str:
    return commit[:7] if len(commit) > 7 else commit
