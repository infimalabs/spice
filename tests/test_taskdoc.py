"""Task-document input accepts common agent-authored stream shapes."""

from __future__ import annotations

import io

from spice.tasks import taskdoc


def test_read_document_normalizes_file_bom_and_newlines(tmp_path):
    source = tmp_path / "tasks.md"
    source.write_bytes(b"\xef\xbb\xbf# Root\r\nBody\rNext\n")

    text = taskdoc.read_document(source)

    assert text == "# Root\nBody\nNext\n"


def test_read_document_reads_dash_from_normalized_stdin(monkeypatch):
    monkeypatch.setattr(
        taskdoc.sys,
        "stdin",
        io.StringIO("\ufeff# Piped root\r\nPiped body\r"),
    )

    text = taskdoc.read_document("-")

    assert text == "# Piped root\nPiped body\n"
