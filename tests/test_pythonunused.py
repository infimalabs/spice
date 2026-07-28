from pathlib import Path

from spice.cli.parser import build_parser
from spice.hooks import precommit
from spice.studies import cli as studies_cli
from spice.studies.pythonunused import (
    REASON_CONFIGURED_ENTRY_POINT,
    REASON_NO_REFERENCES,
    REASON_PRODUCTION_REFERENCE,
    REASON_TEST_ONLY,
    STATUS_CANDIDATE_UNUSED,
    STATUS_RETAINED,
    STATUS_TEST_ONLY,
    STATUS_USED,
    PythonUnusedExemption,
    collect_python_unused_entries,
    scan_python_unused_symbols,
)


def test_python_unused_classifies_top_level_symbols_and_runtime_bindings(tmp_path):
    _write_python_unused_repo(tmp_path)
    exemption = PythonUnusedExemption(
        "spice.live.dynamic_handler",
        "getattr(live, configured_handler_name) dispatch",
    )

    entries = collect_python_unused_entries(tmp_path, exemptions=[exemption])
    selected = {
        entry.symbol: (entry.status, entry.reason, entry.line, entry.test_references)
        for entry in entries
        if entry.symbol
        in {
            "candidate_handler",
            "test_only_handler",
            "registered_handler",
            "dynamic_handler",
            "entry_main",
            "HandlerImplementation",
            "command_handler",
        }
    }

    assert selected == {
        "candidate_handler": (
            STATUS_CANDIDATE_UNUSED,
            REASON_NO_REFERENCES,
            18,
            [],
        ),
        "test_only_handler": (
            STATUS_TEST_ONLY,
            REASON_TEST_ONLY,
            21,
            ["test_live.py"],
        ),
        "registered_handler": (
            STATUS_USED,
            REASON_PRODUCTION_REFERENCE,
            10,
            [],
        ),
        "dynamic_handler": (
            STATUS_RETAINED,
            "named dynamic-dispatch exemption: "
            "getattr(live, configured_handler_name) dispatch",
            15,
            [],
        ),
        "entry_main": (STATUS_USED, REASON_CONFIGURED_ENTRY_POINT, 1, []),
        "HandlerImplementation": (
            STATUS_USED,
            REASON_PRODUCTION_REFERENCE,
            6,
            [],
        ),
        "command_handler": (
            STATUS_USED,
            REASON_PRODUCTION_REFERENCE,
            24,
            [],
        ),
    }

    findings = scan_python_unused_symbols(tmp_path, exemptions=[exemption])
    assert [
        (
            finding.path,
            finding.line,
            finding.kind,
            finding.symbol,
            finding.status,
            finding.reason,
        )
        for finding in findings
    ] == [
        (
            "spice/live.py",
            18,
            "function",
            "candidate_handler",
            STATUS_CANDIDATE_UNUSED,
            REASON_NO_REFERENCES,
        ),
        (
            "spice/live.py",
            21,
            "function",
            "test_only_handler",
            STATUS_TEST_ONLY,
            REASON_TEST_ONLY,
        ),
    ]


def test_python_unused_configured_package_module_uses_main_execution_root(tmp_path):
    _write_python_unused_repo(tmp_path)
    repository_config = tmp_path / "spice.toml"
    repository_config.write_text(
        repository_config.read_text(encoding="utf-8")
        + "package = ['python', '-m', 'spice.commandpkg']\n",
        encoding="utf-8",
    )
    package = tmp_path / "spice" / "commandpkg"
    package.mkdir()
    (package / "__init__.py").write_text(
        "PACKAGE_NAME = 'commandpkg'\n", encoding="utf-8"
    )
    (package / "__main__.py").write_text(
        "from spice.live import package_command_handler\n\npackage_command_handler()\n",
        encoding="utf-8",
    )
    live = tmp_path / "spice" / "live.py"
    live.write_text(
        live.read_text(encoding="utf-8") + "\ndef package_command_handler():\n"
        "    return 'package-command'\n",
        encoding="utf-8",
    )

    entries = collect_python_unused_entries(tmp_path)
    package_handler = next(
        entry for entry in entries if entry.symbol == "package_command_handler"
    )

    assert (
        package_handler.status,
        package_handler.reason,
        package_handler.path,
    ) == (
        STATUS_USED,
        REASON_PRODUCTION_REFERENCE,
        "spice/live.py",
    )


def test_study_python_unused_cli_reports_both_verdicts(tmp_path, monkeypatch, capsys):
    _write_python_unused_repo(tmp_path)
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "python-unused"])

    assert args.func(args) == 1
    output = capsys.readouterr().out
    assert "python-unused: 2 candidate-unused and 1 test-only" in output
    assert (
        "spice/live.py:18 function candidate_handler status=candidate-unused "
        "reason=no production or test references"
    ) in output
    assert (
        "spice/live.py:21 function test_only_handler status=test-only "
        "reason=references only in tests"
    ) in output
    assert "referenced by tests: test_live.py" in output


def test_python_unused_quality_gate_invokes_study_scan(tmp_path):
    _write_python_unused_repo(tmp_path)

    message = precommit.quality_gate_failure(tmp_path, "python-unused")

    assert message is not None
    assert "python-unused: 2 candidate-unused and 1 test-only" in message
    assert "spice/live.py:18 function candidate_handler" in message


def _write_python_unused_repo(root: Path) -> None:
    (root / "spice" / "cli").mkdir(parents=True)
    (root / "tests").mkdir()
    (root / "pyproject.toml").write_text(
        "[project]\n"
        "name = 'python-unused-fixture'\n"
        "version = '1.0.0'\n"
        "[project.scripts]\n"
        "fixture = 'spice.entrypoint:entry_main'\n",
        encoding="utf-8",
    )
    (root / "spice.toml").write_text(
        "[commands]\nmounted = ['python', '-m', 'spice.commandmod']\n",
        encoding="utf-8",
    )
    (root / "spice" / "cli" / "entry.py").write_text(
        "from spice.live import HANDLERS, HandlerImplementation\n"
        "import spice.live as live\n\n"
        "def run(configured_handler_name):\n"
        "    HANDLERS['registered']()\n"
        "    getattr(live, configured_handler_name)()\n"
        "    return HandlerImplementation().handle()\n\n"
        "run('dynamic_handler')\n",
        encoding="utf-8",
    )
    (root / "spice" / "entrypoint.py").write_text(
        "def entry_main():\n    return 0\n", encoding="utf-8"
    )
    (root / "spice" / "commandmod.py").write_text(
        "from spice.live import command_handler\n\ncommand_handler()\n",
        encoding="utf-8",
    )
    (root / "spice" / "live.py").write_text(
        "from typing import Protocol\n\n"
        "class HandlerProtocol(Protocol):\n"
        "    def handle(self): ...\n\n"
        "class HandlerImplementation(HandlerProtocol):\n"
        "    def handle(self):\n"
        "        return 'handled'\n\n"
        "def registered_handler():\n"
        "    return 'registered'\n\n"
        "HANDLERS = {'registered': registered_handler}\n\n"
        "def dynamic_handler():\n"
        "    return 'dynamic'\n\n"
        "def candidate_handler():\n"
        "    return 'candidate'\n\n"
        "def test_only_handler():\n"
        "    return 'test-only'\n\n"
        "def command_handler():\n"
        "    return 'command'\n",
        encoding="utf-8",
    )
    (root / "tests" / "test_live.py").write_text(
        "from spice.live import test_only_handler\n\n"
        "def test_test_only_handler():\n"
        "    assert test_only_handler() == 'test-only'\n",
        encoding="utf-8",
    )
