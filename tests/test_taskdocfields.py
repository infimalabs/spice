"""Closed task-document fields and acceptance capture."""

from spice.tasks.markdown.classifier import Parser


def test_field_labels_are_case_insensitive_emphasized_and_synonym_complete() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "Acceptance: direct\n"
        "Acceptance Criteria: criteria\n"
        "Success Criteria: success\n"
        "Done When: done\n"
        "Definition Of Done: definition\n"
        "AC: short\n"
        "After: Bootstrap Parser\n"
        "Depends On: phase-1--freeze-main\n"
        "Blocked By: Ship Release\n"
        "Prerequisites: prepare-env\n"
        "PRIORITY: HIGH\n"
        "**Flow:** design, TODO\n"
        "Due: 2026-07-20\n"
        "Deadline: 2026-07-21\n"
        "Tags: parser, core\n"
        "Labels: urgent\n"
    )

    root = parser.nodes[0]
    assert root.acceptance == [
        "direct",
        "criteria",
        "success",
        "done",
        "definition",
        "short",
    ]
    assert root.after_raw == [
        ("bootstrap-parser", 8),
        ("phase-1--freeze-main", 9),
        ("ship-release", 10),
        ("prepare-env", 11),
    ]
    assert (root.priority, root.flow, root.due) == (
        "high",
        ["design", "todo"],
        "2026-07-21",
    )
    assert root.tags == ["parser", "core", "urgent"]
    assert parser.warnings == [(15, "field-repeat", "Due repeated; last value won")]


def test_scalar_field_validation_refuses_and_repeats_keep_the_last_value() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "Priority: high\n"
        "Priority: low\n"
        "Flow: todo\n"
        "Flow: review\n"
        "Due: 2026-07-20\n"
        "Deadline: 2026-07-21\n"
        "## Invalid values\n"
        "Priority: urgent\n"
        "Flow: todo, ship\n"
    )

    root, invalid = parser.nodes
    assert (root.priority, root.flow, root.due) == (
        "low",
        ["review"],
        "2026-07-21",
    )
    assert (invalid.priority, invalid.flow) == ("urgent", ["todo", "ship"])
    assert parser.refusals == [
        "invalid priority: urgent",
        "invalid flow phase: ship",
    ]
    assert [warning[1] for warning in parser.warnings] == [
        "field-repeat",
        "field-repeat",
        "field-repeat",
    ]


def test_bare_acceptance_captures_one_criterion_per_list_item() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "Acceptance:\n\n"
        "- [ ] parser remains deterministic\n"
        "2. graph stays connected\n\n"
        "- Follow-up task\n"
    )

    root, follow_up = parser.nodes
    assert root.acceptance == [
        "parser remains deterministic",
        "graph stays connected",
    ]
    assert (follow_up.title, follow_up.parent) == ("Follow-up task", root.idx)


def test_empty_acceptance_and_sentence_shaped_after_lines_warn_as_prose() -> None:
    parser = Parser()

    parser.feed(
        "# Root\n"
        "Acceptance:\n\n"
        "Description resumes.\n"
        "After: that window closes, we archive the remainder.\n"
    )

    root = parser.nodes[0]
    assert root.description() == (
        "Description resumes.\nAfter: that window closes, we archive the remainder."
    )
    assert [warning[1] for warning in parser.warnings] == [
        "empty-acceptance",
        "after-prose",
    ]
