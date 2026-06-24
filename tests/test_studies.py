"""Constitution mechanics: flex ratio, sticky state, magic-number verdicts."""

import subprocess
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.flexstate import (
    sticky_function_keys_after_renames,
    sticky_items_after_flex_breaches,
    sticky_paths_after_renames,
)
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    ENV_POLICY_ALLOW_MARKER,
    FILE_LOC_LIMIT,
    FLEX_DENOMINATOR,
    FLEX_NUMERATOR,
    flex_limit,
)
from spice.studies import cli as studies_cli
from spice.studies.envpolicy import render_env_policy_board, scan_env_policy
from spice.studies.fileloc import scan_loc_violations, scan_staged_loc_violations
from spice.studies import mutations
from spice.studies.reachability import (
    ReachabilityFinding,
    render_reachability_board,
    render_symbol_reachability_board,
    scan_reachability,
    scan_symbol_reachability,
)
from spice.studies.subsumption import scan_subsumption
from spice.studies.magicnums import scan_text_magic_numbers
from spice.studies.shape import (
    configured_package_roots,
    name_cluster_error,
    name_cluster_errors,
)
from spice.studies.testquality import (
    render_assertion_free_board,
    render_private_internal_board,
    scan_assertion_free_tests,
    scan_private_internal_coupling,
)
from spice.studies.typecheck import (
    PYRIGHT_ARGS,
    python_typecheck_argv,
    python_typecheck_targets,
    run_python_typecheck,
)

MAGIC_HIGH_THRESHOLD = 100
MAGIC_HIGH_LITERAL = "125"


def test_flex_ratio_is_three_halves():
    for limit in (FILE_LOC_LIMIT, COMPLEXITY_MAX_LENGTH, COMPLEXITY_MAX_CCN):
        assert flex_limit(limit) == limit * FLEX_NUMERATOR // FLEX_DENOMINATOR


def test_flex_breach_joins_sticky_set():
    sticky = sticky_items_after_flex_breaches(
        [("a.py", 1600), ("b.py", 900)],
        {Path("c.py")},
        key_for_item=lambda item: Path(item[0]),
        is_breach=lambda item: item[1] > flex_limit(FILE_LOC_LIMIT),
    )
    assert sticky == {Path("a.py"), Path("c.py")}


def test_sticky_paths_follow_renames():
    sticky = sticky_paths_after_renames(
        {Path("old.py")}, {Path("old.py"): Path("new.py")}
    )
    assert sticky == {Path("old.py"), Path("new.py")}


def test_binary_assets_are_byte_gated_but_not_line_gated(tmp_path):
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

    assert len(findings) == 1
    assert findings[0].line_count == 0
    assert not findings[0].over_line_limit
    assert findings[0].over_byte_limit


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


def test_private_internal_board_reports_clean_baseline():
    assert render_private_internal_board([]) == (
        "private-internals: no private test coupling found"
    )


def test_reachability_scans_test_files_outside_package_root(tmp_path):
    _write_reachability_repo(tmp_path, "import spice.onlytest\n")

    findings = scan_reachability(tmp_path)

    assert [(f.module, f.module_path, f.only_test_imports) for f in findings] == [
        ("spice.onlytest", "spice/onlytest.py", ["test_only.py"])
    ]


def test_reachability_expands_from_imported_submodule(tmp_path):
    _write_reachability_repo(tmp_path, "from spice import onlytest\n")

    findings = scan_reachability(tmp_path)

    assert [(f.module, f.module_path, f.only_test_imports) for f in findings] == [
        ("spice.onlytest", "spice/onlytest.py", ["test_only.py"])
    ]


