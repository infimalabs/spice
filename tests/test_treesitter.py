from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.studies.magicnums import scan_text_magic_numbers
from spice.studies import treesitter


def test_tree_sitter_seam_parses_csharp_and_javascript_sources():
    csharp = treesitter.parse_source(
        Path("Assets/Scripts/Demo.cs"),
        "public class Demo { private void Run() {} }\n",
    )
    javascript = treesitter.parse_source(
        Path("static/demo.js"),
        "function demo() { return 1; }\n",
    )

    assert csharp is not None
    assert csharp.language == "csharp"
    assert csharp.suffix == ".cs"
    assert csharp.root.type == "compilation_unit"
    assert [child.type for child in csharp.root.children] == ["class_declaration"]
    assert javascript is not None
    assert javascript.language == "javascript"
    assert javascript.suffix == ".js"
    assert javascript.root.type == "program"
    assert [child.type for child in javascript.root.children] == [
        "function_declaration"
    ]


def test_tree_sitter_seam_exposes_suffix_keyed_query_access():
    csharp_query = treesitter.query_for_suffix(
        ".cs", "(class_declaration name: (identifier) @name)"
    )
    javascript_query = treesitter.query_for_suffix(
        ".js", "(function_declaration name: (identifier) @name)"
    )

    assert csharp_query is not None
    assert csharp_query.pattern_count == 1
    assert javascript_query is not None
    assert javascript_query.pattern_count == 1


def test_magic_number_scan_routes_supported_sources_through_tree_sitter_seam(
    monkeypatch,
):
    parsed_suffixes: list[str] = []

    def record_parse(path: Path | str, source: str | bytes):
        suffix = Path(path).suffix
        parsed_suffixes.append(suffix)
        return None

    monkeypatch.setattr(treesitter, "parse_source", record_parse)

    with pytest.raises(SpiceError, match="tree-sitter parse unavailable"):
        scan_text_magic_numbers(Path("sample.cs"), "if (value > 75) { }\n")
    with pytest.raises(SpiceError, match="tree-sitter parse unavailable"):
        scan_text_magic_numbers(Path("sample.js"), "if (value > 75) { }\n")

    assert parsed_suffixes == [".cs", ".js"]
