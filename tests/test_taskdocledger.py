"""Round-trip, fixed-point, and normal-form coverage for the ledger exporter."""

from __future__ import annotations

import pytest

from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import graph_signature
from spice.tasks.markdown.ledger import export_document

# Documents spanning the dialect ladder: an empty page, bare and named lists,
# ordered sequences, cross edges, the full field set in every synonym, deep
# nesting past level six, annotations, frontmatter, and duplicate titles.
CORPUS = {
    "blank": "",
    "single_leaf": "- lonely\n",
    "bare_list": "- alpha\n- beta\n- gamma\n",
    "named_goal": "# Ship the thing\n\n- alpha\n- beta\n",
    "ordered_sequence": "1. first\n2. second\n3. third\n",
    "cross_edges": (
        "# Diamond\n\n"
        "- top\n"
        "- left\n"
        "  After: top\n"
        "- right\n"
        "  After: top\n"
        "- bottom\n"
        "  After: left, right\n"
    ),
    "full_field_set": (
        "# Root\n\n"
        "- work\n"
        "  Acceptance: it compiles\n"
        "  Acceptance: it runs\n"
        "  Priority: high\n"
        "  Flow: todo, doing, done\n"
        "  Due: 2026-08-01\n"
        "  Tags: alpha, beta\n"
        "  This is a description paragraph.\n"
    ),
    "field_synonyms": (
        "# Root\n\n"
        "- work\n"
        "  Done when: it ships\n"
        "  Depends on: other\n"
        "  Deadline: 2026-09-01\n"
        "  Labels: red, green\n"
        "- other\n"
    ),
    "deep_nesting": (
        "# L1\n\n## L2\n\n### L3\n\n#### L4\n\n"
        "##### L5\n\n###### L6\n\n**L7 section**\n\n- deep leaf\n"
    ),
    "nested_lists": "- outer\n  - inner\n    A note.\n    # not a heading\n",
    "blockquote_annotation": "# Root\n\n- task\n  > a quoted note\n  > second line\n",
    "table_annotation": "# Root\n\n- data\n  | a | b |\n  | - | - |\n  | 1 | 2 |\n",
    "frontmatter": "---\ntitle: My Doc\n---\n\n# Root\n\n- alpha\n- beta\n",
    "duplicate_titles": "# Build\n\n## Backend\n\n- deploy\n\n## Frontend\n\n- deploy\n",
}


@pytest.mark.parametrize("source", CORPUS.values(), ids=list(CORPUS))
def test_export_round_trips_to_the_same_graph(source: str) -> None:
    once = parse(source)

    reparsed = parse(export_document(once))

    assert graph_signature(reparsed) == graph_signature(once)


@pytest.mark.parametrize("source", CORPUS.values(), ids=list(CORPUS))
def test_export_reaches_a_byte_identical_fixed_point(source: str) -> None:
    first = export_document(parse(source))

    second = export_document(parse(first))

    assert second == first


def test_empty_document_renders_as_the_empty_string() -> None:
    assert export_document(parse("")) == ""


def test_bare_leaves_pack_tight_as_dash_bullets() -> None:
    assert export_document(parse("- alpha\n- beta\n- gamma\n")) == (
        "- alpha\n- beta\n- gamma\n"
    )


def test_named_goal_renders_an_atx_heading_over_its_leaves() -> None:
    assert (
        export_document(parse("# Ship it\n\n- a\n- b\n")) == "# Ship it\n\n- a\n- b\n"
    )


def test_ordered_run_renders_as_after_edges_not_numbers() -> None:
    rendered = export_document(parse("1. first\n2. second\n3. third\n"))

    assert (
        rendered == "- first\n\n- second\n  After: first\n\n- third\n  After: second\n"
    )


def test_cross_edges_render_as_sorted_after_lines() -> None:
    rendered = export_document(
        parse("# D\n\n- top\n- b\n  After: right, left\n- left\n- right\n")
    )

    assert rendered == (
        "# D\n\n- top\n\n- b\n  After: left, right\n\n- left\n- right\n"
    )


def test_full_field_set_renders_in_canonical_order() -> None:
    rendered = export_document(parse(CORPUS["full_field_set"]))

    assert rendered == (
        "# Root\n\n"
        "- work\n"
        "  Acceptance: it compiles\n"
        "  Acceptance: it runs\n"
        "  Priority: high\n"
        "  Flow: todo, doing, done\n"
        "  Due: 2026-08-01\n"
        "  Tags: alpha, beta\n\n"
        "  This is a description paragraph.\n"
    )


def test_deep_section_past_level_six_renders_as_a_bold_span() -> None:
    rendered = export_document(parse(CORPUS["deep_nesting"]))

    assert rendered == (
        "# L1\n\n## L2\n\n### L3\n\n#### L4\n\n"
        "##### L5\n\n###### L6\n\n**L7 section**\n\n- deep leaf\n"
    )


def test_frontmatter_renders_first() -> None:
    rendered = export_document(parse(CORPUS["frontmatter"]))

    assert rendered.startswith("---\ntitle: My Doc\n---\n\n# Root")


@pytest.mark.parametrize(
    ("marker_source", "canonical_source"),
    [
        ("* a\n* b\n", "- a\n- b\n"),
        ("+ a\n+ b\n", "- a\n- b\n"),
    ],
)
def test_bullet_markers_normalize_to_dashes(
    marker_source: str, canonical_source: str
) -> None:
    assert export_document(parse(marker_source)) == export_document(
        parse(canonical_source)
    )


@pytest.mark.parametrize(
    ("synonym_source", "canonical_source"),
    [
        ("# r\n\n- t\n  Done when: ship\n", "# r\n\n- t\n  Acceptance: ship\n"),
        ("# r\n\n- t\n  Deadline: 2026-09-01\n", "# r\n\n- t\n  Due: 2026-09-01\n"),
        ("# r\n\n- t\n  Labels: x, y\n", "# r\n\n- t\n  Tags: x, y\n"),
    ],
)
def test_field_synonyms_normalize_to_canonical_labels(
    synonym_source: str, canonical_source: str
) -> None:
    assert export_document(parse(synonym_source)) == export_document(
        parse(canonical_source)
    )


def test_after_edges_change_the_rendered_output() -> None:
    with_edge = export_document(parse("# r\n\n- a\n- b\n  After: a\n"))
    without_edge = export_document(parse("# r\n\n- a\n- b\n"))

    assert with_edge != without_edge


def test_nested_leaf_heading_prose_survives_the_flatten_to_a_shallow_bullet() -> None:
    source = "- outer\n  - inner\n    A note.\n    # not a heading\n"

    reparsed = parse(export_document(parse(source)))

    assert graph_signature(reparsed) == graph_signature(parse(source))
