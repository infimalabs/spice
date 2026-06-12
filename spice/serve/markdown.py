"""Minimal, safe markdown for assistant messages.

The UI needs paragraphs, emphasis, inline/fenced code, links, blockquotes,
and lists — rendered from untrusted text, so everything is HTML-escaped
first and only the renderer introduces tags. This is deliberately a small
subset, not a CommonMark engine.
"""

from __future__ import annotations

import html
import re
from collections.abc import Callable
from urllib.parse import quote, unquote, urlparse

from spice.serve.images import worktree_file_image_url

_FENCE_RE = re.compile(r"^```")
_BOLD_RE = re.compile(r"\*\*([^*]+)\*\*")
_ITALIC_RE = re.compile(r"(?<!\*)\*([^*]+)\*(?!\*)")
_LINK_RE = re.compile(r"(?<!!)\[([^\]]+)\]\(([^)\s]+)\)")
_IMAGE_RE = re.compile(r"!\[([^\]]*)\]\(([^)\s]+)\)")
_BULLET_RE = re.compile(r"^[-*+]\s+")
_ORDERED_RE = re.compile(r"^\d+[.)]\s+")
_HEADING_RE = re.compile(r"^(#{1,6})\s+")
_GITHUB_LINE_SUFFIX_RE = re.compile(r"^(?P<path>.+):(?P<start>\d+)(?:-(?P<end>\d+))?$")
_LINE_ANCHOR_RE = re.compile(r"^L\d+(?:-L\d+)?$")
_MAX_HEADING_LEVEL = 6


def render_message_html(text: str, *, worktree_id: str | None = None) -> str:
    if not text or not text.strip():
        return ""
    blocks = _split_blocks(text)
    return "".join(_render_block(kind, lines, worktree_id) for kind, lines in blocks)


def _split_blocks(text: str) -> list[tuple[str, list[str]]]:
    blocks: list[tuple[str, list[str]]] = []
    current: list[str] = []
    in_fence = False
    for line in text.replace("\r\n", "\n").split("\n"):
        if _FENCE_RE.match(line.strip()):
            if in_fence:
                current.append(line)
                blocks.append(("code", current))
                current = []
                in_fence = False
            else:
                if current:
                    blocks.append(("text", current))
                current = [line]
                in_fence = True
            continue
        if in_fence:
            current.append(line)
            continue
        if not line.strip():
            if current:
                blocks.append(("text", current))
                current = []
            continue
        current.append(line)
    if current:
        blocks.append(("code" if in_fence else "text", current))
    return blocks


def _render_block(kind: str, lines: list[str], worktree_id: str | None) -> str:
    if kind == "code":
        body = "\n".join(lines[1:-1] if len(lines) >= 2 else lines[1:])
        return f"<pre><code>{html.escape(body)}</code></pre>"
    return _render_text_lines(lines, worktree_id)


def _render_text_lines(lines: list[str], worktree_id: str | None) -> str:
    rendered: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        stripped = line.strip()
        if line.lstrip().startswith(">"):
            end = _run_end(lines, index, lambda value: value.lstrip().startswith(">"))
            rendered.append(_render_text_run("quote", lines[index:end], worktree_id))
            index = end
        elif _BULLET_RE.match(stripped):
            end = _run_end(lines, index, lambda value: _BULLET_RE.match(value.strip()))
            rendered.append(_render_text_run("bullet", lines[index:end], worktree_id))
            index = end
        elif _ORDERED_RE.match(stripped):
            end = _run_end(lines, index, lambda value: _ORDERED_RE.match(value.strip()))
            rendered.append(_render_text_run("ordered", lines[index:end], worktree_id))
            index = end
        elif _HEADING_RE.match(line):
            rendered.append(_render_text_run("heading", [line], worktree_id))
            index += 1
        else:
            end = _run_end(
                lines,
                index,
                lambda value: (
                    not (
                        value.lstrip().startswith(">")
                        or _BULLET_RE.match(value.strip())
                        or _ORDERED_RE.match(value.strip())
                        or _HEADING_RE.match(value)
                    )
                ),
            )
            rendered.append(
                _render_text_run("paragraph", lines[index:end], worktree_id)
            )
            index = end
    return "".join(rendered)


def _run_end(lines: list[str], start: int, predicate: Callable[[str], object]) -> int:
    index = start
    while index < len(lines) and predicate(lines[index]):
        index += 1
    return index


def _render_text_run(kind: str, lines: list[str], worktree_id: str | None) -> str:
    if kind == "quote":
        inner = "\n".join(line.lstrip()[1:].lstrip() for line in lines)
        rendered = render_message_html(inner, worktree_id=worktree_id)
        return f"<blockquote>{rendered}</blockquote>"
    if kind == "bullet":
        items = "".join(
            f"<li>{_render_inline(_BULLET_RE.sub('', line.strip()), worktree_id)}</li>"
            for line in lines
        )
        return f"<ul>{items}</ul>"
    if kind == "ordered":
        items = "".join(
            f"<li>{_render_inline(_ORDERED_RE.sub('', line.strip()), worktree_id)}</li>"
            for line in lines
        )
        return f"<ol>{items}</ol>"
    if kind == "heading":
        heading = _HEADING_RE.match(lines[0])
        if heading:
            level = min(_MAX_HEADING_LEVEL, len(heading.group(1)))
            body = _render_inline(_HEADING_RE.sub("", lines[0]), worktree_id)
            return f"<h{level}>{body}</h{level}>"
    body = "<br>".join(_render_inline(line, worktree_id) for line in lines)
    return f"<p>{body}</p>"


