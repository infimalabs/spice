"""Commit-message hygiene: subject capped, body auto-folded, no literal \\n."""

from __future__ import annotations

import re
import sys
import textwrap
from pathlib import Path

from spice.errors import SpiceError
from spice.policy import COMMIT_MESSAGE_WRAP_LIMIT
from spice.policyconfig import resolve_policy

COMMIT_MESSAGE_COMMENT_PREFIX = "#"
COMMIT_MESSAGE_SCISSORS_MARKER = ">8"
COMMIT_MESSAGE_LIST_MARKER_RE = re.compile(r"^(\s*)(?:[-*+]|\d+[.)])\s+")
COMMIT_MESSAGE_TRAILER_RE = re.compile(r"^(?P<key>[A-Za-z0-9-]+): .+")
FORBIDDEN_COMMIT_MESSAGE_TRAILER_KEYS = {"co-authored-by"}
PLACEHOLDER_COMMIT_MESSAGE_SUBJECTS = frozenset(
    {
        "changes",
        "fixup",
        "misc",
        "squash",
        "stuff",
        "temp",
        "temporary",
        "tmp",
        "todo",
        "update",
        "updates",
        "wip",
        "work in progress",
    }
)
PLACEHOLDER_COMMIT_MESSAGE_SUBJECT_TRAILING_CHARS = " \t.,:;!?-_#"


def _commit_message_policy_lines(message_text: str) -> list[tuple[int, str]]:
    policy_lines: list[tuple[int, str]] = []
    for line_number, line in enumerate(message_text.splitlines(), start=1):
        if COMMIT_MESSAGE_SCISSORS_MARKER in line:
            break
        if line.startswith(COMMIT_MESSAGE_COMMENT_PREFIX):
            continue
        policy_lines.append((line_number, line.rstrip()))
    while policy_lines and not policy_lines[-1][1]:
        policy_lines.pop()
    return policy_lines


def _line_exempt_from_wrap(line: str, *, is_subject: bool) -> bool:
    stripped = line.strip()
    return (
        not stripped
        or stripped.startswith("http://")
        or stripped.startswith("https://")
        or (not is_subject and _commit_message_trailer_key(stripped) is not None)
    )


def _commit_message_trailer_key(line: str) -> str | None:
    match = COMMIT_MESSAGE_TRAILER_RE.fullmatch(line.strip())
    if match is None:
        return None
    return match.group("key").lower()


def _placeholder_commit_message_subject(subject: str) -> str:
    normalized = re.sub(r"\s+", " ", subject.strip().casefold())
    return normalized.strip(PLACEHOLDER_COMMIT_MESSAGE_SUBJECT_TRAILING_CHARS)


def validate_commit_message_text(
    message_text: str,
    *,
    wrap_limit: int = COMMIT_MESSAGE_WRAP_LIMIT,
    allowed_trailers: frozenset[str] | None = None,
) -> None:
    lines = _commit_message_policy_lines(message_text)
    if not lines or not lines[0][1].strip():
        raise SpiceError("commit message subject must not be empty")

    failures: list[str] = []
    policy_text = "\n".join(line for _, line in lines)
    if "\\n" in policy_text:
        failures.append(
            "literal '\\n' found; write real newlines with `git commit -F <file>`"
        )

    for line_number, line in lines[1:]:
        key = _commit_message_trailer_key(line)
        if key is None:
            continue
        if key in FORBIDDEN_COMMIT_MESSAGE_TRAILER_KEYS:
            failures.append(
                f"line {line_number} uses forbidden trailer Co-Authored-By; "
                "commit messages must not add co-authors"
            )
            continue
        if allowed_trailers is not None and key not in allowed_trailers:
            allowed = ", ".join(sorted(allowed_trailers)) or "none"
            failures.append(
                f"line {line_number} uses disallowed trailer {key}; "
                f"allowed trailers: {allowed}"
            )

    subject_line_number, subject = lines[0]
    placeholder_subject = _placeholder_commit_message_subject(subject)
    if placeholder_subject in PLACEHOLDER_COMMIT_MESSAGE_SUBJECTS:
        failures.append(
            f"line {subject_line_number} subject {subject.strip()!r} is a "
            "placeholder; write a real subject describing the change"
        )
    if len(subject) > wrap_limit:
        failures.append(
            f"line {subject_line_number} is {len(subject)} chars; "
            f"keep the subject at {wrap_limit} or less: {subject}"
        )

    if failures:
        detail = "\n".join(f"- {failure}" for failure in failures)
        raise SpiceError(
            "commit message hygiene failed:\n"
            f"{detail}\n"
            "Use a message file for multi-line commits, e.g. "
            "`git commit -F /tmp/commit-message.txt`."
        )


def fold_commit_message_text(
    message_text: str,
    *,
    wrap_limit: int = COMMIT_MESSAGE_WRAP_LIMIT,
) -> str:
    lines = message_text.splitlines()
    rendered: list[str] = []
    subject_seen = False
    passthrough = False
    for raw_line in lines:
        if passthrough:
            rendered.append(raw_line)
            continue
        line = raw_line.rstrip()
        if COMMIT_MESSAGE_SCISSORS_MARKER in line:
            passthrough = True
            rendered.append(raw_line)
            continue
        if line.startswith(COMMIT_MESSAGE_COMMENT_PREFIX):
            rendered.append(raw_line)
            continue
        if not subject_seen:
            subject_seen = True
            rendered.append(line)
            continue
        rendered.extend(_fold_body_line(line, wrap_limit=wrap_limit))
    normalized = "\n".join(rendered)
    if message_text.endswith(("\n", "\r")):
        normalized += "\n"
    return normalized


def _fold_body_line(line: str, *, wrap_limit: int) -> list[str]:
    if len(line) <= wrap_limit or _line_exempt_from_wrap(line, is_subject=False):
        return [line]
    marker = COMMIT_MESSAGE_LIST_MARKER_RE.match(line)
    subsequent_indent = " " * marker.end() if marker else ""
    wrapped = textwrap.wrap(
        line,
        width=wrap_limit,
        break_long_words=False,
        break_on_hyphens=False,
        drop_whitespace=True,
        replace_whitespace=False,
        subsequent_indent=subsequent_indent,
    )
    return wrapped or [line]


def handle_commit_msg(message_file: str, repo_root: Path) -> int:
    policy = resolve_policy(repo_root).commit_message
    path = Path(message_file)
    message_text = path.read_text(encoding="utf-8")
    folded_text = fold_commit_message_text(message_text, wrap_limit=policy.wrap_limit)
    if folded_text != message_text:
        path.write_text(folded_text, encoding="utf-8")
        print(
            "spice commit-msg: auto-folded commit message body lines; "
            f"keep body prose wrapped at {policy.wrap_limit} chars",
            file=sys.stderr,
        )
    validate_commit_message_text(
        folded_text,
        wrap_limit=policy.wrap_limit,
        allowed_trailers=policy.allowed_trailers,
    )
    return 0
