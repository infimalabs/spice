from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from spice.cli.mounts import MOUNTED_COMMAND_ENV, VISIBLE_PROG_ENV
from spice.errors import SpiceError
from spice.hooks import precommit
from spice.studies.localpaths import (
    render_local_path_board,
    scan_local_path_literals,
)
from spice.studies.walk import (
    partially_staged_paths,
    staged_paths,
    staged_renames,
    tracked_paths,
)
from tests.test_hookhelpers import (
    BUILTIN_PRE_COMMIT_LABELS,
    _argv_toml,
    _failure_program,
    _git,
    _git_init,
    _macos_user_path_marker,
    _patch_pre_commit_builtin_noops,
    _patch_pre_commit_builtin_noops_except_local_paths,
    _patch_pre_commit_builtin_noops_except_staging,
    _patch_pre_commit_builtin_recorders,
    _write_recorder,
    _write_repo_file,
    _write_staged_formatter,
    _write_staged_paths_recorder,
)


def test_policy_pre_commit_extensions_run_after_builtin_steps(tmp_path, monkeypatch):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.commands]\n"
        f"fmt-cs = {_argv_toml(sys.executable, str(recorder), 'fmt-cs')}\n"
        "\n"
        "[tool.spice.policy]\n"
        "pre_commit = [\n"
        '  "fmt-cs",\n'
        '  { label = "assets", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'assets')} }},\n"
        "]\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    assert precommit.handle_pre_commit(tmp_path) == 0
    assert events.read_text(encoding="utf-8").splitlines() == [
        *BUILTIN_PRE_COMMIT_LABELS,
        "fmt-cs",
        "assets",
    ]


def test_policy_pre_commit_builtin_steps_can_be_disabled_and_replaced(
    tmp_path, monkeypatch
):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.pre_commit_builtins]\n"
        "formatters = false\n"
        '"magic-numbers" = { label = "custom magic", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'custom magic')} }}\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    assert precommit.handle_pre_commit(tmp_path) == 0
    assert events.read_text(encoding="utf-8").splitlines() == [
        "repo shape",
        "staging",
        "repo docs",
        "local paths",
        "serve web typecheck",
        "env policy",
        "env name ledger",
        "file shape",
        "complexity",
        "custom magic",
        "reachability",
        "symbol reachability",
        "assertion-free tests",
        "private internals",
    ]


def test_policy_pre_commit_failure_reports_the_step_label(tmp_path, monkeypatch):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        'pre_commit = [{ label = "assets", '
        f"run = {_argv_toml(sys.executable, '-c', _failure_program())} }}]\n",
        encoding="utf-8",
    )
    _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    with pytest.raises(SpiceError) as exc_info:
        precommit.handle_pre_commit(tmp_path)

    message = str(exc_info.value)
    assert "[assets]" in message
    assert "exited 7" in message
    assert "asset failed" in message


def test_policy_pre_commit_success_extensions_run_after_gate_passes(
    tmp_path, monkeypatch
):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "pre_commit = [\n"
        '  { label = "assets", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'assets')} }},\n"
        "]\n"
        "pre_commit_success = [\n"
        '  { label = "success", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'success')} }},\n"
        "]\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    assert precommit.handle_pre_commit(tmp_path) == 0
    assert events.read_text(encoding="utf-8").splitlines() == [
        *BUILTIN_PRE_COMMIT_LABELS,
        "assets",
        "success",
    ]


def test_assertion_free_test_guard_fails_above_limit(tmp_path, monkeypatch):
    test_dir = tmp_path / "tests"
    test_dir.mkdir()
    (test_dir / "test_empty.py").write_text(
        "def test_empty():\n    value = 1\n", encoding="utf-8"
    )
    monkeypatch.setattr(precommit, "ASSERTION_FREE_TEST_LIMIT", 0)

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_assertion_free_test_guard(tmp_path)

    message = str(exc_info.value)
    assert "assertion-free-tests: 1 test(s)" in message
    assert "test_empty.py:1 test_empty" in message
    assert "ASSERTION_FREE_TEST_LIMIT=0" in message