def _image_html(alt: str, src: str, worktree_id: str | None) -> str:
    if src.startswith("data:image/"):
        return (
            '<span class="message-image" title="embedded image">'
            f'<img src="{html.escape(src, quote=True)}" '
            f'alt="{html.escape(alt, quote=True)}" loading="lazy" decoding="async">'
            "</span>"
        )
    if src.startswith(("http://", "https://", "/api/")):
        target = src
    elif worktree_id:
        target = worktree_file_image_url(worktree_id, unquote(src))
    else:
        return f"<em>[image: {html.escape(alt)}]</em>"
    escaped_target = html.escape(target, quote=True)
    escaped_alt = html.escape(alt, quote=True)
    return (
        f'<a class="message-image" href="{escaped_target}" '
        f'title="{html.escape(src, quote=True)}" target="_blank" rel="noopener">'
        f'<img src="{escaped_target}" alt="{escaped_alt}" '
        'loading="lazy" decoding="async"></a>'
    )


def work_tree_proxy_url(target: str, *, worktree_id: str | None = None) -> str:
    parsed = urlparse(target)
    if (
        parsed.scheme in {"data", "http", "https", "mailto"}
        or target.startswith("#")
        or target.startswith("/api/")
    ):
        return target
    target = unquote(parsed.path if parsed.scheme == "file" else target)
    target = _githubify_line_link_suffix(target)
    prefix = (
        f"/work/tree/{quote(worktree_id, safe='')}/" if worktree_id else "/work/tree/"
    )
    parts = re.split(r"([?#])", target, maxsplit=1)
    base = parts[0]
    delimiter = ""
    tail = ""
    if len(parts) == 3:
        base, delimiter, tail = parts
    quoted_base = quote(base, safe="/")
    if delimiter == "#" and _LINE_ANCHOR_RE.match(tail):
        return f"{prefix}{quoted_base}#{quote(tail, safe='-L')}"
    return prefix + quoted_base + quote(f"{delimiter}{tail}", safe="/")


def _githubify_line_link_suffix(target: str) -> str:
    if re.match(r"^[A-Za-z]:[\\/]", target):
        return target
    parts = re.split(r"([?#])", target, maxsplit=1)
    if len(parts) == 3:
        base, delimiter, tail = parts
        base = _transform_line_suffix(base)
        return f"{base}{delimiter}{tail}"
    return _transform_line_suffix(target)


def _transform_line_suffix(target: str) -> str:
    match = _GITHUB_LINE_SUFFIX_RE.match(target)
    if not match:
        return target
    path = match.group("path")
    start = match.group("start")
    end = match.group("end")
    if end:
        return f"{path}#L{start}-L{end}"
    return f"{path}#L{start}"


def _render_inline(raw: str, worktree_id: str | None = None) -> str:
    escaped = html.escape(raw, quote=False)
    escaped, code_spans = _stash_inline_code(escaped)
    escaped = _IMAGE_RE.sub(
        lambda match: _image_html(
            html.unescape(match.group(1) or "image"),
            html.unescape(match.group(2) or ""),
            worktree_id,
        ),
        escaped,
    )
    escaped = _BOLD_RE.sub(lambda match: f"<strong>{match.group(1)}</strong>", escaped)
    escaped = _ITALIC_RE.sub(lambda match: f"<em>{match.group(1)}</em>", escaped)
    escaped = _LINK_RE.sub(
        lambda match: _link_html(match, worktree_id),
        escaped,
    )
    return _restore_inline_code(escaped, code_spans)


def _stash_inline_code(escaped: str) -> tuple[str, list[str]]:
    code_spans: list[str] = []
    rendered: list[str] = []
    index = 0
    while index < len(escaped):
        if escaped[index] != "`":
            rendered.append(escaped[index])
            index += 1
            continue
        tick_count = _backtick_run_length(escaped, index)
        close = escaped.find("`" * tick_count, index + tick_count)
        if close < 0:
            rendered.append(escaped[index : index + tick_count])
            index += tick_count
            continue
        body = _normalize_code_span(escaped[index + tick_count : close])
        span_index = len(code_spans)
        code_spans.append(f"<code>{body}</code>")
        rendered.append(f"\ufff0{span_index}\ufff1")
        index = close + tick_count
    return "".join(rendered), code_spans


def _backtick_run_length(text: str, start: int) -> int:
    end = start
    while end < len(text) and text[end] == "`":
        end += 1
    return end - start


def _normalize_code_span(body: str) -> str:
    normalized = body.replace("\n", " ")
    if (
        len(normalized) >= 2
        and normalized[0].isspace()
        and normalized[-1].isspace()
        and any(not char.isspace() for char in normalized)
    ):
        return normalized[1:-1]
    return normalized


def _restore_inline_code(escaped: str, code_spans: list[str]) -> str:
    for index, code in enumerate(code_spans):
        escaped = escaped.replace(f"\ufff0{index}\ufff1", code)
    return escaped


def _link_html(match: re.Match[str], worktree_id: str | None) -> str:
    target = html.escape(_link_target(match.group(2), worktree_id), quote=True)
    return f'<a href="{target}" rel="noopener" target="_blank">{match.group(1)}</a>'


def _link_target(raw_target: str, worktree_id: str | None) -> str:
    return work_tree_proxy_url(html.unescape(raw_target), worktree_id=worktree_id)
