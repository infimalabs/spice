"""Rendering for inline TASK directives embedded in assistant message text.

The serve transcript renders `TASK title=... | project=... | ...` capture
lines as styled task-capture cards rather than raw text. These helpers detect
the directive lines, extract their fields, and render the HTML and plain-text
summaries used by the message builder.
"""

from __future__ import annotations

import html
from collections.abc import Iterator
from typing import Any

from spice.mail.ackgrammar import (
    extract_task_batch_lines_from_text,
    iter_control_lines,
    task_directive_fields,
    task_directive_line,
)
from spice.serve.markdown import render_message_html

# Display order for the capture card; ordering only. Whether a line is a
# directive at all is not decided here -- spice.mail.ackgrammar owns that, so
# this display and the supervisor cannot disagree about which lines act.
_TASK_DIRECTIVE_PRIMARY_FIELDS = ("title", "project", "acceptance")


def _render_message_html_with_task_directives(
    text: str, captured: frozenset[str], *, worktree_id: str | None = None
) -> str:
    if not text or not text.strip():
        return ""
    rendered: list[str] = []
    pending: list[str] = []
    directive_run: list[str] = []

    def flush_pending() -> None:
        nonlocal pending
        if not pending:
            return
        rendered.append(
            render_message_html("\n".join(pending), worktree_id=worktree_id)
        )
        pending = []

    def flush_directives() -> None:
        nonlocal directive_run
        if not directive_run:
            return
        if len(directive_run) == 1:
            rendered.append(directive_run[0])
        else:
            rendered.append(
                f'<div class="task-directive-stack">{"".join(directive_run)}</div>'
            )
        directive_run = []

    for line, directive in _iter_directive_lines(text, captured):
        if directive is None:
            flush_directives()
            pending.append(line)
            continue
        flush_pending()
        directive_run.append(_task_directive_html(directive))
    flush_directives()
    flush_pending()
    return "".join(rendered)


def captured_directive_lines(text: str) -> frozenset[str]:
    """Return the directives the supervisor reads out of a whole message.

    Resolved before the message is split, because splitting is what destroys
    the context recognition depends on: a segment body begins where its
    ACK/NACK header was cut away, so a marker that sat mid-header is left
    opening the line. Reading the undivided message once answers that, and
    the display consults the answer while rendering the pieces.
    """
    return frozenset(extract_task_batch_lines_from_text(text))


def _iter_directive_lines(
    text: str, captured: frozenset[str]
) -> Iterator[tuple[str, dict[str, Any] | None]]:
    """Pair each line with its directive, or None when the line does not act.

    Two authorities must agree before a card appears, because this text is a
    piece the reducer cut. The local walk drops a directive that is merely
    being shown -- fenced, quoted, indented, or in rendered source context.
    `captured` drops one this piece no longer has the context to judge: a
    segment body opens where its header was cut away, so a marker that sat
    mid-header is left looking like it opens the line.
    """
    for line, suppressed in iter_control_lines(text):
        directive = None if suppressed else _task_directive_from_line(line, captured)
        yield line, directive


def _display_text_with_task_directives(text: str, captured: frozenset[str]) -> str:
    lines = [
        line if directive is None else _task_directive_summary(directive)
        for line, directive in _iter_directive_lines(text, captured)
    ]
    return "\n".join(lines).strip()


def _task_directive_count(text: str, captured: frozenset[str]) -> int:
    return sum(
        1 for _line, directive in _iter_directive_lines(text, captured) if directive
    )


def _task_directive_from_line(
    line: str, captured: frozenset[str]
) -> dict[str, Any] | None:
    fields = task_directive_fields(line)
    if fields is None or task_directive_line(line) not in captured:
        return None
    return {"fields": fields}


def _task_directive_summary(directive: dict[str, Any]) -> str:
    fields = dict(directive.get("fields") or [])
    title = fields.get("title") or fields.get("description") or "inline task"
    project = fields.get("project") or ""
    suffix = f" ({project})" if project else ""
    return f"Task capture: {title}{suffix}"


def _task_directive_html(directive: dict[str, Any]) -> str:
    fields = _ordered_task_directive_fields(directive.get("fields") or [])
    classes = ["task-directive-quote", *_task_directive_extra_classes(directive)]
    class_attr = " ".join(html.escape(class_name, quote=True) for class_name in classes)
    kicker = html.escape(str(directive.get("kicker") or "Task capture"))
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
        f'<blockquote class="{class_attr}">'
        f'<div class="task-directive-kicker">{kicker}</div>'
        f'<dl class="task-directive-properties">{rows}</dl>'
        "</blockquote>"
    )


def _task_directive_extra_classes(directive: dict[str, Any]) -> list[str]:
    raw_classes = directive.get("classes") or []
    if not isinstance(raw_classes, list):
        return []
    result: list[str] = []
    for raw_class in raw_classes:
        class_name = str(raw_class).strip()
        if class_name and all(char.isalnum() or char in "-_" for char in class_name):
            result.append(class_name)
    return result


def _ordered_task_directive_fields(
    fields: list[tuple[str, str]],
) -> list[tuple[str, str]]:
    remaining = _expanded_task_directive_fields(fields)
    ordered: list[tuple[str, str]] = []
    for wanted in _TASK_DIRECTIVE_PRIMARY_FIELDS:
        ordered.extend(field for field in remaining if field[0] == wanted)
        remaining = [field for field in remaining if field[0] != wanted]
    ordered.extend(remaining)
    return ordered


def _expanded_task_directive_fields(
    fields: list[tuple[str, str]],
) -> list[tuple[str, str]]:
    expanded: list[tuple[str, str]] = []
    for key, value in fields:
        values = value.split(" | ") if key == "acceptance" else [value]
        expanded.extend((key, item.strip()) for item in values if item.strip())
    return expanded
