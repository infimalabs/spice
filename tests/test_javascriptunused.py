from pathlib import Path

from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.javascriptunused import (
    STATUS_CANDIDATE_UNUSED,
    STATUS_RETAINED,
    STATUS_USED,
    collect_javascript_unused_entries,
)


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _entries_by_name(entries):
    return {entry.name: entry for entry in entries}


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

const retainedExport = {};
""",
    )
    _write(
        tmp_path / "consumer.js",
        """
usedHelper();
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
        ]
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "javascript-unused: 1 candidate-unused top-level symbol(s) found" in output
    assert "entry.js:6 function candidateHelper" in output
