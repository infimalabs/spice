import json
from pathlib import Path

from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.javascriptunused import JavaScriptUnusedEntry


def test_general_purpose_study_flags_cover_reference_surface():
    parser = build_parser()
    commands = [
        ["study", "file-loc", "--json", "--staged"],
        ["study", "file-loc", "--json", "--baseline-ref", "HEAD"],
        ["study", "complexity", "--json", "--baseline-ref", "HEAD"],
        ["study", "complexity-hotspots", "--json", "--limit", "3"],
        ["study", "csharp-members", "--json", "--limit", "2"],
        ["study", "csharp-unused-candidates", "--json", "--limit", "2"],
        ["study", "magic-numbers", "--json", "--staged", "--baseline-ref", "HEAD"],
        [
            "study",
            "javascript-unused",
            "--json",
            "--limit",
            "2",
            "--allow-symbol",
            "Keep",
        ],
        ["study", "reachability", "--json", "--limit", "2", "--create-tasks"],
        ["study", "symbol-reachability", "--json", "--limit", "2", "--create-tasks"],
        ["study", "assertion-free-tests", "--json", "--limit", "2", "--create-tasks"],
        ["study", "private-internals", "--json", "--limit", "2", "--create-tasks"],
        ["study", "mutations", "--json", "--staged", "--baseline-ref", "HEAD"],
        ["study", "env-policy", "--json", "--staged"],
        ["study", "env-name-ledger", "--json", "--staged"],
        ["study", "shape", "--json"],
        ["study", "subsumption", "coverage.db", "--json"],
    ]

    parsed = [parser.parse_args(command) for command in commands]

    assert all(args.emit_json for args in parsed)
    assert parsed[1].baseline_ref == "HEAD"
    assert parsed[7].allow_symbols == ["Keep"]
    assert parsed[10].create_tasks is True


def test_file_loc_baseline_ref_uses_changed_paths(tmp_path, monkeypatch, capsys):
    seen: dict[str, object] = {}
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        studies_cli,
        "changed_paths",
        lambda root, baseline_ref: [Path("src/app.py")],
    )

    def scan(paths, **kwargs):
        seen["paths"] = paths
        seen["root"] = kwargs["root"]
        return []

    monkeypatch.setattr(studies_cli.fileloc, "scan_loc_violations", scan)
    args = build_parser().parse_args(
        ["study", "file-loc", "--baseline-ref", "main", "--json"]
    )

    assert args.func(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["artifactKind"] == "spice.study.file-loc"
    assert payload["baselineRef"] == "main"
    assert seen == {"paths": [Path("src/app.py")], "root": tmp_path}


def test_javascript_unused_cli_json_payload(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        studies_cli,
        "tracked_paths",
        lambda root: [Path("entry.js")],
    )
    monkeypatch.setattr(
        studies_cli.javascriptunused,
        "scan_javascript_unused_symbols",
        lambda paths, *, root, allow_symbols: [
            JavaScriptUnusedEntry(
                path="entry.js",
                line=1,
                kind="function",
                name="candidateHelper",
                status="candidate-unused",
                reason="no_references_outside_declaration",
                reference_count=1,
            )
        ],
    )
    args = build_parser().parse_args(["study", "javascript-unused", "--json"])

    assert args.func(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["artifactKind"] == "spice.study.javascript-unused"
    assert payload["findings"][0]["name"] == "candidateHelper"


def test_assertion_free_cli_json_create_tasks(tmp_path, monkeypatch, capsys):
    from spice.tasks import create

    path = tmp_path / "tests" / "test_quality.py"
    path.parent.mkdir()
    path.write_text("def test_without_assertion():\n    value = 1\n", encoding="utf-8")
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    created: list[dict[str, object]] = []

    def fake_add(
        title: str,
        *,
        project: str,
        tags: list[str],
        acceptance: list[str],
    ) -> str:
        created.append(
            {
                "title": title,
                "project": project,
                "tags": tags,
                "acceptance": acceptance,
            }
        )
        return f"QUALITY-{len(created)}"

    monkeypatch.setattr(create, "add", fake_add)
    args = build_parser().parse_args(
        ["study", "assertion-free-tests", "--json", "--create-tasks"]
    )

    assert args.func(args) == 1
    payload = json.loads(capsys.readouterr().out)
    assert payload["artifactKind"] == "spice.study.assertion-free-tests"
    assert payload["createdTasks"] == ["QUALITY-1"]
    assert payload["findings"][0]["test_name"] == "test_without_assertion"
    assert created[0]["project"] == "tests.quality"