def test_assertion_free_test_guard_allows_grandfathered_baseline(tmp_path, monkeypatch):
    test_dir = tmp_path / "tests"
    test_dir.mkdir()
    (test_dir / "test_empty.py").write_text(
        "def test_empty():\n    value = 1\n", encoding="utf-8"
    )
    monkeypatch.setattr(precommit, "ASSERTION_FREE_TEST_LIMIT", 1)

    precommit._run_assertion_free_test_guard(tmp_path)
    findings = precommit.testquality.scan_assertion_free_tests(
        precommit.testquality.test_paths(tmp_path), root=tmp_path
    )
    assert len(findings) == 1


def test_symbol_reachability_guard_fails_on_any_finding(tmp_path):
    (tmp_path / "spice" / "cli").mkdir(parents=True)
    (tmp_path / "tests").mkdir()
    (tmp_path / "spice" / "cli" / "entry.py").write_text(
        "import spice.live\n", encoding="utf-8"
    )
    (tmp_path / "spice" / "live.py").write_text(
        "def planted_dead_function_abc():\n    return 1\n", encoding="utf-8"
    )
    (tmp_path / "tests" / "test_symbol.py").write_text(
        "from spice.live import planted_dead_function_abc\n", encoding="utf-8"
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_symbol_reachability_guard(tmp_path)

    message = str(exc_info.value)
    assert "symbol-reachability: 1 test-only symbol(s)" in message
    assert "spice/live.py:planted_dead_function_abc" in message
    assert "zero test-only symbols are allowed" in message


def test_reachability_guard_fails_on_configured_module_provider_finding(tmp_path):
    provider = tmp_path / "js_provider.py"
    payload = json.dumps(
        [
            {
                "kind": "module",
                "subject": "web.dead_widget",
                "path": "web/src/dead_widget.js",
                "imported_by": ["web/test/dead_widget.test.js"],
            }
        ]
    )
    provider.write_text(f"print({payload!r})\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "reachability_providers = [\n"
        '  { name = "javascript", '
        f"run = {json.dumps([sys.executable, str(provider)])}, "
        'when = ["web/**/*.js"] },\n'
        "]\n",
        encoding="utf-8",
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_reachability_guard(tmp_path, [Path("web/src/dead_widget.js")])

    message = str(exc_info.value)
    assert "reachability: 1 test-only finding(s)" in message
    assert "provider: javascript" in message
    assert "subject: web.dead_widget" in message
    assert "zero are allowed - wire each in or delete-both" in message


def test_symbol_reachability_guard_fails_on_configured_symbol_provider_finding(
    tmp_path,
):
    provider = tmp_path / "js_provider.py"
    payload = json.dumps(
        [
            {
                "kind": "function",
                "subject": "render.unusedRender",
                "path": "web/src/render.js",
                "imported_by": ["web/test/render.test.js"],
            }
        ]
    )
    provider.write_text(f"print({payload!r})\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "reachability_providers = [\n"
        '  { name = "javascript", '
        f"run = {json.dumps([sys.executable, str(provider)])}, "
        'when = ["web/**/*.js"] },\n'
        "]\n",
        encoding="utf-8",
    )

    # A symbol-kind provider finding routes to the finer symbol-reachability gate.
    with pytest.raises(SpiceError) as exc_info:
        precommit._run_symbol_reachability_guard(tmp_path, [Path("web/src/render.js")])

    message = str(exc_info.value)
    assert "symbol-reachability: 1 test-only symbol(s)" in message
    assert "provider: javascript" in message
    assert "web/src/render.js:unusedRender" in message
    assert "zero test-only symbols are allowed" in message


def test_symbol_reachability_guard_allows_clean_repo(tmp_path):
    (tmp_path / "spice" / "cli").mkdir(parents=True)
    (tmp_path / "tests").mkdir()
    (tmp_path / "spice" / "cli" / "entry.py").write_text(
        "from spice.live import production_function\nproduction_function()\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "live.py").write_text(
        "def production_function():\n    return 1\n", encoding="utf-8"
    )

    precommit._run_symbol_reachability_guard(tmp_path)
    assert precommit.reachability.scan_symbol_reachability(tmp_path) == []


def test_policy_pre_commit_success_extensions_wait_for_clean_gate(
    tmp_path, monkeypatch
):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "pre_commit = [\n"
        '  { label = "assets", '
        f"run = {_argv_toml(sys.executable, '-c', _failure_program())} }},\n"
        "]\n"
        "pre_commit_success = [\n"
        '  { label = "success", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'success')} }},\n"
        "]\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    with pytest.raises(SpiceError) as exc_info:
        precommit.handle_pre_commit(tmp_path)

    assert "asset failed" in str(exc_info.value)
    assert events.read_text(encoding="utf-8").splitlines() == BUILTIN_PRE_COMMIT_LABELS


def test_policy_pre_commit_extensions_receive_filtered_staged_paths(
    tmp_path, monkeypatch
):
    repo = _git_init(tmp_path / "repo")
    recorder = _write_staged_paths_recorder(tmp_path)
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy]\n"
        "pre_commit = [\n"
        '  { label = "cs", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'cs')}, "
        'when = ["*.cs"] },\n'
        '  { label = "lua", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'lua')}, "
        'when = ["*.lua"] },\n'
        '  { label = "always", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'always')} }},\n"
        "]\n",
    )
    _write_repo_file(repo, "docs/readme.md", "docs\n")
    _write_repo_file(repo, "src/main.cs", "class Program {}\n")
    _git(repo, "add", ".")
    _patch_pre_commit_builtin_noops(monkeypatch)

    assert precommit.handle_pre_commit(repo) == 0

    rows = (tmp_path / "staged-paths.txt").read_text(encoding="utf-8").splitlines()
    assert rows == [
        "cs:src/main.cs",
        "always:docs/readme.md|pyproject.toml|src/main.cs",
    ]


def _write_mount_env_recorder(tmp_path):
    recorder = tmp_path / "record_mount_env.py"
    mounted_env = "SPICE_" + "MOUNTED_COMMAND"
    prog_env = "SPICE_" + "VISIBLE_PROG"
    recorder.write_text(
        "import os\n"
        "from pathlib import Path\n"
        "import sys\n"
        f"mounted = os.environ.get({mounted_env!r})\n"  # env-policy: allow
        f"prog = os.environ.get({prog_env!r})\n"  # env-policy: allow
        "with Path(sys.argv[0]).with_name('mount-env.txt').open("
        "'a', encoding='utf-8') as handle:\n"
        "    handle.write(\n"
        "        sys.argv[1] + ':mounted=' + repr(mounted)"
        " + ':prog=' + repr(prog) + '\\n'\n"
        "    )\n",
        encoding="utf-8",
    )
    return recorder


def test_mounted_pre_commit_step_carries_mount_env_but_raw_does_not(
    tmp_path, monkeypatch
):
    repo = _git_init(tmp_path / "repo")
    recorder = _write_mount_env_recorder(tmp_path)
    # Isolate from any ambient mount env (e.g. when the suite itself runs under
    # `spice release`, a mount), so the raw step genuinely starts without it.
    monkeypatch.delenv(MOUNTED_COMMAND_ENV, raising=False)
    monkeypatch.delenv(VISIBLE_PROG_ENV, raising=False)
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.commands]\n"
        f"checkit = {_argv_toml(sys.executable, str(recorder), 'mountarg')}\n"
        "\n"
        "[tool.spice.policy]\n"
        "pre_commit = [\n"
        '  "checkit",\n'
        '  { label = "raw", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'rawarg')} }},\n"
        "]\n",
    )
    _write_repo_file(repo, "docs/readme.md", "docs\n")
    _git(repo, "add", ".")
    _patch_pre_commit_builtin_noops(monkeypatch)

    assert precommit.handle_pre_commit(repo) == 0

    rows = sorted((tmp_path / "mount-env.txt").read_text(encoding="utf-8").splitlines())
    # The mounted step presents as the spice mount; the raw run step does not.
    assert rows == [
        "mountarg:mounted='1':prog='spice checkit'",
        "rawarg:mounted=None:prog=None",
    ]


