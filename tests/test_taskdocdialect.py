"""Task-document graph model and storage normal forms."""

from spice.tasks import config
from spice.tasks.markdown.dialect import Doc, Node, graph_signature


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
