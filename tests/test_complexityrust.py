"""Rust complexity collection preserves comments, chars, and lifetimes."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from spice.studies import complexity
from spice.studies.complexity import (
    collect_complexity_records,
    scan_staged_complexity_violations,
)


def test_rust_comment_apostrophe_cannot_absorb_preceding_routine(tmp_path: Path):
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    source_path = Path("src/session_command.rs")
    source = tmp_path / source_path
    source.parent.mkdir()
    padding = [f"const PAD_{index}: usize = {index};" for index in range(90)]
    source.write_text(
        "\n".join(
            [
                "fn duration_label(duration: std::time::Duration) -> String {",
                "    if duration.subsec_nanos() == 0 {",
                '        return format!("{}s", duration.as_secs());',
                "    }",
                '    let fraction = format!("{:09}", duration.subsec_nanos());',
                "    format!(",
                '        "{}.{fraction}s",',
                "        duration.as_secs(),",
                "        fraction = fraction.trim_end_matches('0')",
                "    )",
                "}",
                "",
                *padding,
                "",
                "fn borrow<'a>(value: &'a str) -> &'a str { value }",
                "",
                "fn innocent() {",
                "    /// the receiver's drop.",
                "    /* the worker's result. */",
                "}",
                "",
            ]
        ),
        encoding="utf-8",
    )
    subprocess.run(["git", "add", source_path], cwd=tmp_path, check=True)
    records = collect_complexity_records([source_path], root=tmp_path)

    assert {
        record.function_name: (record.length, record.ccn) for record in records
    } == {
        "borrow": (1, 1),
        "duration_label": (11, 2),
        "innocent": (4, 1),
    }
    assert (
        scan_staged_complexity_violations(
            [source_path],
            root=tmp_path,
            max_ccn=20,
            max_length=80,
            ccn_flex_limit_value=20,
            length_flex_limit_value=80,
        )
        == []
    )


def test_rust_collection_bypasses_lizard_entirely(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    path = Path("src/lib.rs")
    source = tmp_path / path
    source.parent.mkdir()
    source.write_text("fn ready() {}\n", encoding="utf-8")

    def reject_lizard() -> str:
        raise AssertionError("Rust collection must not enter the lizard backend")

    monkeypatch.setattr(complexity, "require_lizard", reject_lizard)

    assert collect_complexity_records([path], root=tmp_path) == [
        complexity.ComplexityRecord(
            path="src/lib.rs",
            function_name="ready",
            ccn=1,
            length=1,
            nloc=1,
        )
    ]


def test_mixed_collection_sends_only_non_rust_sources_to_lizard(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    rust_path = Path("src/lib.rs")
    python_path = Path("src/app.py")
    (tmp_path / "src").mkdir()
    (tmp_path / rust_path).write_text("fn ready() {}\n", encoding="utf-8")
    (tmp_path / python_path).write_text("def run():\n    pass\n", encoding="utf-8")
    commands: list[list[str]] = []

    def record_lizard(command: list[str], **_kwargs):
        commands.append(command)
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(complexity, "require_lizard", lambda: "/tools/lizard")
    monkeypatch.setattr(complexity, "run_bounded_process_group", record_lizard)

    records = collect_complexity_records([rust_path, python_path], root=tmp_path)

    assert [record.function_name for record in records] == ["ready"]
    assert commands == [["/tools/lizard", "--csv", str(tmp_path / python_path)]]


def test_rust_measurements_match_tree_sitter_complexity_contract(tmp_path: Path):
    path = Path("src/app.rs")
    source = tmp_path / path
    source.parent.mkdir()
    source.write_text(
        """impl Alpha {
    fn new(items: &[bool]) {
        for item in items {
            if *item && items.len() > 1 {}
        }
    }
}
impl Beta {
    fn new() {
    }
}
""",
        encoding="utf-8",
    )

    records = collect_complexity_records([path], root=tmp_path)

    assert [
        (record.function_name, record.ccn, record.length) for record in records
    ] == [("Alpha::new", 4, 5), ("Beta::new", 1, 2)]
