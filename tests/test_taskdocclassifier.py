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