def test_symbol_reachability_excludes_production_used_local_helpers(tmp_path):
    _write_symbol_reachability_repo(tmp_path)

    module_findings = scan_reachability(tmp_path)
    symbol_findings = scan_symbol_reachability(tmp_path)
    module_output = "\n".join(render_reachability_board(module_findings))
    symbol_output = "\n".join(render_symbol_reachability_board(symbol_findings))

    assert "reachability: 1 test-only module(s)" in module_output
    assert "spice/orphan_module_xyz.py" in module_output
    assert "symbol-reachability: 2 test-only symbol(s)" in symbol_output
    assert "spice/live.py:LiveThing.planted_dead_method_abc" in symbol_output
    assert "spice/live.py:planted_dead_function_abc" in symbol_output
    assert "handle_one_request" not in symbol_output
    assert "shared_helper" not in symbol_output
    assert "shared_method" not in symbol_output


def test_symbol_reachability_resolves_registry_literal_dispatch(tmp_path):
    """A symbol reached only through a registry dict/list literal in a
    production module is a real production reference: the scanner sees the
    symbol named as a literal value, so registry-dispatched handlers are not
    false-flagged as test-only the way getattr-by-constructed-string would be.
    """
    (tmp_path / "spice" / "cli").mkdir(parents=True)
    (tmp_path / "tests").mkdir()
    (tmp_path / "spice" / "cli" / "entry.py").write_text(
        "from ..registry import DISPATCH, ORDERED\n"
        "def main(key):\n"
        "    DISPATCH[key]()\n"
        "    return ORDERED[0]()\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "registry.py").write_text(
        "from .handlers import handle_dict_only, handle_list_only\n"
        "DISPATCH = {'one': handle_dict_only}\n"
        "ORDERED = [handle_list_only]\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "handlers.py").write_text(
        "def handle_dict_only():\n    return 1\n\n"
        "def handle_list_only():\n    return 2\n\n"
        "def handle_orphan():\n    return 3\n",
        encoding="utf-8",
    )
    (tmp_path / "tests" / "test_handlers.py").write_text(
        "from spice.handlers import (\n"
        "    handle_dict_only,\n"
        "    handle_list_only,\n"
        "    handle_orphan,\n"
        ")\n"
        "def test_handlers():\n"
        "    assert handle_dict_only() == 1\n"
        "    assert handle_list_only() == 2\n"
        "    assert handle_orphan() == 3\n",
        encoding="utf-8",
    )

    findings = scan_symbol_reachability(tmp_path)

    flagged = {f.symbol for f in findings}
    assert flagged == {"handle_orphan"}


def test_symbol_reachability_allowlist_exempts_qualified_symbol(tmp_path):
    _write_symbol_reachability_repo(tmp_path)

    findings = scan_symbol_reachability(
        tmp_path, allowlist=["spice.live.planted_dead_function_abc"]
    )
    output = "\n".join(render_symbol_reachability_board(findings))

    assert "symbol-reachability: 1 test-only symbol(s)" in output
    assert "planted_dead_function_abc" not in output
    assert "spice/live.py:LiveThing.planted_dead_method_abc" in output


def test_symbol_reachability_allowlist_exempts_whole_module(tmp_path):
    _write_symbol_reachability_repo(tmp_path)

    findings = scan_symbol_reachability(tmp_path, allowlist=["spice.live"])

    assert findings == []


def test_study_symbol_reachability_cli_reports_test_only_symbol(
    tmp_path, monkeypatch, capsys
):
    _write_symbol_reachability_repo(tmp_path)
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "symbol-reachability"])

    assert args.func(args) == 1
    output = capsys.readouterr().out
    assert "symbol-reachability: 2 test-only symbol(s)" in output
    assert "spice/live.py:planted_dead_function_abc" in output
    assert "spice.live.planted_dead_function_abc (function)" in output


def test_study_reachability_cli_reports_test_only_module(tmp_path, monkeypatch, capsys):
    _write_reachability_repo(tmp_path, "from spice import onlytest\n")
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "reachability"])

    assert args.func(args) == 1
    output = capsys.readouterr().out
    assert "reachability: 1 test-only module(s)" in output
    assert "spice/onlytest.py" in output
    assert "module: spice.onlytest" in output


