"""Constitution mechanics: flex ratio, sticky state, magic-number verdicts."""

import json
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.flexstate import (
    sticky_function_keys_after_renames,
    sticky_paths_after_renames,
)
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    FILE_LOC_LIMIT,
    FLEX_DENOMINATOR,
    FLEX_NUMERATOR,
    flex_limit,
)
from spice.sqliteconnection import sqlite_connection
from spice.studies import cli as studies_cli
from spice.studies import fileloc, gates, mutations, subsumption, testquality
from spice.studies.fileloc import scan_loc_violations, scan_staged_loc_violations
from spice.studies.subsumption import record_subsumption, scan_subsumption
from spice.studies.magicnums import scan_text_magic_numbers
from spice.studies.testquality import (
    render_assertion_free_board,
    render_private_internal_board,
    scan_assertion_free_tests,
    scan_private_internal_coupling,
)

MAGIC_HIGH_THRESHOLD = 100
MAGIC_HIGH_LITERAL = "125"


def test_flex_ratio_is_three_halves():
    for limit in (FILE_LOC_LIMIT, COMPLEXITY_MAX_LENGTH, COMPLEXITY_MAX_CCN):
        assert flex_limit(limit) == limit * FLEX_NUMERATOR // FLEX_DENOMINATOR


def test_sticky_paths_follow_renames():
    sticky = sticky_paths_after_renames(
        {Path("old.py")}, {Path("old.py"): Path("new.py")}
    )
    assert sticky == {Path("old.py"), Path("new.py")}


def test_binary_assets_are_not_file_shape_candidates(tmp_path):
    rel_path = Path("screenshot.png")
    (tmp_path / rel_path).write_bytes(b"\x89PNG\r\n\x1a\n\0" + b"\n" * 2000)

    findings = scan_loc_violations(
        [rel_path],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
    )

    assert findings == []


def test_generated_lockfiles_do_not_trip_file_shape_pressure(tmp_path):
    lock_path = Path("uv.lock")
    generic_lock_path = Path("tool.lock")
    nested_lock_path = Path("client") / "package-lock.json"
    source_path = Path("large_source.py")
    (tmp_path / "client").mkdir()
    (tmp_path / lock_path).write_text("package = []\n" * 20, encoding="utf-8")
    (tmp_path / generic_lock_path).write_text("state = []\n" * 20, encoding="utf-8")
    (tmp_path / nested_lock_path).write_text(
        '{"lockfileVersion": 3}\n' * 20,
        encoding="utf-8",
    )
    (tmp_path / source_path).write_text("print('large')\n" * 20, encoding="utf-8")

    findings = scan_loc_violations(
        [lock_path, generic_lock_path, nested_lock_path, source_path],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
    )

    assert [finding.path for finding in findings] == [source_path.as_posix()]


def test_asset_heavy_repo_scans_source_candidates_by_default(tmp_path):
    asset_path = Path("textures") / "splash.png"
    data_path = Path("meshes") / "scene.fbx"
    generated_path = Path("dist") / "compiled.py"
    root_generated_path = Path("schema_pb2.py")
    minified_path = Path("bundle.min.css")
    repo_doc_path = Path("docs") / "guide.md"
    source_path = Path("src") / "large_source.py"
    for path in (
        asset_path,
        data_path,
        generated_path,
        root_generated_path,
        minified_path,
        repo_doc_path,
        source_path,
    ):
        (tmp_path / path).parent.mkdir(parents=True, exist_ok=True)
    (tmp_path / asset_path).write_bytes(b"\x89PNG\r\n\x1a\n\0" + b"x" * 200)
    (tmp_path / data_path).write_text("vertex data\n" * 20, encoding="utf-8")
    (tmp_path / generated_path).write_text("print('built')\n" * 20, encoding="utf-8")
    (tmp_path / root_generated_path).write_text(
        "DESCRIPTOR = object()\n" * 20,
        encoding="utf-8",
    )
    (tmp_path / minified_path).write_text(".x{color:red}" * 20, encoding="utf-8")
    (tmp_path / repo_doc_path).write_text("doctrine\n" * 20, encoding="utf-8")
    (tmp_path / source_path).write_text("print('large')\n" * 20, encoding="utf-8")

    findings = scan_loc_violations(
        [
            asset_path,
            data_path,
            generated_path,
            root_generated_path,
            minified_path,
            repo_doc_path,
            source_path,
        ],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
        repo_doc_paths={repo_doc_path},
    )

    assert [finding.path for finding in findings] == [source_path.as_posix()]


def test_study_explicit_directory_reports_file_path_requirement(tmp_path, monkeypatch):
    directory = tmp_path / "spice" / "serve"
    directory.mkdir(parents=True)
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "file-loc", "spice/serve"])

    with pytest.raises(SpiceError, match="file paths.*spice/serve"):
        args.func(args)


