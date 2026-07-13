import json
from pathlib import Path

import pytest

from spice import extensions as extension_loader
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.studies import cli as studies_cli
from spice.studies.javascriptunused import JavaScriptUnusedEntry
from spice.studies.links import MarkdownLinkCaseFinding
from tests.test_extensionhelpers import (
    FilteredExtensionDistribution,
    build_fixture_distribution,
)


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
        ["study", "taste", "--json", "--staged"],
        ["study", "shape", "--json"],
        ["study", "markdown-links", "--json"],
        ["study", "subsumption", "coverage.db", "--json"],
    ]

    parsed = [parser.parse_args(command) for command in commands]

    assert all(args.emit_json for args in parsed)
    assert parsed[1].baseline_ref == "HEAD"
    assert parsed[7].allow_symbols == ["Keep"]
    assert parsed[10].create_tasks is True


def test_taste_cli_renders_exact_inclusive_inflection_suggestions(
    tmp_path, monkeypatch, capsys
):
    doc = tmp_path / "notes.md"
    doc.write_text(
        "Review the WHITELISTS.\nRemove BLACKLISTED records.\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "taste", "notes.md"])

    assert args.func(args) == 1
    assert capsys.readouterr().out == "\n".join(
        [
            "taste: 2 low-value or poor-taste word(s); rephrase for better taste",
            "  FAIL  notes.md:1  'whitelists' -> consider 'allowlists'",
            "  FAIL  notes.md:2  'blacklisted' -> consider 'blocklisted'",
            "",
        ]
    )


def test_study_extension_command_from_fixture_wheel_runs_json_success(
    tmp_path, monkeypatch, capsys
):
    wheel, distribution = build_fixture_distribution(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))
    monkeypatch.setattr(
        extension_loader.metadata,
        "distributions",
        lambda: [
            FilteredExtensionDistribution(
                distribution,
                {extension_loader.SPICE_STUDY_ENTRY_POINT_GROUP: {"toy-study"}},
            )
        ],
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    parser = build_parser()

    built_in_args = parser.parse_args(["study", "shape", "--json"])
    extension_args = parser.parse_args(["study", "toy-study", "src/app.py", "--json"])

    assert built_in_args.study_action == "shape"
    assert extension_args.study_action == "toy-study"
    assert extension_args.func(extension_args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload == {
        "artifactKind": "spice.study.toy-study",
        "result": {"paths": ["src/app.py"], "study": "toy"},
    }


def test_study_extension_shadow_fails_with_shared_loader_error(tmp_path, monkeypatch):
    wheel, distribution = build_fixture_distribution(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))
    monkeypatch.setattr(
        extension_loader.metadata,
        "distributions",
        lambda: [
            FilteredExtensionDistribution(
                distribution,
                {
                    extension_loader.SPICE_STUDY_ENTRY_POINT_GROUP: {
                        "file-loc",
                        "toy-study",
                    }
                },
            )
        ],
    )

    with pytest.raises(SpiceError) as exc_info:
        build_parser()

    message = str(exc_info.value)
    assert "extension entry point group 'spice.studies'" in message
    assert "entry 'file-loc'" in message
    assert "shadows built-in" in message


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


def test_markdown_links_cli_renders_clean_board(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(studies_cli, "tracked_paths", lambda root: [Path("docs/a.md")])
    monkeypatch.setattr(
        studies_cli.links,
        "markdown_link_case_findings",
        lambda root, *, paths: [],
    )
    args = build_parser().parse_args(["study", "markdown-links"])

    assert args.func(args) == 0
    assert capsys.readouterr().out == "markdown-links: ok\n"


def test_markdown_links_cli_renders_finding_board(tmp_path, monkeypatch, capsys):
    finding = MarkdownLinkCaseFinding(
        source_path=Path("docs/index.md"),
        line=3,
        raw_target="GUIDE.md",
        resolved_path=Path("docs/GUIDE.md"),
        expected_path=Path("docs/guide.md"),
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(studies_cli, "tracked_paths", lambda root: [Path("docs/a.md")])
    monkeypatch.setattr(
        studies_cli.links,
        "markdown_link_case_findings",
        lambda root, *, paths: [finding],
    )
    args = build_parser().parse_args(["study", "markdown-links"])

    assert args.func(args) == 1
    assert capsys.readouterr().out == "\n".join(
        [
            "markdown-links: 1 case-mismatched tracked markdown link target(s)",
            "  FAIL  docs/index.md:3 GUIDE.md -> docs/guide.md",
            "",
        ]
    )


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