def test_policy_formatter_extensions_restage_rewritten_staged_paths(
    tmp_path, monkeypatch
):
    repo = _git_init(tmp_path / "repo")
    formatter = _write_staged_formatter(tmp_path, "class Program { }\n")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy]\n"
        "pre_commit = [\n"
        '  { label = "cs formatter", '
        f"run = {_argv_toml(sys.executable, str(formatter))}, "
        'formatter = true, when = ["*.cs"] },\n'
        "]\n",
    )
    _write_repo_file(repo, "src/main.cs", "class Program{}\n")
    _git(repo, "add", ".")
    _patch_pre_commit_builtin_noops(monkeypatch)

    assert precommit.handle_pre_commit(repo) == 0

    indexed = _git(repo, "show", ":src/main.cs").stdout
    worktree = (repo / "src/main.cs").read_text(encoding="utf-8")
    assert indexed == "class Program { }\n"
    assert worktree == indexed


def test_policy_pre_commit_extensions_wait_for_staging_guard(tmp_path, monkeypatch):
    repo = _git_init(tmp_path / "repo")
    recorder = _write_staged_paths_recorder(tmp_path)
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy]\n"
        "pre_commit = [\n"
        '  { label = "cs", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'cs')}, "
        'when = ["*.cs"] },\n'
        "]\n",
    )
    _write_repo_file(repo, "src/main.cs", "class Program {}\n")
    _git(repo, "add", ".")
    _git(repo, "commit", "-m", "initial")
    _write_repo_file(repo, "src/main.cs", "class Program { int staged; }\n")
    _git(repo, "add", "src/main.cs")
    _write_repo_file(
        repo, "src/main.cs", "class Program { int staged; int unstaged; }\n"
    )
    _patch_pre_commit_builtin_noops_except_staging(monkeypatch)

    with pytest.raises(SpiceError, match="partially staged files"):
        precommit.handle_pre_commit(repo)

    assert not (tmp_path / "staged-paths.txt").exists()


