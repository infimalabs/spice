from __future__ import annotations

import json
from pathlib import Path

from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.csharpunused import (
    CSharpUnusedEntry,
    STATUS_CANDIDATE_UNUSED,
    STATUS_RETAINED,
    STATUS_USED,
    collect_csharp_unused_entries,
    render_csharp_unused_board,
)


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _entries_by_key(entries):
    return {(entry.kind, entry.name): entry for entry in entries}


def test_collect_csharp_unused_candidates_classifies_generic_csharp_surfaces(
    tmp_path: Path,
) -> None:
    _write(
        tmp_path / "src" / "Sample.cs",
        """
using System.Text;
using AliasTool = Demo.Tools.Helper;
using CandidateAlias = Demo.Tools.Unused;

namespace Demo
{
    class Sample
    {
        private int idleCounter;
        private int activeCounter;
        [SomeAttribute] private int reflectedCounter;

        public void Run()
        {
            UsedHelper();
        }

        private void UsedHelper()
        {
            activeCounter++;
            AliasTool.Touch();
        }

        private void CandidateHelper() {}
    }

    partial class PartialSample
    {
        private int partialCounter;
        private void PartialHook() {}
    }
}
""",
    )

    entries = collect_csharp_unused_entries([Path("src/Sample.cs")], root=tmp_path)
    by_key = _entries_by_key(entries)

    assert by_key[("private_field", "idleCounter")].status == STATUS_CANDIDATE_UNUSED
    assert by_key[("private_method", "CandidateHelper")].status == (
        STATUS_CANDIDATE_UNUSED
    )
    assert by_key[("using_directive", "CandidateAlias")].status == (
        STATUS_CANDIDATE_UNUSED
    )
    assert by_key[("private_field", "activeCounter")].status == STATUS_USED
    assert by_key[("private_method", "UsedHelper")].status == STATUS_USED
    assert by_key[("using_directive", "AliasTool")].status == STATUS_USED
    assert by_key[("private_field", "reflectedCounter")].status == STATUS_RETAINED
    assert by_key[("private_field", "reflectedCounter")].reason == (
        "attribute_retained"
    )
    assert by_key[("private_field", "partialCounter")].reason == ("partial_declaration")
    assert by_key[("method", "PartialHook")].reason == "partial_declaration"
    assert by_key[("using_directive", "System.Text")].reason == (
        "namespace_import_requires_semantic_resolution"
    )


def test_custom_return_type_method_reports_method_identifier(tmp_path: Path) -> None:
    _write(
        tmp_path / "src" / "Sample.cs",
        """
namespace Demo
{
    class Widget {}

    class Sample
    {
        private Widget CandidateHelper()
        {
            return null;
        }
    }
}
""",
    )

    entries = collect_csharp_unused_entries([Path("src/Sample.cs")], root=tmp_path)
    by_key = _entries_by_key(entries)

    assert by_key[("private_method", "CandidateHelper")].status == (
        STATUS_CANDIDATE_UNUSED
    )


def test_interface_members_are_not_private_unused_candidates(tmp_path: Path) -> None:
    _write(
        tmp_path / "src" / "Sample.cs",
        """
namespace Demo
{
    interface ISample
    {
        private void HiddenHelper() {}
        void Run();
    }

    class Sample
    {
        private void CandidateHelper() {}
    }
}
""",
    )

    entries = collect_csharp_unused_entries([Path("src/Sample.cs")], root=tmp_path)
    private_method_names = sorted(
        entry.name for entry in entries if entry.kind == "private_method"
    )

    assert private_method_names == ["CandidateHelper", "HiddenHelper"]


def test_study_csharp_unused_candidates_cli_reports_candidates(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    _write(
        tmp_path / "Sample.cs",
        """
class Sample
{
    private void CandidateHelper() {}
}
""",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "csharp-unused-candidates", "Sample.cs"])

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "csharp-unused-candidates: candidateUnused=1" in output
    assert "Sample.cs:4 private_method CandidateHelper" in output


def test_csharp_unused_board_limit_applies_to_candidate_rows() -> None:
    entries = [
        _entry("active", STATUS_USED, line=1),
        _entry("candidateOne", STATUS_CANDIDATE_UNUSED, line=2),
        _entry("candidateTwo", STATUS_CANDIDATE_UNUSED, line=3),
        _entry("retained", STATUS_RETAINED, line=4),
    ]

    output = render_csharp_unused_board(entries, limit=1)

    assert output.splitlines() == [
        "csharp-unused-candidates: candidateUnused=2 used=1 retained=1 showing=1",
        "Candidate Entries",
        "  Sample.cs:2 private_method candidateOne refs=1 reason=test",
        "Used Entries",
        "  Sample.cs:1 private_method active refs=1 reason=test",
        "Retained Entries",
        "  Sample.cs:4 private_method retained refs=1 reason=test",
    ]


def test_study_csharp_unused_candidates_cli_json(tmp_path: Path, monkeypatch, capsys):
    _write(
        tmp_path / "Sample.cs",
        """
class Sample
{
    private void CandidateHelper() {}
}
""",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(
        ["study", "csharp-unused-candidates", "Sample.cs", "--json"]
    )

    assert args.func(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["artifactKind"] == "spice.study.csharp-unused-candidates"
    assert payload["stats"]["candidateUnused"] == 1
    assert payload["entries"][0]["name"] == "CandidateHelper"


def _entry(name: str, status: str, *, line: int) -> CSharpUnusedEntry:
    return CSharpUnusedEntry(
        path="Sample.cs",
        line=line,
        kind="private_method",
        name=name,
        status=status,
        reason="test",
        reference_count=1,
    )
