import subprocess
import sys
from pathlib import Path
from textwrap import dedent

from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.javascriptunused import (
    STATUS_CANDIDATE_UNUSED,
    STATUS_RETAINED,
    STATUS_TEST_ONLY,
    STATUS_USED,
    collect_javascript_unused_entries,
    scan_javascript_unused_symbols,
)


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _entries_by_name(entries):
    return {entry.name: entry for entry in entries}


def test_javascript_unused_module_keeps_tree_sitter_packages_lazy() -> None:
    script = """
        import sys

        import spice.studies.javascriptunused

        loaded = sorted(
            name
            for name in sys.modules
            if name == "spice.studies.treesitter" or name.startswith("tree_sitter")
        )
        print("\\n".join(loaded), end="")
    """
    result = subprocess.run(
        [sys.executable, "-c", dedent(script)],
        cwd=Path(__file__).resolve().parents[1],
        check=True,
        capture_output=True,
        text=True,
    )

    assert result.stdout == ""


def test_top_level_parser_keeps_tree_sitter_seam_lazy() -> None:
    script = """
        import sys

        from spice.cli.parser import build_parser

        build_parser()
        state = "loaded" if "spice.studies.treesitter" in sys.modules else "lazy"
        print(state)
    """
    result = subprocess.run(
        [sys.executable, "-c", dedent(script)],
        cwd=Path(__file__).resolve().parents[1],
        check=True,
        capture_output=True,
        text=True,
    )

    assert result.stdout == "lazy\n"


def test_collect_javascript_unused_symbols_counts_used_and_retained_exports(
    tmp_path: Path,
) -> None:
    _write(
        tmp_path / "app.helpers.js",
        """
function usedHelper() {
  return 1;
}

function candidateHelper() {
  return 2;
}

const retainedExport = {
  boot() {
    return "ok";
  },
};
""",
    )
    _write(
        tmp_path / "app.js",
        """
usedHelper();
""",
    )

    entries = collect_javascript_unused_entries(
        [Path("app.helpers.js"), Path("app.js")],
        root=tmp_path,
        allow_symbols=["retainedExport"],
    )
    by_name = _entries_by_name(entries)

    assert by_name["usedHelper"].status == STATUS_USED
    assert by_name["usedHelper"].reason == ("identifier_referenced_outside_declaration")
    assert by_name["candidateHelper"].status == STATUS_CANDIDATE_UNUSED
    assert by_name["candidateHelper"].reason == "no_references_outside_declaration"
    assert by_name["retainedExport"].status == STATUS_RETAINED
    assert by_name["retainedExport"].reason == "intentional_global_allowlist"


def test_collect_javascript_unused_symbols_classifies_test_only_references(
    tmp_path: Path,
) -> None:
    _write(
        tmp_path / "app.helpers.js",
        """
function testedHelper() {
  return 1;
}

function candidateHelper() {
  return 2;
}
""",
    )
    _write(
        tmp_path / "tests" / "fixtures" / "harness.js",
        """
context.testedHelper(1, 2);
""",
    )

    entries = collect_javascript_unused_entries(
        [Path("app.helpers.js"), Path("tests/fixtures/harness.js")],
        root=tmp_path,
    )
    by_name = _entries_by_name(entries)

    tested = by_name["testedHelper"]
    candidate = by_name["candidateHelper"]
    assert tested.status == STATUS_TEST_ONLY
    assert tested.reason == "references_only_in_tests"
    assert tested.reference_count == 1
    assert tested.test_reference_count == 1
    assert candidate.status == STATUS_CANDIDATE_UNUSED
    assert candidate.reason == "no_references_outside_declaration"
    assert candidate.reference_count == 1
    assert candidate.test_reference_count == 0
    assert tested.status != candidate.status


def test_javascript_unused_exemptions_match_exact_path_and_symbol(
    tmp_path: Path,
) -> None:
    _write(
        tmp_path / "first.js",
        """
function testedHelper() {
  return 1;
}
""",
    )
    _write(
        tmp_path / "second.js",
        """
function otherTestedHelper() {
  return 2;
}
""",
    )
    _write(
        tmp_path / "tests" / "fixtures" / "harness.js",
        """
context.testedHelper();
context.otherTestedHelper();
""",
    )

    findings = scan_javascript_unused_symbols(
        [
            Path("first.js"),
            Path("second.js"),
            Path("tests/fixtures/harness.js"),
        ],
        root=tmp_path,
        declaration_exemptions={
            ("first.js", "testedHelper"): "fixture exercises this declaration",
        },
    )

    assert [(finding.path, finding.name) for finding in findings] == [
        ("second.js", "otherTestedHelper")
    ]


def test_collect_javascript_unused_symbols_keeps_test_declared_helpers_used(
    tmp_path: Path,
) -> None:
    _write(
        tmp_path / "tests" / "fixtures" / "harness.js",
        """
function fixtureHelper() {
  return 1;
}

function fixtureCandidate() {
  return 2;
}

fixtureHelper();
""",
    )

    entries = collect_javascript_unused_entries(
        [Path("tests/fixtures/harness.js")],
        root=tmp_path,
    )
    by_name = _entries_by_name(entries)

    assert by_name["fixtureHelper"].status == STATUS_USED
    assert by_name["fixtureCandidate"].status == STATUS_CANDIDATE_UNUSED
    assert by_name["fixtureCandidate"].reason == "no_references_outside_declaration"


def test_javascript_unused_study_cli_reports_candidates(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    _write(
        tmp_path / "entry.js",
        """
function usedHelper() {
  return 1;
}

function candidateHelper() {
  return 2;
}

function testedHelper() {
  return 3;
}

const retainedExport = {};
""",
    )
    _write(
        tmp_path / "consumer.js",
        """
usedHelper();
""",
    )
    _write(
        tmp_path / "tests" / "fixtures" / "harness.js",
        """
context.testedHelper();
""",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(
        [
            "study",
            "javascript-unused",
            "--allow-symbol",
            "retainedExport",
            "entry.js",
            "consumer.js",
            "tests/fixtures/harness.js",
        ]
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert (
        "javascript-unused: 1 candidate-unused and 1 test-only "
        "top-level symbol(s) found"
    ) in output
    assert (
        "entry.js:6 function candidateHelper status=candidate-unused "
        "refs=1 test_refs=0 reason=no_references_outside_declaration"
    ) in output
    assert (
        "entry.js:10 function testedHelper status=test-only "
        "refs=1 test_refs=1 reason=references_only_in_tests"
    ) in output
