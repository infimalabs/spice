from spice.mail.feedback import parse_supervisor_feedback_line, supervisor_feedback_line


def test_supervisor_feedback_line_round_trips_compact_fields():
    line = supervisor_feedback_line(
        "task.error",
        error="batch add rejected: line 2 project depth",
        **{"allowed-project-stems": ["task.unit", "serve.ui"]},
    )

    assert line == (
        "feedback task.error "
        "allowed-project-stems=task.unit,serve.ui "
        "'error=batch add rejected: line 2 project depth'"
    )
    parsed = parse_supervisor_feedback_line(line)
    assert parsed is not None
    assert parsed.kind == "task.error"
    assert parsed.fields == {
        "allowed-project-stems": "task.unit,serve.ui",
        "error": "batch add rejected: line 2 project depth",
    }


def test_supervisor_feedback_parser_rejects_non_feedback_lines():
    assert parse_supervisor_feedback_line("not feedback") is None
    assert parse_supervisor_feedback_line("feedback") is None
    assert parse_supervisor_feedback_line("feedback task.created malformed") is None


def test_supervisor_feedback_line_collapses_multiline_values():
    line = supervisor_feedback_line("task.error", error="first\nsecond")

    assert line == "feedback task.error 'error=first second'"