def test_study_reachability_cli_create_tasks_passes_findings(
    tmp_path, monkeypatch, capsys
):
    _write_reachability_repo(tmp_path, "from spice import onlytest\n")
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    created_paths: list[str] = []
    monkeypatch.setattr(
        studies_cli,
        "_create_exhaust_tasks",
        lambda findings: created_paths.extend(f.module_path for f in findings),
    )
    args = build_parser().parse_args(["study", "reachability", "--create-tasks"])

    assert args.func(args) == 1

    output = capsys.readouterr().out
    assert "reachability: 1 test-only module(s)" in output
    assert created_paths == ["spice/onlytest.py"]


def test_create_exhaust_tasks_adds_decision_metadata_for_each_finding(
    monkeypatch, capsys
):
    from spice.tasks import create

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
        return f"EXHAUST-{len(created)}"

    monkeypatch.setattr(create, "add", fake_add)

    studies_cli._create_exhaust_tasks(
        [
            ReachabilityFinding(
                module="spice.onlytest",
                module_path="spice/onlytest.py",
                only_test_imports=["tests/test_only.py"],
            ),
            ReachabilityFinding(
                module="spice.empty",
                module_path="spice/empty.py",
                only_test_imports=[],
            ),
        ]
    )

    assert created == [
        {
            "title": "Exhaust decision: wire-in/delete-both spice/onlytest.py",
            "project": "tests.exhaust",
            "tags": ["exhaust", "decision", "wire_in_delete_both"],
            "acceptance": [
                "Resolve spice.onlytest by either wiring it into a production "
                "entry point or deleting spice/onlytest.py along with every "
                "test that imports it.",
                "Current test-only importers: tests/test_only.py.",
            ],
        },
        {
            "title": "Exhaust decision: wire-in/delete-both spice/empty.py",
            "project": "tests.exhaust",
            "tags": ["exhaust", "decision", "wire_in_delete_both"],
            "acceptance": [
                "Resolve spice.empty by either wiring it into a production "
                "entry point or deleting spice/empty.py along with every test "
                "that imports it.",
                "Current test-only importers: unknown.",
            ],
        },
    ]
    assert capsys.readouterr().out == (
        "  task created: EXHAUST-1\n  task created: EXHAUST-2\n"
    )


def test_reachability_merges_default_allowlist(tmp_path):
    _write_reachability_repo(tmp_path, "import spice.release\n", module_name="release")

    assert scan_reachability(tmp_path) == []


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
    import sqlite3

    path = root / ".coverage"
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT)")
    con.execute("CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT)")
    con.execute(
        "CREATE TABLE lines "
        "(id INTEGER PRIMARY KEY, file_id INTEGER, context_id INTEGER, lineno INTEGER)"
    )
    for fid, fpath in enumerate(files, 1):
        con.execute(f"INSERT INTO file VALUES ({fid}, '{fpath}')")
    line_id = 1
    for cid, (ctx_name, file_lines) in enumerate(contexts.items(), 1):
        con.execute(f"INSERT INTO context VALUES ({cid}, '{ctx_name}')")
        for file_index, lines in file_lines.items():
            for lineno in lines:
                con.execute(
                    f"INSERT INTO lines VALUES ({line_id},{file_index + 1},{cid},{lineno})"
                )
                line_id += 1
    con.commit()
    con.close()
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
    import sqlite3

    path = root / ".coverage"
    con = sqlite3.connect(path)
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
    # Collect all context names; arc-only contexts may not appear in line coverage.
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
    con.commit()
    con.close()
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


def _write_reachability_repo(
    root: Path, test_import: str, *, module_name: str = "onlytest"
) -> None:
    (root / "spice" / "cli").mkdir(parents=True)
    (root / "tests").mkdir()
    (root / "spice" / "cli" / "entry.py").write_text("", encoding="utf-8")
    (root / "spice" / f"{module_name}.py").write_text("VALUE = 1\n", encoding="utf-8")
    (root / "tests" / "test_only.py").write_text(test_import, encoding="utf-8")


