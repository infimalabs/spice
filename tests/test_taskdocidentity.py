"""Task-document identity and qualification laws."""

from spice.tasks.markdown.classifier import parse


def test_structure_qualified_slugs_hold_steady_as_the_document_grows() -> None:
    baseline = parse(
        "# Release\n"
        "## Phase Alpha\n"
        "- Update changelog\n"
        "## Phase Beta\n"
        "- Update changelog\n"
    )
    grown = parse(
        "# Release\n"
        "## Phase Alpha\n"
        "- Update changelog\n"
        "## Phase Beta\n"
        "- Update changelog\n"
        "## Phase Gamma\n"
        "- Update changelog\n"
    )

    baseline_identity = {
        baseline.nodes[node.parent].title: node.slug
        for node in baseline.nodes
        if node.title == "Update changelog"
    }
    grown_identity = {
        grown.nodes[node.parent].title: node.slug
        for node in grown.nodes
        if node.title == "Update changelog"
    }
    assert baseline_identity == {
        "Phase Alpha": "phase-alpha--update-changelog",
        "Phase Beta": "phase-beta--update-changelog",
    }
    assert grown_identity == {
        "Phase Alpha": "phase-alpha--update-changelog",
        "Phase Beta": "phase-beta--update-changelog",
        "Phase Gamma": "phase-gamma--update-changelog",
    }


def test_wording_qualifier_is_exactly_the_slug_words_each_rival_lacks() -> None:
    document = parse(
        "# Guides\n"
        "- Update [guide](https://docs.example/red/red/one)\n"
        "- Update [guide](https://docs.example/blue/two)\n"
    )

    qualified = sorted(
        node.slug for node in document.nodes if node.title.startswith("Update")
    )
    assert qualified == ["update-guide--blue-two", "update-guide--red-one"]
    assert [warning[1] for warning in document.warnings] == [
        "dup-qualified",
        "dup-qualified",
    ]


def test_document_root_slug_is_reserved_for_the_synthetic_root() -> None:
    collision = parse("- Document root\n- Ship release\n")
    nested = parse("- Ship release\n- Deploy\n  - Document root\n")

    assert collision.refusals == [
        "title collides with the synthetic root: Document root "
        "(line 1); rename or nest it"
    ]
    synthetic = nested.nodes[nested.root]
    nested_root = next(node for node in nested.nodes if node.title == "Document root")
    assert (synthetic.slug, nested_root.slug) == (
        "document-root",
        "deploy--document-root",
    )


def test_indistinguishable_duplicate_titles_refuse_with_exact_message() -> None:
    document = parse("# Ship\n- Do the thing\n- Do the thing\n")

    assert document.refusals == [
        "duplicate title in document: do-the-thing "
        "(lines 2 and 3; nothing distinguishes them)"
    ]