def test_policy_exclude_filters_staged_and_tracked_walks(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy]\n"
        'exclude = ["Assets/Engine/Scripts/Generated/Codegen/", '
        '"toolbox/codegen/generated/*.py"]\n',
    )
    _write_repo_file(
        repo, "Assets/Engine/Scripts/Generated/Codegen/Message.cs", "generated\n"
    )
    _write_repo_file(repo, "toolbox/codegen/generated/service.py", "generated\n")
    _write_repo_file(repo, "toolbox/codegen/generated/README.md", "tracked\n")
    _write_repo_file(repo, "src/app.py", "print('kept')\n")
    _git(repo, "add", ".")

    staged = {path.as_posix() for path in staged_paths(repo)}
    tracked = {path.as_posix() for path in tracked_paths(repo)}

    assert "Assets/Engine/Scripts/Generated/Codegen/Message.cs" not in staged
    assert "Assets/Engine/Scripts/Generated/Codegen/Message.cs" not in tracked
    assert "toolbox/codegen/generated/service.py" not in staged
    assert "toolbox/codegen/generated/service.py" not in tracked
    assert "toolbox/codegen/generated/README.md" in staged
    assert "src/app.py" in staged
    assert "pyproject.toml" in staged


def test_file_shape_guard_excludes_generated_lockfiles_but_keeps_source_pressure(
    tmp_path,
):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(repo, "uv.lock", "package = []\n" * 1700)
    _write_repo_file(repo, "tool.lock", "state = []\n" * 1700)
    _write_repo_file(repo, "package-lock.json", '{"lockfileVersion": 3}\n' * 1700)
    _git(repo, "add", ".")

    precommit._run_file_loc_guard(
        repo,
        [Path("uv.lock"), Path("tool.lock"), Path("package-lock.json")],
    )

    _write_repo_file(repo, "large_source.py", "print('large')\n" * 1700)
    _git(repo, "add", "large_source.py")

    with pytest.raises(SpiceError, match="large_source.py"):
        precommit._run_file_loc_guard(repo, [Path("large_source.py")])


def test_file_shape_guard_reads_configured_bounds(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.limits]\n"
        "file_loc = 2\n"
        "file_bytes = 100000\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 1.0\n",
    )
    _write_repo_file(repo, "src/app.py", "a = 1\nb = 2\nc = 3\n")
    _git(repo, "add", ".")

    with pytest.raises(SpiceError, match="3 lines > 2"):
        precommit._run_file_loc_guard(repo, [Path("src/app.py")])


def test_complexity_guard_reads_configured_bounds(tmp_path, monkeypatch):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.limits]\n"
        "routine_ccn = 2\n"
        "routine_length = 20\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 1.0\n",
    )
    _write_repo_file(repo, "src/app.py", "def run():\n    return 1\n")
    _git(repo, "add", ".")
    record = precommit.complexity.ComplexityRecord(
        path="src/app.py",
        function_name="run",
        ccn=3,
        length=10,
        nloc=10,
    )
    monkeypatch.setattr(
        precommit.complexity,
        "collect_complexity_records",
        lambda _paths, *, root: [record],
    )

    with pytest.raises(SpiceError, match="ccn 3 > 2"):
        precommit._run_complexity_guard(repo, [Path("src/app.py")])


