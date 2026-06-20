"""Rendering for inline TASK directives embedded in assistant message text.

The serve transcript renders `TASK title=... | project=... | ...` capture
lines as styled task-capture cards rather than raw text. These helpers detect
the directive lines, extract their fields, and render the HTML and plain-text
summaries used by the message builder.
"""

from __future__ import annotations

import html
from typing import Any

from spice.serve.markdown import render_message_html

_TASK_DIRECTIVE_TOKEN = "TASK"
_TASK_DIRECTIVE_SEPARATOR_CHARS = " \t:-"
_TASK_DIRECTIVE_PRIMARY_FIELDS = ("title", "project", "acceptance")


def _render_message_html_with_task_directives(
    text: str, *, worktree_id: str | None = None
) -> str:
    if not text or not text.strip():
        return ""
    rendered: list[str] = []
    pending: list[str] = []
    for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        directive = _task_directive_from_line(line)
        if directive is None:
            pending.append(line)
            continue
        if pending:
            rendered.append(
                render_message_html("\n".join(pending), worktree_id=worktree_id)
            )
            pending = []
        rendered.append(_task_directive_html(directive))
    if pending:
        rendered.append(
            render_message_html("\n".join(pending), worktree_id=worktree_id)
        )
    return "".join(rendered)


def _display_text_with_task_directives(text: str) -> str:
    lines: list[str] = []
    for line in text.splitlines():
        directive = _task_directive_from_line(line)
        if directive is None:
            lines.append(line)
        else:
            lines.append(_task_directive_summary(directive))
    return "\n".join(lines).strip()


def _strip_task_directive_lines(text: str) -> str:
    lines = [
        line for line in text.splitlines() if _task_directive_from_line(line) is None
    ]
    return "\n".join(lines).strip()


def _task_directive_count(text: str) -> int:
    return sum(1 for line in text.splitlines() if _task_directive_from_line(line))


def _task_directive_from_line(line: str) -> dict[str, Any] | None:
    stripped = line.strip()
    token_end = len(_TASK_DIRECTIVE_TOKEN)
    if not stripped.startswith(_TASK_DIRECTIVE_TOKEN):
        return None
    if len(stripped) > token_end and stripped[token_end] not in (
        _TASK_DIRECTIVE_SEPARATOR_CHARS
    ):
        return None
    payload = stripped[token_end:].lstrip(_TASK_DIRECTIVE_SEPARATOR_CHARS)
    fields = _task_directive_fields(payload)
    if not _task_directive_has_primary_fields(fields):
        return None
    return {"payload": payload, "fields": fields}


def _task_directive_fields(payload: str) -> list[tuple[str, str]]:
    fields: list[tuple[str, str]] = []
    for part in payload.split("|"):
        if "=" not in part:
            continue
        key, value = part.split("=", 1)
        key = " ".join(key.strip().split())
        value = " ".join(value.strip().split())
        if key and value:
            fields.append((key, value))
    return fields


def _task_directive_has_primary_fields(fields: list[tuple[str, str]]) -> bool:
    keys = {key for key, _value in fields}
    return all(key in keys for key in _TASK_DIRECTIVE_PRIMARY_FIELDS)


def _task_directive_summary(directive: dict[str, Any]) -> str:
    fields = dict(directive.get("fields") or [])
    title = fields.get("title") or fields.get("description") or "inline task"
    project = fields.get("project") or ""
    suffix = f" ({project})" if project else ""
    return f"Task capture: {title}{suffix}"


def _task_directive_html(directive: dict[str, Any]) -> str:
    fields = _ordered_task_directive_fields(directive.get("fields") or [])
    rows = "".join(
        '<div class="task-directive-property">'
        f"<dt>{html.escape(label)}</dt>"
        f"<dd>{html.escape(value)}</dd>"
        "</div>"
        for label, value in fields
    )
    if not rows:
        rows = (
            '<div class="task-directive-property">'
            "<dt>status</dt><dd>pending capture</dd>"
            "</div>"
        )
    return (
        '<blockquote class="task-directive-quote">'
        '<div class="task-directive-kicker">Task capture</div>'
        f'<dl class="task-directive-properties">{rows}</dl>'
        "</blockquote>"
    )


def _ordered_task_directive_fields(
    fields: list[tuple[str, str]],
) -> list[tuple[str, str]]:
    remaining = list(fields)
    ordered: list[tuple[str, str]] = []
    for wanted in _TASK_DIRECTIVE_PRIMARY_FIELDS:
        for index, (key, value) in enumerate(remaining):
            if key == wanted:
                ordered.append((key, value))
                remaining.pop(index)
                break
    ordered.extend(remaining)
    return ordered
