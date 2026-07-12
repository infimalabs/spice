"""Single-pass task-document classification and attachment."""

from spice.tasks.markdown.classifier import Parser


def test_parser_dispatches_frontmatter_comments_fences_and_rules() -> None:
    parser = Parser()

    parser.feed(
        "---\nowner: release\n---\n"
        "<!-- ignored\n- hidden task\n-->\n"
        "# Root\n"
        "***\n"
        "```text\n- fenced task\n```\n"
    )

    assert parser.preamble.annotations == ["---\nowner: release\n---"]
    assert [(node.kind, node.title) for node in parser.nodes] == [("heading", "Root")]
    assert parser.nodes[0].annotations == ["```text\n- fenced task\n```"]


def test_setext_and_escaped_prose_take_their_first_matching_rules() -> None:
    parser = Parser()

    parser.feed("Setext root\n===\n\\- prose, not work\nPriority\\: prose\n")

    assert [(node.kind, node.title) for node in parser.nodes] == [
        ("heading", "Setext root")
    ]
    assert parser.nodes[0].description() == "- prose, not work\nPriority: prose"


def test_attachment_uses_content_columns_and_lazy_continuation() -> None:
    parser = Parser()

    parser.feed("# Root\n- Child\n  child body\nlazy child body\n\nroot body\n")

    root, child = parser.nodes
    assert child.parent == root.idx
    assert child.description() == "child body\nlazy child body"
    assert root.description() == "root body"


def test_description_storage_preserves_paragraphs_and_expands_tabs() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n- Child\n  first paragraph\n\n\n\tsecond paragraph\n\nroot paragraph\n"
    )

    root, child = parser.nodes
    assert child.description() == "first paragraph\n\n  second paragraph"
    assert root.description() == "root paragraph"


def test_annotation_lines_coalesce_by_contiguous_shape() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "- Child\n"
        "  > first decision\n"
        "  > second decision\n"
        "  | Name | Value |\n"
        "  | --- | --- |\n"
        "  [first]: https://example.com/first\n"
        "  [second]: https://example.com/second\n"
        "  [Docs](https://example.com/docs)\n"
        "  [API](https://example.com/api)\n"
        "\n"
        "  [Guide](https://example.com/guide)\n"
        "  A sentence merely containing a [link](https://example.com) stays prose.\n"
    )

    root, child = parser.nodes
    assert child.parent == root.idx
    assert child.annotations == [
        "> first decision\n> second decision",
        "| Name | Value |\n| --- | --- |",
        "[first]: https://example.com/first\n[second]: https://example.com/second",
        "[Docs](https://example.com/docs)\n[API](https://example.com/api)",
        "[Guide](https://example.com/guide)",
    ]
    assert child.description() == (
        "A sentence merely containing a [link](https://example.com) stays prose."
    )


def test_indented_code_stays_content_instead_of_minting_structure() -> None:
    parser = Parser()

    parser.feed("# Root\n- Child\n      - code, not work\n")

    root, child = parser.nodes
    assert [(node.kind, node.title) for node in parser.nodes] == [
        ("heading", "Root"),
        ("item", "Child"),
    ]
    assert child.parent == root.idx
    assert child.description() == "    - code, not work"
    assert parser.warnings == [(3, "indent-code", "indented-code line kept as content")]


def test_preamble_content_remains_available_for_root_finalization() -> None:
    parser = Parser()

    parser.feed("Preamble body\n\n# Root\n")

    assert parser.preamble.description() == "Preamble body"
    assert parser.nodes[0].title == "Root"


def test_headings_nest_by_level_and_bold_sections_extend_past_h6() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "### Deep\n"
        "###### Limit\n\n"
        "**Beyond six**\n\n"
        "**Sibling seven**\n"
        "## Reset\n"
    )

    root, deep, limit, beyond, sibling, reset = parser.nodes
    assert [node.level for node in parser.nodes] == [1, 3, 6, 7, 7, 2]
    assert [node.parent for node in parser.nodes] == [
        None,
        root.idx,
        deep.idx,
        limit.idx,
        limit.idx,
        root.idx,
    ]
    assert [warning[1] for warning in parser.warnings] == [
        "bold-heading",
        "bold-heading",
    ]
    assert reset.title == "Reset"


def test_list_indent_nests_items_and_ordered_runs_chain() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "1. First\n"
        "2. Second\n"
        "   - Nested\n"
        "3) Third\n"
        "## History\n"
        "1985. A year, not a task\n"
    )

    root, first, second, nested, third, history = parser.nodes
    assert [first.parent, second.parent, third.parent] == [root.idx] * 3
    assert nested.parent == second.idx
    assert (second.idx, first.idx, "after") in parser.document().edges
    assert (third.idx, second.idx, "after") in parser.document().edges
    assert history.description() == "1985. A year, not a task"
    assert parser.warnings[-1][1] == "ordered-start"


def test_checkboxes_strip_before_nodes_and_field_bullets_feed_their_parent() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "- [ ] Open work\n"
        "- [x] Board-owned status\n"
        "- [x] Priority: high\n"
        "- Acceptance: parser stays deterministic\n"
    )

    root, open_work, board_owned = parser.nodes
    assert [open_work.title, board_owned.title] == ["Open work", "Board-owned status"]
    assert [open_work.checked, board_owned.checked] == [False, True]
    assert root.priority == "high"
    assert root.acceptance == ["parser stays deterministic"]
    assert [warning[1] for warning in parser.warnings] == [
        "checked-discarded",
        "checked-discarded",
    ]


def test_field_sections_feed_the_nearest_shallower_node() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "Acceptance Criteria\n"
        "-------------------\n"
        "- parses headings\n"
        "- preserves nesting\n\n"
        "**Dependencies**\n"
        "- Bootstrap parser\n"
        "## Notes\n"
        "Keep the classifier deterministic.\n"
        "- Review unusual indentation\n"
        "- Preserve warning lines\n"
        "## Child\n"
    )

    root, child = parser.nodes
    assert root.acceptance == ["parses headings", "preserves nesting"]
    assert root.after_raw == [("bootstrap-parser", 8)]
    assert root.description() == "Keep the classifier deterministic."
    assert root.annotations == [
        "> Review unusual indentation\n> Preserve warning lines"
    ]
    assert child.parent == root.idx
    assert [warning[1] for warning in parser.warnings] == [
        "field-section",
        "field-section",
        "bold-heading",
        "field-section",
    ]
