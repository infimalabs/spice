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

SUBSUMPTION_RENDER_LIMIT = 10


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
        [
            "study",
            "subsumption",
            "--record",
            "--package",
            "spice",
            "--retain-coverage",
            "coverage.db",
            "--pytest-arg=-q",
            "--limit",
            str(SUBSUMPTION_RENDER_LIMIT),
            "--json",
        ],
    ]

    parsed = [parser.parse_args(command) for command in commands]

    assert all(args.emit_json for args in parsed)
    assert parsed[1].baseline_ref == "HEAD"
    assert parsed[7].allow_symbols == ["Keep"]
    assert parsed[10].create_tasks is True
    assert parsed[-1].record is True
    assert parsed[-1].retain_coverage == Path("coverage.db")
    assert parsed[-1].pytest_arg == ["-q"]
    assert parsed[-1].limit == SUBSUMPTION_RENDER_LIMIT


def test_task_generating_studies_share_deferred_origin_flags():
    parser = build_parser()
    actions = tuple(studies_cli.TASK_GENERATING_STUDY_ACTIONS)
    commands = [
        [
            "study",
            action,
            "--create-tasks",
            "--deferred",
            "--origin",
            "ack:1jN54zJJ",
        ]
        for action in actions
    ]

    parsed = [parser.parse_args(command) for command in commands]
    controls = [studies_cli._study_task_creation_controls(args) for args in parsed]

    assert tuple(args.study_action for args in parsed) == actions
    assert all(args.create_tasks for args in parsed)
    assert controls == [
        studies_cli.StudyTaskCreationControls(
            deferred=True,
            origin="ack:1jN54zJJ",
            print_created=True,
        )
    ] * len(actions)


def test_task_generator_registry_drives_parser_and_dispatch_controls(
    tmp_path, monkeypatch
):
    generated = {}
    created = {}

    def recording_generator(action):
        def generator(args, root):
            generated[action] = (args.study_action, root)
            spec = studies_cli.StudyTaskSpec(
                study=action,
                finding_identity=("registry-contract",),
                title=f"Registry contract: {action}",
                project="tests.quality",
                tags=("registry-contract",),
                acceptance=("central dispatch creates this task",),
            )
            return studies_cli.TaskGeneratingStudyResult(
                task_specs=(spec,),
                has_findings=True,
                text_output=f"{action}: registry contract",
                json_fields={"findings": [action]},
            )

        return generator

    def configure_probe(actions):
        studies_cli._add_study_action(
            actions, "registry-probe", "Synthetic registry dispatch probe."
        )

    registry = {
        action: studies_cli.TaskGeneratingStudyAction(
            configure_parser=entry.configure_parser,
            create_tasks_help=entry.create_tasks_help,
            generator=recording_generator(action),
        )
        for action, entry in studies_cli.TASK_GENERATING_STUDY_ACTIONS.items()
    }
    registry["registry-probe"] = studies_cli.TaskGeneratingStudyAction(
        configure_parser=configure_probe,
        create_tasks_help="Create the synthetic registry probe task.",
        generator=recording_generator("registry-probe"),
    )

    def create_tasks(specs, *, controls):
        action = specs[0].study
        created[action] = (tuple(specs), controls)
        return [f"TASK-{action}"]

    monkeypatch.setattr(studies_cli, "TASK_GENERATING_STUDY_ACTIONS", registry)
    monkeypatch.setattr(studies_cli, "create_study_tasks", create_tasks)
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    parser = build_parser()

    for action in registry:
        args = parser.parse_args(
            [
                "study",
                action,
                "--create-tasks",
                "--deferred",
                "--origin",
                "ack:1jN54zJJ",
                "--json",
            ]
        )
        assert args.func(args) == 1

    expected_controls = studies_cli.StudyTaskCreationControls(
        deferred=True,
        origin="ack:1jN54zJJ",
        print_created=False,
    )
    assert generated == {action: (action, tmp_path) for action in registry}
    assert {
        action: (specs[0].study, controls)
        for action, (specs, controls) in created.items()
    } == {action: (action, expected_controls) for action in registry}


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
        lambda paths, *, root, allow_symbols, declaration_exemptions: [
            JavaScriptUnusedEntry(
                path="entry.js",
                line=1,
                kind="function",
                name="candidateHelper",
                status="candidate-unused",
                reason="no_references_outside_declaration",
                reference_count=1,
                test_reference_count=0,
            ),
            JavaScriptUnusedEntry(
                path="entry.js",
                line=5,
                kind="function",
                name="testedHelper",
                status="test-only",
                reason="references_only_in_tests",
                reference_count=1,
                test_reference_count=2,
            ),
        ],
    )
    args = build_parser().parse_args(["study", "javascript-unused", "--json"])

    assert args.func(args) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["artifactKind"] == "spice.study.javascript-unused"
    assert payload["declarationExemptions"] == [
        {
            "path": "spice/serve/static/app.mosaic-event-log.js",
            "reason": (
                "shared browser replay oracle that exercises the production "
                "event-log branch implementation without duplicating that "
                "algorithm into tests"
            ),
            "symbol": "mosaicReplayEventLog",
        }
    ]
    assert payload["findings"][0]["name"] == "candidateHelper"
    assert payload["findings"][0]["status"] == "candidate-unused"
    assert payload["findings"][1]["name"] == "testedHelper"
    assert payload["findings"][1]["status"] == "test-only"
    assert payload["findings"][1]["test_reference_count"] == 2


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
    path = tmp_path / "tests" / "test_quality.py"
    path.parent.mkdir()
    path.write_text("def test_without_assertion():\n    value = 1\n", encoding="utf-8")
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    created = []
    options: dict[str, object] = {}

    def fake_create(specs, **kwargs):
        created.extend(specs)
        options.update(kwargs)
        return ["QUALITY-1"]

    monkeypatch.setattr(studies_cli, "create_study_tasks", fake_create)
    args = build_parser().parse_args(
        [
            "study",
            "assertion-free-tests",
            "--json",
            "--create-tasks",
            "--deferred",
            "--origin",
            "ack:1jN54zJJ",
        ]
    )

    assert args.func(args) == 1
    payload = json.loads(capsys.readouterr().out)
    assert payload["artifactKind"] == "spice.study.assertion-free-tests"
    assert payload["createdTasks"] == ["QUALITY-1"]
    assert payload["findings"][0]["test_name"] == "test_without_assertion"
    assert created[0].project == "tests.quality"
    assert options == {
        "controls": studies_cli.StudyTaskCreationControls(
            deferred=True,
            origin="ack:1jN54zJJ",
            print_created=False,
        )
    }
