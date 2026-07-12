"""Task-document graph model and storage normal forms."""

from spice.tasks import config
from spice.tasks.markdown.dialect import (
    QUALIFIER_SEPARATOR,
    Doc,
    Node,
    graph_signature,
    slugify,
)


def test_graph_signature_covers_node_fields_parenthood_and_cross_edges() -> None:
    root = Node(
        idx=0,
        kind="heading",
        title="Release",
        line=1,
        children=[1],
        acceptance=["release accepted"],
        annotations=["> decision  \n"],
        priority="high",
        flow=["todo", "review"],
        due="2026-08-01",
        tags=["shipping"],
        slug="release",
    )
    root.store_description_line("Root context  ")
    child = Node(
        idx=1,
        kind="heading",
        title="Verify",
        line=4,
        parent=0,
        slug="verify",
    )
    document = Doc(
        nodes=[root, child],
        root=0,
        edges=[(0, 1, "containment"), (1, 0, "after")],
        refusals=[],
        warnings=[],
    )

    assert graph_signature(document) == (
        (
            (
                "release",
                (
                    "Release",
                    None,
                    "high",
                    ("todo", "review"),
                    "2026-08-01",
                    ("shipping",),
                    ("release accepted",),
                    "Root context",
                    ("> decision",),
                ),
            ),
            (
                "verify",
                ("Verify", "release", None, (), None, (), (), "", ()),
            ),
        ),
        frozenset({("verify", "release")}),
    )


def test_description_storage_normalizes_columns_tabs_and_blank_runs() -> None:
    node = Node(idx=0, kind="item", title="Store", line=1, content_col=2)
    for line in ("\tBody  ", "  ", "", "    code  ", ""):
        node.store_description_line(line)

    assert node.desc == ["  Body", "", "", "  code", ""]
    assert node.description() == "  Body\n\n  code"


def test_slugify_joins_lowercased_ascii_words_with_single_hyphens() -> None:
    assert slugify("Deploy 2.0 Now") == "deploy-2-0-now"


def test_slugify_uses_inline_link_text() -> None:
    assert slugify("Read [the guide](https://example.com/v2)") == "read-the-guide"


def test_slugify_link_url_does_not_contribute_so_variant_urls_share_a_slug() -> None:
    # Two titles differing only in link URL slug identically, proving the URL is
    # dropped and only the link text reaches the slug.
    assert slugify("See [docs](http://v1)") == slugify("See [docs](http://v2)")
    assert slugify("See [docs](http://v1)") == "see-docs"


def test_slugify_returns_empty_when_title_has_no_ascii_words() -> None:
    assert slugify("日本語") == ""


def test_repeated_separators_collapse_so_a_natural_slug_holds_no_double_hyphen() -> (
    None
):
    # Runs of non-word characters collapse to single hyphens, so the result is
    # exactly the words joined singly; that is why QUALIFIER_SEPARATOR "--" can
    # never occur inside a natural slug.
    assert slugify("A  ---  B  ==  C") == "a-b-c"


def test_qualifier_separator_is_reserved_double_hyphen() -> None:
    assert QUALIFIER_SEPARATOR == "--"


def test_task_document_identity_udas_are_system_owned_strings() -> None:
    schema = config.uda_schema()

    assert config.TASKDOC_SYSTEM_UDAS == frozenset(
        {config.TASKDOC_ID_UDA, config.TASKDOC_PARENT_UDA}
    )
    assert schema[config.TASKDOC_ID_UDA] == {
        "type": "string",
        "label": config.TASKDOC_ID_UDA,
    }
    assert schema[config.TASKDOC_PARENT_UDA] == {
        "type": "string",
        "label": config.TASKDOC_PARENT_UDA,
    }
