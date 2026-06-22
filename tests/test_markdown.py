"""Safe transcript markdown rendering."""

import re
import subprocess
from urllib.parse import urlparse

from spice.paths import shared_attachment_root
from spice.serve.app import (
    ServeState,
    _resolve_work_tree_link_path,
    _work_tree_proxy_target_from_request,
)
from spice.serve.markdown import render_message_html
from spice.serve.worktree.target import WorktreeTarget


def test_markdown_splits_paragraphs_and_lists_without_blank_lines():
    html = render_message_html(
        "Current state:\n"
        "- `spice agent activation` succeeded.\n"
        "- **task next** reports no task.\n"
        "Done."
    )

    assert "<p>Current state:</p>" in html
    assert "<ul>" in html
    assert "<code>spice agent activation</code> succeeded." in html
    assert "<strong>task next</strong> reports no task." in html
    assert "<p>Done.</p>" in html


def test_markdown_renders_pipe_table_with_inline_cells():
    html = render_message_html(
        "Here is the data:\n"
        "| Commit | Change |\n"
        "| --- | --- |\n"
        "| `abc123` | bumped **priority** |\n"
        "| def456 | moved project |"
    )

    assert "<p>Here is the data:</p>" in html
    assert "<table><thead><tr><th>Commit</th><th>Change</th></tr></thead>" in html
    assert "<tbody>" in html
    assert (
        "<td><code>abc123</code></td><td>bumped <strong>priority</strong></td>" in html
    )
    assert "<td>def456</td><td>moved project</td>" in html
    # The delimiter row must not leak through as a body row.
    assert "---" not in html


def test_markdown_table_requires_matching_delimiter_row():
    # A pipe line followed by a non-delimiter line stays a paragraph, not a table.
    html = render_message_html("a | b\nplain text")

    assert "<table>" not in html
    assert "a | b" in html


def test_markdown_renders_mixed_headings_ordered_lists_and_quotes():
    html = render_message_html(
        "## Plan\n"
        "1. Inspect\n"
        "2. Verify\n"
        "> quoted **steering**\n"
        "Next: [docs](https://example.test/docs)."
    )

    assert "<h2>Plan</h2>" in html
    assert "<ol><li>Inspect</li><li>Verify</li></ol>" in html
    assert "<blockquote><p>quoted <strong>steering</strong></p></blockquote>" in html
    assert (
        '<a href="https://example.test/docs" rel="noopener" target="_blank">docs</a>'
        in html
    )


def test_markdown_preserves_three_nested_quote_levels():
    html = render_message_html("> outer\n> > middle\n> > > inner")

    assert html == (
        "<blockquote><p>outer</p>"
        "<blockquote><p>middle</p>"
        "<blockquote><p>inner</p></blockquote>"
        "</blockquote></blockquote>"
    )


def test_markdown_renders_worktree_file_links_with_line_anchors():
    html = render_message_html(
        "Updated [renderer](spice/serve/markdown.py:30).", worktree_id="main tree"
    )

    assert (
        '<a href="/work/tree/main%20tree/spice/serve/markdown.py#L30" '
        'rel="noopener" target="_blank">renderer</a>'
    ) in html


def test_markdown_renders_absolute_file_links_through_worktree_proxy():
    html = render_message_html(
        "See [skill](/path/to/spice/spice/agent/SKILL.md:20).",
        worktree_id="spice-d943f38a",
    )

    assert (
        '<a href="/work/tree/spice-d943f38a/'
        '/path/to/spice/spice/agent/SKILL.md#L20" '
        'rel="noopener" target="_blank">skill</a>'
    ) in html


def test_markdown_keeps_markdown_link_text_inside_inline_code_as_code():
    html = render_message_html("Shape: `[file](path.py:line)`")

    assert html == "<p>Shape: <code>[file](path.py:line)</code></p>"


def test_markdown_keeps_nested_backtick_code_span_text_literal():
    html = render_message_html(
        "Shape: `` `[file](path.py:line)` ``", worktree_id="lane"
    )

    assert html == "<p>Shape: <code>`[file](path.py:line)`</code></p>"


def test_work_tree_proxy_route_resolves_lane_worktree_file(tmp_path):
    anchor = tmp_path / "anchor"
    worktree = tmp_path / "lane"
    anchor.mkdir()
    worktree.mkdir()
    target = worktree / "spice" / "serve" / "markdown.py"
    target.parent.mkdir(parents=True)
    target.write_text("renderer\n", encoding="utf-8")
    state = ServeState(anchor_root=anchor)
    state.cached_targets = [
        WorktreeTarget(
            id="lane-id",
            repo_root=worktree,
            name="lane",
            branch="main",
        )
    ]

    parsed = urlparse(f"/work/tree/lane-id/{target}")
    selected, route_target = _work_tree_proxy_target_from_request(state, parsed)
    resolved = _resolve_work_tree_link_path(state, route_target or "", selected)

    assert selected is not None
    assert selected.id == "lane-id"
    assert resolved == target


def test_work_tree_proxy_route_resolves_rendered_shared_attachment_link(tmp_path):
    anchor = tmp_path / "anchor"
    worktree = _git_repo(tmp_path / "lane")
    anchor.mkdir()
    shared = shared_attachment_root(worktree) / "digest" / "01-image.png"
    shared.parent.mkdir(parents=True, exist_ok=True)
    shared.write_bytes(b"image")
    state = ServeState(anchor_root=anchor)
    state.cached_targets = [
        WorktreeTarget(
            id="lane-id",
            repo_root=worktree,
            name="lane",
            branch="main",
        )
    ]

    html = render_message_html(
        f"Attachment [paste.png]({shared.as_posix()}).", worktree_id="lane-id"
    )
    href = _first_href(html)
    parsed = urlparse(href)
    selected, route_target = _work_tree_proxy_target_from_request(state, parsed)
    resolved = _resolve_work_tree_link_path(state, route_target or "", selected)

    assert selected is not None
    assert selected.id == "lane-id"
    assert resolved == shared


def test_work_tree_proxy_route_rejects_absolute_file_outside_allowed_roots(tmp_path):
    anchor = tmp_path / "anchor"
    worktree = _git_repo(tmp_path / "lane")
    outside = tmp_path / "outside.png"
    anchor.mkdir()
    outside.write_bytes(b"outside")
    state = ServeState(anchor_root=anchor)
    state.cached_targets = [
        WorktreeTarget(
            id="lane-id",
            repo_root=worktree,
            name="lane",
            branch="main",
        )
    ]

    html = render_message_html(
        f"Attachment [outside.png]({outside.as_posix()}).", worktree_id="lane-id"
    )
    href = _first_href(html)
    parsed = urlparse(href)
    selected, route_target = _work_tree_proxy_target_from_request(state, parsed)
    resolved = _resolve_work_tree_link_path(state, route_target or "", selected)

    assert selected is not None
    assert selected.id == "lane-id"
    assert resolved is None


def _first_href(html: str) -> str:
    match = re.search(r'href="([^"]+)"', html)
    assert match is not None
    return match.group(1)


def _git_repo(path):
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    return path
