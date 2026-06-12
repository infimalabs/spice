"""Safe transcript markdown rendering."""

from urllib.parse import urlparse

from spice.serve.app import (
    ServeState,
    _resolve_work_tree_link_path,
    _work_tree_proxy_target_from_request,
)
from spice.serve.markdown import render_message_html
from spice.serve.worktrees import WorktreeTarget


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