def _write_symbol_reachability_repo(root: Path) -> None:
    (root / "spice" / "cli").mkdir(parents=True)
    (root / "tests").mkdir()
    (root / "spice" / "cli" / "entry.py").write_text(
        "from ..live import production_function, LiveHandler, LiveThing\n"
        "production_function()\n"
        "LiveThing().production_method()\n"
        "LiveHandler\n",
        encoding="utf-8",
    )
    (root / "spice" / "live.py").write_text(
        "from http.server import BaseHTTPRequestHandler\n\n"
        "def production_function():\n"
        "    return shared_helper()\n\n"
        "def shared_helper():\n"
        "    return 1\n\n"
        "def planted_dead_function_abc():\n"
        "    return 2\n\n"
        "class LiveThing:\n"
        "    def production_method(self):\n"
        "        return self.shared_method()\n\n"
        "    def shared_method(self):\n"
        "        return 3\n\n"
        "    def planted_dead_method_abc(self):\n"
        "        return 4\n"
        "\n"
        "class LiveHandler(BaseHTTPRequestHandler):\n"
        "    def handle_one_request(self):\n"
        "        return None\n",
        encoding="utf-8",
    )
    (root / "spice" / "orphan_module_xyz.py").write_text(
        "def only_tests_call():\n    return 5\n", encoding="utf-8"
    )
    (root / "tests" / "test_symbols.py").write_text(
        "from spice.live import LiveHandler, LiveThing, planted_dead_function_abc, shared_helper\n"
        "import spice.orphan_module_xyz\n\n"
        "def test_symbols():\n"
        "    shared_helper()\n"
        "    planted_dead_function_abc()\n"
        "    LiveHandler.handle_one_request\n"
        "    LiveThing().shared_method()\n"
        "    LiveThing().planted_dead_method_abc()\n",
        encoding="utf-8",
    )


def test_generated_lockfiles_are_pruned_from_file_shape_sticky_state(
    tmp_path, monkeypatch
):
    lock_path = Path("uv.lock")
    sticky_source_path = Path("sticky_source.py")
    (tmp_path / lock_path).write_text("package = []\n" * 20, encoding="utf-8")
    (tmp_path / sticky_source_path).write_text(
        "print('large')\n" * 20,
        encoding="utf-8",
    )
    saved: dict[str, set[Path]] = {}

    monkeypatch.setattr(
        "spice.studies.fileloc.staged_renames",
        lambda _root: {},
    )
    monkeypatch.setattr(
        "spice.studies.fileloc._load_sticky",
        lambda _root, git_path: {lock_path, sticky_source_path},
    )
    monkeypatch.setattr(
        "spice.studies.fileloc._save_sticky",
        lambda paths, _root, git_path: saved.setdefault(git_path, set(paths)),
    )

    findings = scan_staged_loc_violations(
        [lock_path],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
    )

    assert findings == []
    assert len(saved) == 2
    assert all(lock_path not in paths for paths in saved.values())
    assert all(sticky_source_path in paths for paths in saved.values())


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


def test_js_const_definitions_and_call_args_pass():
    text = "const messageLimit = 400;\nsetTimeout(tick, 600);\nx = y * 1000;\n"
    assert scan_text_magic_numbers(Path("sample.js"), text) == []


def test_js_arrow_and_shift_operators_pass():
    text = "const f = (x) => 500;\nconst y = bits >> 16;\n"
    assert scan_text_magic_numbers(Path("sample.js"), text) == []


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


def test_mutation_study_scores_module_and_records_killing_tests(tmp_path, monkeypatch):
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
            if "return a - b" in source.read_text(encoding="utf-8"):
                return subprocess.CompletedProcess(
                    command,
                    1,
                    stdout="FAILED tests/test_sample.py::test_add - AssertionError\n",
                    stderr="",
                )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
        raise AssertionError(f"unexpected command: {command}")

    monkeypatch.setattr(mutations.subprocess, "run", fake_run)

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
                results=(mutations.MutationResult(point=point, status="survived"),),
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


def test_env_policy_defaults_still_apply(tmp_path):
    names = [
        "SPICE_" + "TASK_BACKEND",
        "CODEX_" + "THREAD_ID",
        "CLAUDE_" + "CODE_SESSION_ID",
    ]
    path = tmp_path / "sample.py"
    path.write_text(
        "\n".join(f'VALUE = os.environ["{name}"]' for name in names),
        encoding="utf-8",
    )

    findings = scan_env_policy([Path("sample.py")], root=tmp_path)

    assert [finding.name for finding in findings] == names