def test_assertion_free_scanner_counts_tests_without_assertions(tmp_path):
    path = tmp_path / "tests" / "test_quality.py"
    path.parent.mkdir()
    path.write_text(
        "\n".join(
            [
                "import pytest",
                "",
                "def test_without_assertion():",
                "    value = 1",
                "",
                "def test_did_not_throw_only():",
                "    int('1')",
                "",
                "def test_with_assert():",
                "    value = 1",
                "    assert value == 1",
                "",
                "def test_with_pytest_raises():",
                "    with pytest.raises(ValueError):",
                "        raise ValueError('x')",
                "",
                "def test_with_pytest_raises_call():",
                "    pytest.raises(ValueError, int, 'x')",
                "",
                "def test_with_unittest_assertion(case, result):",
                "    case.assertEqual(result, 'ok')",
                "",
                "def test_with_mock_assertion(mock):",
                "    mock.assert_called_once_with('x')",
                "",
                "def test_assert_true_equivalent():",
                "    assert True",
                "",
                "def test_unittest_assert_true_equivalent(case):",
                "    case.assertTrue(True)",
                "",
                "def test_unittest_assert_true_compare_equivalent(case):",
                "    case.assertTrue(1 == 1)",
                "",
                "def helper_without_assertion():",
                "    value = 2",
                "",
                "def test_nested_assertion_does_not_count():",
                "    def helper():",
                "        assert True",
                "    helper()",
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    findings = scan_assertion_free_tests([Path("tests/test_quality.py")], root=tmp_path)

    assert [(f.path, f.line, f.test_name) for f in findings] == [
        ("tests/test_quality.py", 3, "test_without_assertion"),
        ("tests/test_quality.py", 6, "test_did_not_throw_only"),
        ("tests/test_quality.py", 26, "test_assert_true_equivalent"),
        ("tests/test_quality.py", 29, "test_unittest_assert_true_equivalent"),
        (
            "tests/test_quality.py",
            32,
            "test_unittest_assert_true_compare_equivalent",
        ),
        ("tests/test_quality.py", 38, "test_nested_assertion_does_not_count"),
    ]


def test_assertion_free_scanner_counts_configured_assertion_helpers(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        'assertion_helpers = ["ensure_contract", "contracts.require_valid"]\n',
        encoding="utf-8",
    )
    path = tmp_path / "tests" / "test_quality.py"
    path.parent.mkdir()
    path.write_text(
        "\n".join(
            [
                "import contracts",
                "",
                "def test_registered_leaf_helper():",
                "    ensure_contract({'ok': True})",
                "",
                "def test_registered_dotted_helper():",
                "    contracts.require_valid({'ok': True})",
                "",
                "def test_unregistered_validator_call_still_flags():",
                "    contracts.other_validator({'ok': True})",
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    findings = scan_assertion_free_tests([Path("tests/test_quality.py")], root=tmp_path)

    assert [(f.path, f.line, f.test_name) for f in findings] == [
        (
            "tests/test_quality.py",
            9,
            "test_unregistered_validator_call_still_flags",
        )
    ]


def test_assertion_free_scanner_rejects_invalid_helper_config(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nassertion_helpers = "ensure_contract"\n',
        encoding="utf-8",
    )
    path = tmp_path / "tests" / "test_quality.py"
    path.parent.mkdir()
    path.write_text(
        "def test_helper_call():\n    ensure_contract({})\n", encoding="utf-8"
    )

    with pytest.raises(SpiceError, match="assertion_helpers must be a list"):
        scan_assertion_free_tests([Path("tests/test_quality.py")], root=tmp_path)


def test_study_assertion_free_cli_reports_findings(tmp_path, monkeypatch, capsys):
    path = tmp_path / "tests" / "test_quality.py"
    path.parent.mkdir()
    path.write_text("def test_without_assertion():\n    value = 1\n", encoding="utf-8")
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "assertion-free-tests"])

    assert args.func(args) == 1
    output = capsys.readouterr().out
    assert "assertion-free-tests: 1 test(s)" in output
    assert "tests/test_quality.py:1 test_without_assertion" in output


def test_assertion_free_board_reports_clean_baseline():
    assert render_assertion_free_board([]) == (
        "assertion-free-tests: no assertion-free tests found"
    )


def test_assertion_free_scanner_detects_suffix_named_files(tmp_path):
    path = tmp_path / "tests" / "quality_test.py"
    path.parent.mkdir()
    path.write_text(
        "def test_without_assertion():\n    value = 1\n",
        encoding="utf-8",
    )

    findings = scan_assertion_free_tests([Path("tests/quality_test.py")], root=tmp_path)

    assert len(findings) == 1
    assert findings[0].test_name == "test_without_assertion"
    assert findings[0].path == "tests/quality_test.py"


def test_testquality_discovers_configured_multi_root_test_paths(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\ntest_paths = ["tests", "Assets/**/Tests"]\n',
        encoding="utf-8",
    )
    default_path = tmp_path / "tests" / "test_quality.py"
    default_path.parent.mkdir()
    default_path.write_text(
        "def test_without_assertion():\n    value = 1\n", encoding="utf-8"
    )
    unity_root = tmp_path / "Assets" / "Game" / "Tests"
    unity_root.mkdir(parents=True)
    (unity_root / "test_quality.py").write_text(
        "def test_without_assertion():\n    value = 2\n", encoding="utf-8"
    )
    (unity_root / "test_private.py").write_text(
        "from spice.worker import _private_helper\n\n"
        "def test_private_import():\n"
        "    value = 1\n"
        "    assert value == 1\n",
        encoding="utf-8",
    )
    (unity_root / "helper.py").write_text(
        "def test_helper_name_but_file_not_a_test():\n    pass\n", encoding="utf-8"
    )
    skipped = tmp_path / "Assets" / "Game" / "NotTests" / "test_skip.py"
    skipped.parent.mkdir()
    skipped.write_text("def test_skip():\n    pass\n", encoding="utf-8")

    paths = testquality.test_paths(tmp_path)

    assert [path.as_posix() for path in paths] == [
        "Assets/Game/Tests/test_private.py",
        "Assets/Game/Tests/test_quality.py",
        "tests/test_quality.py",
    ]
    assertion_findings = scan_assertion_free_tests(paths, root=tmp_path)
    assert [(f.path, f.test_name) for f in assertion_findings] == [
        ("Assets/Game/Tests/test_quality.py", "test_without_assertion"),
        ("tests/test_quality.py", "test_without_assertion"),
    ]
    private_findings = scan_private_internal_coupling(paths, root=tmp_path)
    assert [(f.path, f.test_name, f.target) for f in private_findings] == [
        (
            "Assets/Game/Tests/test_private.py",
            "<module>",
            "spice.worker._private_helper",
        )
    ]


def test_assertion_free_scanner_detects_class_methods(tmp_path):
    path = tmp_path / "tests" / "test_class.py"
    path.parent.mkdir()
    path.write_text(
        "\n".join(
            [
                "class TestSuite:",
                "    def test_without_assertion(self):",
                "        value = 1",
                "",
                "    def test_with_assert(self):",
                "        value = 1",
                "        assert value == 1",
                "",
                "    def helper_not_a_test(self):",
                "        pass",
                "",
                "class NotATestClass:",
                "    def test_ignored(self):",
                "        value = 2",
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    findings = scan_assertion_free_tests([Path("tests/test_class.py")], root=tmp_path)

    assert [(f.test_name, f.line) for f in findings] == [
        ("TestSuite.test_without_assertion", 2),
    ]


def test_private_internal_scanner_flags_imports_and_internal_assertions(tmp_path):
    path = tmp_path / "tests" / "test_private.py"
    path.parent.mkdir()
    path.write_text(
        "\n".join(
            [
                "from spice.worker import _private_helper",
                "from spice._secret import public_helper",
                "",
                "def test_public_contract_stays_clean():",
                "    assert {'public': 1}['public'] == 1",
                "",
                "def test_private_key_assertion():",
                "    assert {'_state': 1}['_state'] == 1",
                "",
                "def test_private_shape_assertion(result):",
                "    assert result == {'_phase': 'claimed'}",
                "",
                "def test_private_attr_assertion(result):",
                "    assert result._state == 1",
            ]
        )
        + "\n",
        encoding="utf-8",
    )

    findings = scan_private_internal_coupling(
        [Path("tests/test_private.py")], root=tmp_path
    )

    assert [(f.line, f.test_name, f.kind, f.target) for f in findings] == [
        (1, "<module>", "private import", "spice.worker._private_helper"),
        (2, "<module>", "private import", "spice._secret"),
        (8, "test_private_key_assertion", "private key assertion", "_state"),
        (11, "test_private_shape_assertion", "private key assertion", "_phase"),
        (
            14,
            "test_private_attr_assertion",
            "private attribute assertion",
            "_state",
        ),
    ]


def test_study_private_internals_cli_reports_findings(tmp_path, monkeypatch, capsys):
    path = tmp_path / "tests" / "test_private.py"
    path.parent.mkdir()
    path.write_text(
        "from spice.worker import _private_helper\n"
        "def test_public_contract():\n"
        "    assert 1 == 1\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "private-internals"])

    assert args.func(args) == 1
    output = capsys.readouterr().out
    assert "private-internals: 1 coupling(s)" in output
    assert "private import spice.worker._private_helper" in output


def test_study_private_internals_cli_honors_configured_couplings(
    tmp_path, monkeypatch, capsys
):
    path = tmp_path / "tests" / "test_private.py"
    path.parent.mkdir()
    path.write_text(
        "from spice.worker import _private_helper\n"
        "def test_public_contract():\n"
        "    assert 1 == 1\n",
        encoding="utf-8",
    )
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "internal_couplings = [\n"
        '  { path = "tests/test_private.py", test = "<module>", '
        'target = "spice.worker._private_helper" },\n'
        "]\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "private-internals"])

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "private-internals: no unmanaged private test coupling found" in output


def test_private_internal_board_reports_clean_baseline():
    assert render_private_internal_board([]) == (
        "private-internals: no private test coupling found"
    )


def test_subsumption_identifies_fully_subsumed_test(tmp_path):
    db = _write_coverage_db(
        tmp_path,
        files=["spice/foo.py"],
        contexts={
            "test_a": {0: [1, 2, 3]},
            "test_b": {0: [1, 2, 3, 4]},
        },
    )

    report = scan_subsumption(db)

    assert report.tests_scanned == 2
    assert len(report.findings) == 1
    assert report.findings[0].test == "test_a"
    assert report.findings[0].subsumed_by == "test_b"
    assert report.findings[0].covered_lines == 3


def test_subsumption_equal_features_retain_deterministic_representative(tmp_path):
    db = _write_coverage_db(
        tmp_path,
        files=["spice/foo.py"],
        contexts={
            "test_z": {0: [1, 2, 3]},
            "test_a": {0: [1, 2, 3]},
            "test_m": {0: [1, 2, 3]},
        },
    )

    report = scan_subsumption(db)

    assert [(finding.test, finding.subsumed_by) for finding in report.findings] == [
        ("test_m", "test_a"),
        ("test_z", "test_a"),
    ]
    assert len(report.cohorts) == 1
    assert report.cohorts[0].relation == "equal-feature"
    assert report.cohorts[0].representative == "test_a"
    assert report.cohorts[0].candidates == ("test_m", "test_z")


def test_subsumption_reports_analyzed_excluded_and_context_free_denominators(
    tmp_path,
):
    db = _write_coverage_db_v7(
        tmp_path,
        files=["spice/foo.py", "tests/test_foo.py"],
        contexts={
            "test_analyzed": {0: [1, 2]},
            "test_excluded": {1: [1, 2]},
            "": {0: [3]},
        },
    )

    report = scan_subsumption(db, package_prefix="spice")

    assert report.tests_scanned == 1
    assert report.source_files_scanned == 1
    assert report.coverage_contexts == 2
    assert report.excluded_test_contexts == 1
    assert report.context_free_contexts == 1


def _seed_checkout(root: Path) -> dict[str, str]:
    root.mkdir()
    original = {
        "tracked.txt": "tracked\n",
        "staged.txt": "staged\n",
        "untracked.txt": "untracked\n",
    }
    for name, content in original.items():
        (root / name).write_text(content, encoding="utf-8")
    return original


def _write_interpreter_with_dev_dependencies(root: Path) -> Path:
    """A checkout-local interpreter double whose dependency probe reports no gaps.

    It must not borrow ``sys.prefix``: the harness running this test need not carry
    the checkout's pytest-cov development dependency.
    """
    python = root / ".venv" / "bin" / "python"
    python.parent.mkdir(parents=True)
    python.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    python.chmod(0o755)
    return python


def _write_interpreter_missing_dev_dependencies(root: Path) -> Path:
    """A real interpreter path whose probe reports every requirement absent."""
    python = root / ".venv" / "bin" / "python"
    python.parent.mkdir(parents=True)
    python.write_text('#!/bin/sh\nshift 2\nprintf "%s\\n" "$*"\n', encoding="utf-8")
    python.chmod(0o755)
    return python


def _checkout_files(root: Path) -> dict[str, str]:
    return {
        path.name: path.read_text(encoding="utf-8")
        for path in root.iterdir()
        if path.is_file()
    }


def test_record_subsumption_uses_disposable_artifacts_and_preserves_checkout(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    root = tmp_path / "checkout"
    original = _seed_checkout(root)
    interpreter = _write_interpreter_with_dev_dependencies(root)
    observed: dict[str, object] = {}
    real_run_tool_command = subsumption.run_tool_command

    def fake_run(command, *, policy, operation, cwd, capture_output, check, **kwargs):
        if policy == "probe":
            # The dependency probe is a real subprocess against the checkout's
            # declared interpreter; only the coverage run is stood in for here.
            return real_run_tool_command(
                command,
                policy=policy,
                operation=operation,
                cwd=cwd,
                capture_output=capture_output,
                check=check,
                **kwargs,
            )
        env = kwargs["env"]
        coverage_path = Path(env["COVERAGE_FILE"])
        junit_arg = next(arg for arg in command if arg.startswith("--junitxml="))
        junit_path = Path(junit_arg.split("=", 1)[1])
        generated = _write_coverage_db_v7(
            coverage_path.parent,
            files=["spice/foo.py"],
            contexts={
                "test_a": {0: [1, 2]},
                "test_b": {0: [1, 2, 3]},
            },
        )
        generated.replace(coverage_path)
        junit_path.write_text(
            '<testsuites tests="2"><testsuite tests="2" /></testsuites>',
            encoding="utf-8",
        )
        observed.update(
            command=tuple(command),
            cwd=cwd,
            coverage_path=coverage_path,
            check=check,
            policy=policy,
            operation=operation,
            capture_output=capture_output,
            env=dict(env),
        )
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr("spice.studies.subsumption.run_tool_command", fake_run)

    report = record_subsumption(
        root, package="spice", package_prefix="spice", pytest_args=["-k", "smoke"]
    )

    assert report.suite_tests == 2
    assert report.tests_scanned == 2
    assert report.coverage_artifact is None
    assert observed["cwd"] == root
    assert observed["check"] is False
    assert observed["policy"] == "coverage"
    assert observed["operation"] == "record subsumption coverage"
    assert observed["capture_output"] is False
    assert observed["command"][0] == str(interpreter)
    assert observed["command"][0] != sys.executable
    assert observed["command"][1:4] == ("-B", "-m", "pytest")
    assert observed["command"][-2:] == ("-k", "smoke")
    assert observed["env"]["COVERAGE_FILE"] == str(observed["coverage_path"])
    assert "--cov-context=test" in observed["command"]
    assert "--cov-branch" in observed["command"]
    assert str(observed["coverage_path"]).startswith(tempfile.gettempdir())
    assert _checkout_files(root) == original


def test_record_subsumption_refuses_a_checkout_with_no_declared_test_runtime(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    root = tmp_path / "checkout"
    original = _seed_checkout(root)

    with pytest.raises(SpiceError, match="uv sync --group dev") as failure:
        record_subsumption(
            root,
            package="spice",
            package_prefix="spice",
            coverage_output=Path("artifacts/coverage.db"),
        )

    assert "declares no virtualenv and no uv project" in str(failure.value)
    assert _checkout_files(root) == original
    assert {path.name for path in root.iterdir()} == set(original)


def test_record_subsumption_names_the_dependency_its_interpreter_cannot_import(
    tmp_path, monkeypatch
):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    root = tmp_path / "checkout"
    original = _seed_checkout(root)
    interpreter = _write_interpreter_missing_dev_dependencies(root)

    with pytest.raises(SpiceError, match="uv sync --group dev") as failure:
        record_subsumption(
            root,
            package="spice",
            package_prefix="spice",
            coverage_output=Path("artifacts/coverage.db"),
        )

    message = str(failure.value)
    assert str(interpreter) in message
    assert "cannot import pytest, pytest_cov" in message
    assert _checkout_files(root) == original
    assert {path.name for path in root.iterdir()} == {*original, ".venv"}


def test_subsumption_no_findings_when_tests_are_disjoint(tmp_path):
    db = _write_coverage_db(
        tmp_path,
        files=["spice/foo.py"],
        contexts={
            "test_a": {0: [1, 2]},
            "test_b": {0: [3, 4]},
        },
    )

    report = scan_subsumption(db)

    assert report.findings == ()


def test_subsumption_raises_on_missing_coverage_file(tmp_path):
    with pytest.raises(FileNotFoundError, match="coverage file not found"):
        scan_subsumption(tmp_path / ".coverage")


def _write_coverage_db(
    root: Path,
    *,
    files: list[str],
    contexts: dict[str, dict[int, list[int]]],
) -> Path:
    path = root / ".coverage"
    with sqlite_connection(path) as con:
        con.execute("CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT)")
        con.execute("CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT)")
        con.execute(
            "CREATE TABLE lines "
            "(id INTEGER PRIMARY KEY, file_id INTEGER, context_id INTEGER, "
            "lineno INTEGER)"
        )
        for fid, fpath in enumerate(files, 1):
            con.execute(f"INSERT INTO file VALUES ({fid}, '{fpath}')")
        line_id = 1
        for cid, (ctx_name, file_lines) in enumerate(contexts.items(), 1):
            con.execute(f"INSERT INTO context VALUES ({cid}, '{ctx_name}')")
            for file_index, lines in file_lines.items():
                for lineno in lines:
                    con.execute(
                        "INSERT INTO lines VALUES "
                        f"({line_id},{file_index + 1},{cid},{lineno})"
                    )
                    line_id += 1
    return path


def _nums_to_numbits(nums: list[int]) -> bytes:
    """Encode a list of 1-based line numbers into a coverage.py v7 numbits blob."""
    buf = bytearray()
    for num in nums:
        n = num - 1  # 0-based bit index
        byte_idx = n // 8
        bit_idx = n % 8
        while byte_idx >= len(buf):
            buf.append(0)
        buf[byte_idx] |= 1 << bit_idx
    return bytes(buf)


def _write_coverage_db_v7(
    root: Path,
    *,
    files: list[str],
    contexts: dict[str, dict[int, list[int]]],
    arcs: dict[str, dict[int, list[tuple[int, int]]]] | None = None,
) -> Path:
    path = root / ".coverage"
    with sqlite_connection(path) as con:
        con.execute("CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT)")
        con.execute("CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT)")
        con.execute(
            "CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB)"
        )
        if arcs is not None:
            con.execute(
                "CREATE TABLE arc "
                "(file_id INTEGER, context_id INTEGER, fromno INTEGER, tono INTEGER)"
            )
        for fid, fpath in enumerate(files, 1):
            con.execute(f"INSERT INTO file VALUES ({fid}, '{fpath}')")
        # Collect all context names; arc-only contexts may not appear in line
        # coverage.
        all_ctx_names = list(contexts.keys())
        if arcs is not None:
            for name in arcs:
                if name not in contexts:
                    all_ctx_names.append(name)
        for cid, ctx_name in enumerate(all_ctx_names, 1):
            con.execute(f"INSERT INTO context VALUES ({cid}, '{ctx_name}')")
            if ctx_name in contexts:
                for file_index, lines in contexts[ctx_name].items():
                    numbits = _nums_to_numbits(lines)
                    con.execute(
                        "INSERT INTO line_bits VALUES (?, ?, ?)",
                        (file_index + 1, cid, numbits),
                    )
            if arcs is not None:
                for file_index, arc_list in arcs.get(ctx_name, {}).items():
                    for fromno, tono in arc_list:
                        con.execute(
                            "INSERT INTO arc VALUES (?, ?, ?, ?)",
                            (file_index + 1, cid, fromno, tono),
                        )
    return path


def test_subsumption_v7_identifies_subsumed_test(tmp_path):
    db = _write_coverage_db_v7(
        tmp_path,
        files=["spice/foo.py"],
        contexts={
            "test_a": {0: [1, 2, 3]},
            "test_b": {0: [1, 2, 3, 4]},
        },
    )

    report = scan_subsumption(db)

    assert report.tests_scanned == 2
    assert len(report.findings) == 1
    assert report.findings[0].test == "test_a"
    assert report.findings[0].subsumed_by == "test_b"
    assert report.findings[0].covered_lines == 3


def test_subsumption_v7_edge_line_numbers(tmp_path):
    # Line 1 (byte 0 bit 0), line 8 (byte 0 bit 7), line 9 (byte 1 bit 0)
    db = _write_coverage_db_v7(
        tmp_path,
        files=["spice/foo.py"],
        contexts={
            "test_edge": {0: [1, 8, 9]},
            "test_super": {0: [1, 8, 9, 16]},
        },
    )

    report = scan_subsumption(db)

    assert len(report.findings) == 1
    assert report.findings[0].test == "test_edge"
    assert report.findings[0].covered_lines == 3


def test_subsumption_same_lines_distinct_arcs_not_subsumed(tmp_path):
    # Two tests covering identical lines but different branch arcs — NOT subsumed.
    db = _write_coverage_db_v7(
        tmp_path,
        files=["spice/foo.py"],
        contexts={
            "test_true_branch": {0: [1, 2]},
            "test_false_branch": {0: [1, 2]},
        },
        arcs={
            "test_true_branch": {0: [(1, 2)]},  # true branch: 1→2
            "test_false_branch": {0: [(1, -1)]},  # false branch: 1→exit
        },
    )

    report = scan_subsumption(db)

    assert report.findings == ()


def test_subsumption_same_lines_same_arcs_is_subsumed(tmp_path):
    # Two tests with identical lines and identical arcs — IS subsumed.
    db = _write_coverage_db_v7(
        tmp_path,
        files=["spice/foo.py"],
        contexts={
            "test_a": {0: [1, 2]},
            "test_b": {0: [1, 2, 3]},
        },
        arcs={
            "test_a": {0: [(1, 2)]},
            "test_b": {0: [(1, 2), (2, 3)]},
        },
    )

    report = scan_subsumption(db)

    assert len(report.findings) == 1
    assert report.findings[0].test == "test_a"


def test_subsumption_arc_only_database_counts_tests_and_files(tmp_path):
    # Database with arc table but no line_bits rows (branch-only coverage fixture).
    db = _write_coverage_db_v7(
        tmp_path,
        files=["spice/foo.py"],
        contexts={},  # no line_bits rows
        arcs={
            "test_true_branch": {0: [(1, 2)]},
            "test_false_branch": {0: [(1, -1)]},
        },
    )

    report = scan_subsumption(db)

    assert report.tests_scanned == 2
    assert report.source_files_scanned == 1
    assert report.findings == ()  # distinct arcs → not subsumed


def test_subsumption_arc_only_detects_subsumed_test(tmp_path):
    # Arc-only database where test_a's arcs are a strict subset of test_b's.
    db = _write_coverage_db_v7(
        tmp_path,
        files=["spice/foo.py"],
        contexts={},
        arcs={
            "test_a": {0: [(1, 2)]},
            "test_b": {0: [(1, 2), (2, 3)]},
        },
    )

    report = scan_subsumption(db)

    assert report.tests_scanned == 2
    assert len(report.findings) == 1
    assert report.findings[0].test == "test_a"
    assert report.findings[0].subsumed_by == "test_b"
    assert report.findings[0].covered_lines == 0  # no line coverage, only arcs
    assert report.findings[0].covered_arcs == 1
    assert report.findings[0].covered_features == 1


def test_generated_lockfiles_are_pruned_from_file_shape_sticky_state(tmp_path):
    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    lock_path = Path("uv.lock")
    sticky_source_path = Path("sticky_source.py")
    (repo / lock_path).write_text("package = []\n" * 20, encoding="utf-8")
    (repo / sticky_source_path).write_text(
        "print('large')\n" * 20,
        encoding="utf-8",
    )
    ledgers = (
        gates.path_sticky_ledger(
            fileloc.FILE_LOC_STICKY_STATE_GIT_PATH,
            version=fileloc.FILE_LOC_VERSION,
        ),
        gates.path_sticky_ledger(
            fileloc.FILE_BYTE_STICKY_STATE_GIT_PATH,
            version=fileloc.FILE_LOC_VERSION,
        ),
    )
    for ledger in ledgers:
        gates.persist_sticky_ledger(
            ledger,
            {lock_path, sticky_source_path},
            root=repo,
        )

    findings = scan_staged_loc_violations(
        [lock_path],
        root=repo,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
        persist=True,
    )

    assert findings == []
    assert [
        gates.load_sticky_ledger(ledger, root=repo, renames={}) for ledger in ledgers
    ] == [{sticky_source_path}, {sticky_source_path}]


def test_sticky_function_keys_follow_renames():
    sticky = sticky_function_keys_after_renames(
        {("old.py", "run")}, {Path("old.py"): Path("new.py")}
    )
    assert sticky == {("old.py", "run"), ("new.py", "run")}


def test_python_comparison_pivot_is_flagged():
    findings = scan_text_magic_numbers(
        Path("sample.py"), "def f(n):\n    return n > 75\n"
    )
    assert [(finding.line, finding.literal) for finding in findings] == [(2, "75")]


def test_magic_threshold_is_explicit_scan_policy():
    findings = scan_text_magic_numbers(
        Path("sample.py"),
        f"def f(n):\n    return n > {MAGIC_HIGH_LITERAL}\n",
        examine_threshold=MAGIC_HIGH_THRESHOLD,
    )
    assert [(finding.line, finding.literal) for finding in findings] == [
        (2, MAGIC_HIGH_LITERAL)
    ]


def test_python_named_constant_and_call_args_pass():
    text = "LIMIT = 4096\n\n\ndef f(handle):\n    return handle.read(4096)\n"
    assert scan_text_magic_numbers(Path("sample.py"), text) == []


def test_python_small_comparisons_pass():
    text = "def f(items):\n    return len(items) > 2\n"
    assert scan_text_magic_numbers(Path("sample.py"), text) == []


def test_js_comparison_pivot_is_flagged():
    findings = scan_text_magic_numbers(
        Path("sample.js"), "if (delta > 75) {\n  grow();\n}\n"
    )
    assert [(finding.line, finding.literal) for finding in findings] == [(1, "75")]


def test_js_default_arg_is_flagged_by_tree_sitter_scan():
    text = "function run(count = 75) {\n  return count;\n}\n"
    findings = scan_text_magic_numbers(Path("sample.js"), text)
    assert [(finding.line, finding.literal) for finding in findings] == [(1, "75")]


def test_tree_sitter_magic_scan_honors_threshold():
    findings = scan_text_magic_numbers(
        Path("sample.js"),
        "if (delta > 75) {\n  grow();\n}\n",
        examine_threshold=100,
    )
    assert findings == []


def test_js_const_definitions_and_call_args_pass():
    text = "const messageLimit = 400;\nsetTimeout(tick, 600);\nx = y * 1000;\n"
    assert scan_text_magic_numbers(Path("sample.js"), text) == []


def test_js_arrow_and_shift_operators_pass():
    text = "const f = (x) => 500;\nconst y = bits >> 16;\n"
    assert scan_text_magic_numbers(Path("sample.js"), text) == []


def test_csharp_tree_sitter_magic_positions_are_flagged():
    text = (
        "public class Demo {\n"
        "  const int LIMIT = 75;\n"
        "  void Run(int count = 75) {\n"
        "    if (count > 75) {}\n"
        "    if (count is 99) {}\n"
        "    var part = items[10..20];\n"
        "    Use(600);\n"
        "  }\n"
        "}\n"
    )
    findings = scan_text_magic_numbers(Path("sample.cs"), text)
    assert [(finding.line, finding.literal) for finding in findings] == [
        (3, "75"),
        (4, "75"),
        (5, "99"),
        (6, "10"),
        (6, "20"),
    ]


def test_c_grammar_family_covers_other_languages():
    go_findings = scan_text_magic_numbers(
        Path("sample.go"), "if delta > 75 {\n\tgrow()\n}\n"
    )
    rust_findings = scan_text_magic_numbers(
        Path("sample.rs"), "if delta > 75 { grow(); } // limit\n"
    )
    assert [(f.line, f.literal) for f in go_findings] == [(1, "75")]
    assert [(f.line, f.literal) for f in rust_findings] == [(1, "75")]


def test_mutation_points_and_mutated_text_flip_operator():
    text = "def add(a, b):\n    return a + b\n"

    points = mutations.mutation_points_for_text(text)
    mutated = mutations.mutated_text(text, points[0].index)

    assert points[0].description == "replace + with -"
    assert "return a - b" in mutated


def test_changed_python_paths_skips_configured_test_roots(tmp_path, monkeypatch):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\ntest_paths = ["tests", "Assets/**/Tests"]\n',
        encoding="utf-8",
    )
    source = tmp_path / "pkg" / "sample.py"
    default_test = tmp_path / "tests" / "test_sample.py"
    unity_test = tmp_path / "Assets" / "Game" / "Tests" / "test_sample.py"
    for path in (source, default_test, unity_test):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("VALUE = 1\n", encoding="utf-8")

    def fake_run(command, **kwargs):
        assert command[:2] == ["git", "diff"]
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=(
                "pkg/sample.py\n"
                "tests/test_sample.py\n"
                "Assets/Game/Tests/test_sample.py\n"
            ),
            stderr="",
        )

    monkeypatch.setattr(mutations, "run_git_command", fake_run)

    assert mutations.changed_python_paths(tmp_path) == [Path("pkg/sample.py")]


def test_mutation_study_scores_module_and_records_killing_tests(tmp_path, monkeypatch):
    subprocess.run(
        ["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True, text=True
    )
    source = tmp_path / "pkg" / "sample.py"
    source.parent.mkdir()
    source.write_text("def add(a, b):\n    return a + b\n", encoding="utf-8")
    test_path = Path("tests/test_sample.py")

    def fake_run(command, **kwargs):
        if "--collect-only" in command:
            return subprocess.CompletedProcess(
                command,
                0,
                stdout="tests/test_sample.py::test_add\n",
                stderr="",
            )
        if "pytest" in command:
            # Mutants apply inside the scratch checkout pytest runs from,
            # never in the caller's file.
            scratch_source = Path(kwargs["cwd"]) / "pkg" / "sample.py"
            if "return a - b" in scratch_source.read_text(encoding="utf-8"):
                return subprocess.CompletedProcess(
                    command,
                    1,
                    stdout="FAILED tests/test_sample.py::test_add - AssertionError\n",
                    stderr="",
                )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
        raise AssertionError(f"unexpected command: {command}")

    monkeypatch.setattr(mutations, "run_bounded_process_group", fake_run)
    monkeypatch.setattr(mutations, "run_tool_command", fake_run)

    study = mutations.run_mutation_study(
        [Path("pkg/sample.py")],
        root=tmp_path,
        test_paths=[test_path],
        max_mutants_per_module=1,
        timeout_seconds=5,
    )

    report = study.reports[0]
    assert report.path == "pkg/sample.py"
    assert report.killed == 1
    assert report.survived == 0
    assert report.score == 1.0
    assert report.zero_constraint_tests == ()
    assert source.read_text(encoding="utf-8") == "def add(a, b):\n    return a + b\n"


def test_mutation_board_flags_zero_constraint_tests():
    point = mutations.MutationPoint(index=0, line=1, description="flip")
    study = mutations.MutationStudy(
        reports=(
            mutations.ModuleMutationReport(
                path="pkg/sample.py",
                mutants=1,
                killed=0,
                survived=1,
                timed_out=0,
                results=(
                    mutations.MutationResult(
                        point=point,
                        status="survived",
                        source_sha256="fixture-sha256",
                        timeout_seconds=5,
                        attempt_outcomes=("survived",),
                    ),
                ),
                zero_constraint_tests=("tests/test_sample.py::test_add",),
            ),
        ),
        ratchet_regressions=(
            mutations.RatchetRegression(
                path="pkg/sample.py",
                baseline_score=1.0,
                current_score=0.0,
            ),
        ),
    )

    board = mutations.render_mutation_board(study)

    assert "pkg/sample.py | 0/1 | 1 | 0 | 0%" in board
    assert "- pkg/sample.py: tests/test_sample.py::test_add" in board
    assert "- pkg/sample.py: 0% < 100%" in board


def test_write_mutation_ratchet_preserves_standing_invocation_policy(tmp_path):
    ratchet = tmp_path / "mutation-ratchet.json"
    standing = {
        "surface": "spice dev doctor",
        "targets": ["pkg/sample.py"],
        "tests": ["tests/test_sample.py"],
    }
    ratchet.write_text(
        json.dumps({"version": 1, "standing": standing, "modules": {}}),
        encoding="utf-8",
    )
    report = mutations.ModuleMutationReport(
        path="pkg/sample.py",
        mutants=2,
        killed=1,
        survived=1,
        timed_out=0,
        results=(),
    )

    mutations.write_ratchet(ratchet, (report,))

    payload = json.loads(ratchet.read_text(encoding="utf-8"))
    assert payload["standing"] == standing
    assert payload["modules"] == {
        "pkg/sample.py": {
            "score": 0.5,
            "killed": 1,
            "survived": 1,
            "timed_out": 0,
            "mutants": 2,
        }
    }


def test_mutation_cli_resolves_ratchet_paths_from_repo_root(tmp_path, monkeypatch):
    calls = {}

    def fake_run_mutation_study(paths, **kwargs):
        calls["paths"] = paths
        calls["ratchet_path"] = kwargs["ratchet_path"]
        return mutations.MutationStudy(reports=())

    def fake_write_ratchet(path, reports):
        calls["write_ratchet_path"] = path
        calls["written_reports"] = reports
        return path

    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        studies_cli.mutations, "run_mutation_study", fake_run_mutation_study
    )
    monkeypatch.setattr(studies_cli.mutations, "write_ratchet", fake_write_ratchet)
    args = build_parser().parse_args(
        [
            "study",
            "mutations",
            "pkg/sample.py",
            "--ratchet",
            ".spice/mutation-ratchet.json",
            "--write-ratchet",
            ".spice/mutation-ratchet.json",
        ]
    )

    assert args.func(args) == 0
    assert calls["paths"] == [Path("pkg/sample.py")]
    assert calls["ratchet_path"] == tmp_path / ".spice/mutation-ratchet.json"
    assert calls["write_ratchet_path"] == tmp_path / ".spice/mutation-ratchet.json"
    assert calls["written_reports"] == ()


def test_c_grammar_comments_pass():
    text = "// retries > 75 is too many\n/* delta > 99 */\nlet x = 5;\n"
    assert scan_text_magic_numbers(Path("sample.rs"), text) == []