def test_policy_exclude_filters_renames_but_not_partially_staged_guard(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        '[tool.spice.policy]\nexclude = ["generated/"]\n',
    )
    _write_repo_file(repo, "src/old.py", "print('old')\n")
    _git(repo, "add", ".")
    _git(repo, "commit", "-m", "base")
    (repo / "generated").mkdir()

    _git(repo, "mv", "src/old.py", "generated/old.py")
    _write_repo_file(repo, "generated/partial.py", "staged\n")
    _git(repo, "add", "generated/partial.py")
    _write_repo_file(repo, "generated/partial.py", "unstaged\n")

    assert staged_renames(repo) == {}
    assert partially_staged_paths(repo) == [Path("generated/partial.py")]


def test_policy_exclude_filters_path_based_builtin_gate_steps(tmp_path, monkeypatch):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        '[tool.spice.policy]\nexclude = ["generated/"]\n',
    )
    _write_repo_file(repo, "generated/service.py", "print('generated')\n")
    _write_repo_file(repo, "src/app.py", "print('kept')\n")
    _git(repo, "add", ".")
    seen: dict[str, list[str]] = {}

    def record(label: str):
        def inner(repo_root: Path, paths: list[Path]) -> None:
            seen[label] = [path.as_posix() for path in paths]

        return inner

    monkeypatch.setattr(precommit, "_run_shape_guards", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_staging_guard", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_repo_truth_doc_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_python_format_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_serve_web_typecheck_guard", lambda repo_root: None
    )
    monkeypatch.setattr(precommit, "_run_local_path_guard", record("local paths"))
    monkeypatch.setattr(precommit, "_run_env_policy_guard", record("env policy"))
    monkeypatch.setattr(precommit, "_run_env_name_ledger_guard", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_file_loc_guard", record("file shape"))
    monkeypatch.setattr(precommit, "_run_complexity_guard", record("complexity"))
    monkeypatch.setattr(precommit, "_run_magic_numbers_guard", record("magic numbers"))
    monkeypatch.setattr(
        precommit, "_run_reachability_guard", lambda repo_root, paths=None: None
    )
    monkeypatch.setattr(
        precommit, "_run_symbol_reachability_guard", lambda repo_root, paths=None: None
    )
    monkeypatch.setattr(
        precommit, "_run_assertion_free_test_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "clear_successful_sticky_state", lambda repo_root: None
    )

    assert precommit.handle_pre_commit(repo) == 0
    for paths in seen.values():
        assert "generated/service.py" not in paths
        assert "src/app.py" in paths
        assert "pyproject.toml" in paths


def test_local_path_policy_flags_absolute_macos_user_path_marker(tmp_path):
    marker = _macos_user_path_marker()
    path = tmp_path / "sample.py"
    path.write_text(f'ROOT = "{marker}engineer/project"\n', encoding="utf-8")

    findings = scan_local_path_literals([Path("sample.py")], root=tmp_path)

    assert [(finding.path, finding.line) for finding in findings] == [("sample.py", 1)]
    board = render_local_path_board(findings)
    assert "local-paths: 1 absolute macOS user path literal(s)" in board
    assert "sample.py:1" in board


def test_default_pre_commit_gate_reports_local_path_literals(tmp_path, monkeypatch):
    marker = _macos_user_path_marker()
    path = tmp_path / "sample.md"
    path.write_text(f"See {marker}engineer/project/notes.md\n", encoding="utf-8")
    _patch_pre_commit_builtin_noops_except_local_paths(tmp_path, monkeypatch)

    with pytest.raises(SpiceError) as exc_info:
        precommit.handle_pre_commit(tmp_path)

    message = str(exc_info.value)
    assert "[local paths]" in message
    assert "sample.md:1" in message


def test_local_path_policy_tests_do_not_spell_forbidden_marker_literal():
    marker = _macos_user_path_marker()
    assert marker not in Path(__file__).read_text(encoding="utf-8")