def test_env_policy_repo_patterns_merge_with_defaults(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        'env_name_patterns = ["MYPROJ_[A-Z0-9_]+", "ENGINE_[A-Z0-9_]+", "DEPLOY_TARGET"]\n',
        encoding="utf-8",
    )
    names = [
        "SPICE_" + "TASK_BACKEND",
        "CLAUDE_" + "CODE_SESSION_ID",
        "MYPROJ_" + "AUTH_TOKEN",
        "ENGINE_" + "THISISABATCHMODE",
        "DEPLOY_TARGET",
    ]
    path = tmp_path / "sample.cs"
    path.write_text(
        "\n".join(f'var value = "{name}";' for name in names), encoding="utf-8"
    )

    findings = scan_env_policy([Path("sample.cs")], root=tmp_path)

    assert [finding.name for finding in findings] == names


def test_env_policy_allow_marker_guidance_applies_to_repo_patterns(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nenv_name_patterns = ["MYPROJ_[A-Z0-9_]+"]\n',
        encoding="utf-8",
    )
    env_name = "MYPROJ_" + "AUTH_TOKEN"
    path = tmp_path / "sample.py"
    path.write_text(f'VALUE = "{env_name}"\n', encoding="utf-8")

    board = render_env_policy_board(scan_env_policy([Path("sample.py")], root=tmp_path))

    assert f"add `# {ENV_POLICY_ALLOW_MARKER}`" in board
    assert f"sample.py:1: {env_name}" in board


def test_env_policy_previous_line_marker_waives_next_statement(tmp_path):
    env_name = "SPICE_" + "TASK_BACKEND"
    path = tmp_path / "sample.py"
    path.write_text(
        f'# env-policy: allow\nVALUE = "{env_name}"\n',
        encoding="utf-8",
    )

    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def test_env_policy_inline_marker_does_not_waive_next_statement(tmp_path):
    waived_name = "SPICE_" + "TASK_BACKEND"
    unwaived_name = "CODEX_" + "THREAD_ID"
    path = tmp_path / "sample.py"
    path.write_text(
        f'WAIVED = "{waived_name}"  # env-policy: allow\n'
        f'UNWAIVED = "{unwaived_name}"\n',
        encoding="utf-8",
    )

    findings = scan_env_policy([Path("sample.py")], root=tmp_path)

    assert [(finding.line, finding.name) for finding in findings] == [
        (2, unwaived_name)
    ]


def test_env_policy_wrapped_statement_marker_waives_wrapped_literal(tmp_path):
    path = tmp_path / "sample.py"
    path.write_text(
        "monkeypatch.setenv(\n"
        '    "CODEX_THREAD_ID",\n'
        '    "thread-ambient-value-that-makes-the-line-wrap-beyond-the-formatter-limit",\n'
        ")  # env-policy: allow\n",
        encoding="utf-8",
    )

    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def _make_package(root: Path, name: str) -> None:
    pkg = root / name
    pkg.mkdir(parents=True, exist_ok=True)
    (pkg / "mod.py").write_text("x = 1\n", encoding="utf-8")


def test_package_roots_explicit_policy_overrides_derivation(tmp_path):
    _make_package(tmp_path, "chosen")
    _make_package(tmp_path, "ignored")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["chosen"]\n'
        '[tool.setuptools.packages.find]\ninclude = ["ignored*"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "chosen"]


