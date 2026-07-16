"""Task-document prose escape law in both directions."""

import pytest

from spice.tasks.markdown.dialect import (
    Node,
    escape_description_line,
    unescape_prose,
)


@pytest.mark.parametrize(
    ("stored", "escaped"),
    [
        ("# Heading", "\\# Heading"),
        ("- item", "\\- item"),
        ("+ item", "\\+ item"),
        ("1. ordered", "1\\. ordered"),
        ("2) ordered", "2\\) ordered"),
        ("```python", "\\```python"),
        ("~~~text", "\\~~~text"),
        ("---", "\\---"),
        ("___", "\\___"),
        ("===", "\\==="),
        ("> quote", "\\> quote"),
        ("| table", "\\| table"),
        ("<!-- note -->", "\\<!-- note -->"),
        ("**Section**", "\\**Section**"),
        ("Acceptance: done", "Acceptance\\: done"),
        ("Acceptance:", "Acceptance\\:"),
        ("**Acceptance:** done", "\\**Acceptance:** done"),
        ("[ref]: https://example.test", "\\[ref]: https://example.test"),
        ("[docs](https://example.test)", "\\[docs](https://example.test)"),
    ],
)
def test_description_escape_round_trips_every_covered_shape(
    stored: str, escaped: str
) -> None:
    assert escape_description_line(stored, 0) == escaped
    assert unescape_prose(escaped) == stored
    assert escape_description_line(unescape_prose(escaped), 0) == escaped


@pytest.mark.parametrize(
    ("authored", "stored"),
    [
        ("  \\# Nested heading", "  # Nested heading"),
        ("12\\) numbered", "12) numbered"),
        ("Priority\\: high", "Priority: high"),
    ],
)
def test_node_stores_escaped_author_lines_as_prose(authored: str, stored: str) -> None:
    node = Node(
        idx=0,
        kind="heading",
        title="Root",
        line=1,
        desc=[unescape_prose(authored)],
    )

    assert node.description() == stored
    assert node.escaped_description_lines(node.content_col) == [authored]
