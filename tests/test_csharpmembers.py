from __future__ import annotations

import json
from pathlib import Path

from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.csharpmembers import collect_csharp_class_records


def test_collect_csharp_class_records_maps_members(tmp_path: Path) -> None:
    path = tmp_path / "Sample.cs"
    path.write_text(
        """using System;

namespace Demo
{
    public partial class SampleSystem
    {
        int counter;

        struct Snapshot
        {
            public int Count;
        }

        protected override void OnCreate()
        {
            counter = 0;
        }

        static bool TryComputeValue(int value)
        {
            return value > 0;
        }
    }
}
""",
        encoding="utf-8",
    )

    records = collect_csharp_class_records(
        [Path("Sample.cs")], root=tmp_path, class_name="SampleSystem"
    )

    assert len(records) == 1
    record = records[0]
    assert record.path == "Sample.cs"
    assert record.name == "SampleSystem"
    assert record.member_count == 4
    assert [member.kind for member in record.members] == [
        "field_declaration",
        "struct_declaration",
        "method_declaration",
        "method_declaration",
    ]
    assert record.members[0].name == "counter"
    assert record.members[1].name == "Snapshot"
    assert record.members[2].name == "OnCreate"
    assert record.members[3].name == "TryComputeValue"


def test_study_csharp_members_cli_json(tmp_path: Path, monkeypatch, capsys) -> None:
    path = tmp_path / "Sample.cs"
    path.write_text(
        """namespace Demo
{
    public partial class SampleSystem
    {
        int counter;
        void Tick() {}
    }
}
""",
        encoding="utf-8",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(
        [
            "study",
            "csharp-members",
            "Sample.cs",
            "--class-name",
            "SampleSystem",
            "--json",
        ]
    )

    assert args.func(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["classCount"] == 1
    assert payload["classes"][0]["name"] == "SampleSystem"
    assert payload["classes"][0]["member_count"] == 2


def test_study_csharp_members_cli_text_limit(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    path = tmp_path / "Sample.cs"
    path.write_text(
        """class Sample
{
    int counter;
    void Small() {}
    void Larger()
    {
        counter++;
    }
}
""",
        encoding="utf-8",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(
        ["study", "csharp-members", "Sample.cs", "--limit", "1"]
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "csharp-members: 1 class(es)" in output
    assert "longest_top_1=" in output
    assert "Larger" in output