def test_package_roots_derived_from_setuptools_find_excludes_non_python(tmp_path):
    _make_package(tmp_path, "app")
    # A build artifact matching the include glob but carrying no Python.
    (tmp_path / "app.egg-info").mkdir()
    (tmp_path / "app.egg-info" / "PKG-INFO").write_text("meta\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools.packages.find]\nwhere = ["."]\ninclude = ["app*"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_derived_from_explicit_packages_list_keeps_top_level(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools]\npackages = ["app", "app.sub", "app.sub.deep"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_derived_from_poetry_packages(tmp_path):
    _make_package(tmp_path / "src", "pkg")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "pkg"\n'
        '[[tool.poetry.packages]]\ninclude = "pkg"\nfrom = "src"\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "pkg"]


def test_package_roots_derived_from_poetry_name(tmp_path):
    _make_package(tmp_path, "widget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "widget"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "widget"]


def test_package_roots_derived_from_hatch_wheel_packages(tmp_path):
    _make_package(tmp_path / "src", "gadget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.hatch.build.targets.wheel]\npackages = ["src/gadget"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "gadget"]


def test_package_roots_derived_from_hatch_build_include(tmp_path):
    _make_package(tmp_path / "packages", "gadget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.hatch.build]\ninclude = ["packages/gadget"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "packages" / "gadget"]


def test_package_roots_derived_from_flit_module(tmp_path):
    _make_package(tmp_path, "thing")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.flit.module]\nname = "thing"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "thing"]


def test_package_roots_derived_from_pdm_includes(tmp_path):
    _make_package(tmp_path, "core")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.pdm.build]\nincludes = ["core"]\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "core"]


def test_package_roots_derived_from_src_layout(tmp_path):
    _make_package(tmp_path / "src", "lib")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "anything"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "lib"]


def test_package_roots_derived_from_project_name(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "app"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_empty_when_truly_underivable(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "different"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == []


def test_package_roots_malformed_poetry_packages_fails_loudly(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "x"\npackages = "notalist"\n', encoding="utf-8"
    )

    with pytest.raises(SpiceError):
        configured_package_roots(tmp_path)


def test_python_typecheck_targets_follow_package_roots(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools.packages.find]\nwhere = ["."]\ninclude = ["app*"]\n',
        encoding="utf-8",
    )

    assert python_typecheck_targets(tmp_path) == ("app",)


def test_python_typecheck_targets_empty_without_a_package(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "x"\n', encoding="utf-8"
    )

    assert python_typecheck_targets(tmp_path) == ()


def test_python_typecheck_argv_appends_fixed_flags_and_targets():
    argv = python_typecheck_argv(("app",))

    assert argv[-len(PYRIGHT_ARGS) - 1 :] == (*PYRIGHT_ARGS, "app")
    assert "pyright" in " ".join(argv)


def test_run_python_typecheck_noops_without_targets(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "x"\n', encoding="utf-8"
    )

    assert run_python_typecheck(tmp_path) is None


def _name_cluster_repo(tmp_path: Path, names: list[str]) -> Path:
    pkg = tmp_path / "app"
    pkg.mkdir()
    for name in names:
        (pkg / f"{name}.py").write_text("x = 1\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["app"]\n', encoding="utf-8"
    )
    return tmp_path


def test_name_cluster_flags_shared_prefix_run(tmp_path):
    _name_cluster_repo(tmp_path, ["teamcommands", "teamfilters", "teammetrics"])

    error = name_cluster_error(tmp_path)
    assert "name-cluster policy violation" in error
    assert "prefix 'team'" in error
    assert "namespace subpackage" in error


def test_name_cluster_flags_shared_suffix_run(tmp_path):
    _name_cluster_repo(tmp_path, ["onepayload", "twopayload", "redpayload"])

    assert "suffix 'payload'" in name_cluster_error(tmp_path)


def test_name_cluster_passes_two_siblings(tmp_path):
    _name_cluster_repo(tmp_path, ["teamcommands", "teamfilters"])

    assert name_cluster_errors(tmp_path) == []


def test_name_cluster_ignores_short_affix(tmp_path):
    _name_cluster_repo(tmp_path, ["webapp", "webcli", "webrun"])

    assert name_cluster_errors(tmp_path) == []


def test_study_shape_cli_fails_on_name_cluster(tmp_path, monkeypatch, capsys):
    _name_cluster_repo(tmp_path, ["teamcommands", "teamfilters", "teammetrics"])
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "shape"])

    assert args.func(args) == 1
    assert "name-cluster policy violation" in capsys.readouterr().out
