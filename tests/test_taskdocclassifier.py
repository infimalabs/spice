"""Single-pass task-document classification and attachment."""

from spice.tasks.markdown.classifier import Parser, parse
from spice.tasks.markdown.dialect import Doc


def _slug_edges(document: Doc, kind: str) -> set[tuple[str, str]]:
    return {
        (document.nodes[source].slug, document.nodes[target].slug)
        for source, target, edge_kind in document.edges
        if edge_kind == kind
    }


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


def test_empty_and_structureless_documents_refuse_distinctly() -> None:
    assert parse("").refusals == ["task document is empty"]
    assert parse("just some prose\nmore prose\n").refusals == [
        "task document has no nodes (content but no structure)"
    ]
    assert parse("").refusals != parse("just some prose\n").refusals


def test_title_without_ascii_words_refuses_at_its_line() -> None:
    document = parse("# Root\n- 日本語\n")
    assert document.refusals == ["title has no ASCII words: 日本語 (line 2)"]


def test_single_parentless_node_is_the_root() -> None:
    document = parse("# Login hardening\n- Freeze main\n- Cut release\n")
    root = document.nodes[document.root]
    assert root.slug == "login-hardening"
    assert root.parent is None
    assert _slug_edges(document, "containment") == {
        ("login-hardening", "freeze-main"),
        ("login-hardening", "cut-release"),
    }


def test_multiple_parentless_nodes_get_a_synthetic_root_depending_on_each() -> None:
    document = parse(
        "- Fix the session timeout\n"
        "- Update the login form copy\n"
        "- Add a regression test\n"
    )
    root = document.nodes[document.root]
    assert root.slug == "document-root"
    # The synthetic root depends on every parentless leaf, so the whole document
    # is one weakly connected component with the root completing last.
    assert _slug_edges(document, "after") == {
        ("document-root", "fix-the-session-timeout"),
        ("document-root", "update-the-login-form-copy"),
        ("document-root", "add-a-regression-test"),
    }


def test_preamble_content_merges_onto_the_root() -> None:
    document = parse(
        "Tighten the whole login path.\n\n# Login hardening\nBody paragraph.\n"
    )
    root = document.nodes[document.root]
    assert root.slug == "login-hardening"
    assert root.description() == "Tighten the whole login path.\n\nBody paragraph."


def test_duplicated_titles_take_parent_qualified_slugs_and_each_warns() -> None:
    document = parse(
        "# Rollout\n## Phase 1\n- Update changelog\n## Phase 2\n- Update changelog\n"
    )
    qualified = sorted(
        node.slug for node in document.nodes if node.title == "Update changelog"
    )
    assert qualified == ["phase-1--update-changelog", "phase-2--update-changelog"]
    assert qualified[0] != qualified[1]
    assert [warning[1] for warning in document.warnings] == [
        "dup-qualified",
        "dup-qualified",
    ]


def test_siblings_sharing_a_parent_qualify_by_distinguishing_words() -> None:
    # Two bullets differing only in their link URL slug identically, so wording
    # -- links included -- supplies the distinguisher the base slug dropped.
    document = parse(
        "# Guides\n- Update [the guide](http://x/v1)\n- Update [the guide](http://x/v2)\n"
    )
    qualified = sorted(
        node.slug for node in document.nodes if node.title.startswith("Update")
    )
    assert qualified == ["update-the-guide--v1", "update-the-guide--v2"]
    assert qualified[0] != qualified[1]


def test_indistinct_duplicates_refuse_naming_both_lines() -> None:
    document = parse("# Ship\n- Do the thing\n- Do the thing\n")
    assert document.refusals == [
        "duplicate title in document: do-the-thing "
        "(lines 2 and 3; nothing distinguishes them)"
    ]


def test_only_same_parent_siblings_are_indistinct_when_a_far_one_structure_qualifies() -> (
    None
):
    document = parse(
        "# R\n"
        "## Phase 1\n- Update changelog\n- Update changelog\n"
        "## Phase 2\n- Update changelog\n"
    )
    # The Phase 2 occurrence is distinguished by its parent, so only the two
    # Phase 1 siblings collide -- the refusal names lines 3 and 4, not line 6.
    assert document.nodes[-1].slug == "phase-2--update-changelog"
    assert document.refusals == [
        "duplicate title in document: update-changelog "
        "(lines 3 and 4; nothing distinguishes them)"
    ]


def test_parentless_document_root_title_collides_with_the_synthetic_root() -> None:
    document = parse("- Document root\n- Ship it\n")
    assert document.refusals == [
        "title collides with the synthetic root: Document root "
        "(line 1); rename or nest it"
    ]


def test_nested_document_root_title_qualifies_away_from_the_reserved_slug() -> None:
    document = parse("- Ship it\n- Deploy\n  - Document root\n")
    synthetic = document.nodes[document.root]
    nested = next(node for node in document.nodes if node.title == "Document root")
    assert synthetic.slug == "document-root"
    assert nested.slug == "deploy--document-root"
    assert nested.slug != synthetic.slug


def test_after_edges_resolve_and_a_preamble_after_dedupes_containment() -> None:
    # The preamble's `Depends on: freeze-main` merges onto the root, but the root
    # already contains freeze-main, so that dependency edge is dropped as a
    # duplicate of containment -- only the sibling After survives.
    document = parse(
        "Depends on: freeze-main\n\n"
        "# Release\n- Freeze main\n- Cut release\n  After: freeze-main\n"
    )
    assert _slug_edges(document, "after") == {("cut-release", "freeze-main")}


def test_dependency_cycle_refuses_naming_a_slug_on_the_cycle() -> None:
    document = parse("# Root\n- A\n  After: b\n- B\n  After: a\n")
    assert document.refusals == ["dependency cycle at a"]


def test_unknown_after_target_refuses_at_its_line() -> None:
    document = parse("# Root\n- Ship\n  After: nonexistent\n")
    assert document.refusals == ["unknown After target: nonexistent (line 3)"]


def test_bare_after_target_naming_a_duplicated_title_refuses_as_ambiguous() -> None:
    document = parse(
        "# Root\n"
        "## Phase 1\n- Update changelog\n"
        "## Phase 2\n- Update changelog\n"
        "### Ship\nAfter: update-changelog\n"
    )
    assert document.refusals == [
        "ambiguous After target: update-changelog "
        "(title is duplicated; use a qualified slug)"
    ]
