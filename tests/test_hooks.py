"""Pre-commit gate pieces: repo-truth doc caps and their configuration."""

import os
import stat
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.errors import SpiceError
from spice.hooks import commitmsg, precommit
from spice.hooks.install import hooks_dir, init_repo, install_hooks_for_repo
from spice.hooks.precommit import _run_repo_truth_doc_guard, repo_truth_docs
from spice.policy import REPO_TRUTH_DOC_LIMIT, REPO_TRUTH_DOCS
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

PROJECT_ROOT = Path(__file__).resolve().parents[1]

BUILTIN_PRE_COMMIT_LABELS = [
    "repo shape",
    "staging",
    "repo docs",
    "formatters",
    "local paths",
    "serve web typecheck",
    "env policy",
    "file shape",
    "complexity",
    "magic numbers",
    "reachability",
    "symbol reachability",
    "assertion-free tests",
    "private internals",
]


def test_default_repo_truth_docs_apply_without_configuration(tmp_path):
    assert repo_truth_docs(tmp_path) == list(REPO_TRUTH_DOCS)


def test_declared_repo_truth_docs_override_the_default(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nrepo_truth_docs = ["AGENTS.md", "TESTING.md"]\n',
        encoding="utf-8",
    )
    assert repo_truth_docs(tmp_path) == ["AGENTS.md", "TESTING.md"]


def test_doc_within_cap_passes(tmp_path):
    (tmp_path / "AGENTS.md").write_text("short doctrine\n", encoding="utf-8")
    _run_repo_truth_doc_guard(tmp_path)
    assert (tmp_path / "AGENTS.md").read_text(encoding="utf-8") == "short doctrine\n"


def test_doc_over_cap_fails_loudly(tmp_path):
    (tmp_path / "AGENTS.md").write_text(
        "x" * (REPO_TRUTH_DOC_LIMIT + 1), encoding="utf-8"
    )
    with pytest.raises(SpiceError, match="character cap"):
        _run_repo_truth_doc_guard(tmp_path)


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


def test_private_internal_gate_fails_above_limit(tmp_path, monkeypatch):
    test_dir = tmp_path / "tests"
    package_dir = tmp_path / "spice" / "worker"
    test_dir.mkdir()
    package_dir.mkdir(parents=True)
    (package_dir / "public.py").write_text("_VALUE = 1\n", encoding="utf-8")
    (test_dir / "test_private.py").write_text(
        "from spice.worker.public import _VALUE\n"
        "\n"
        "def test_private_import():\n"
        "    assert _VALUE == 1\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(precommit, "PRIVATE_INTERNAL_COUPLING_LIMIT", 0)
    _patch_pre_commit_builtin_noops_except_private_internals(monkeypatch)

    with pytest.raises(SpiceError) as exc_info:
        precommit.handle_pre_commit(tmp_path)

    message = str(exc_info.value)
    assert "[private internals]" in message
    assert "private-internals: 1 coupling(s)" in message
    assert "private import spice.worker.public._VALUE" in message
    assert "PRIVATE_INTERNAL_COUPLING_LIMIT=0" in message


def test_private_internal_gate_allows_grandfathered_baseline(tmp_path, monkeypatch):
    test_dir = tmp_path / "tests"
    package_dir = tmp_path / "spice" / "worker"
    test_dir.mkdir()
    package_dir.mkdir(parents=True)
    (package_dir / "public.py").write_text("_VALUE = 1\n", encoding="utf-8")
    (test_dir / "test_private.py").write_text(
        "from spice.worker.public import _VALUE\n"
        "\n"
        "def test_private_import():\n"
        "    assert _VALUE == 1\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(precommit, "PRIVATE_INTERNAL_COUPLING_LIMIT", 1)
    _patch_pre_commit_builtin_noops_except_private_internals(monkeypatch)

    assert precommit.handle_pre_commit(tmp_path) == 0
    findings = precommit.testquality.scan_private_internal_coupling(
        precommit.testquality.test_paths(tmp_path), root=tmp_path
    )
    assert len(findings) == 1


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
    monkeypatch.setattr(precommit, "_run_file_loc_guard", record("file shape"))
    monkeypatch.setattr(precommit, "_run_complexity_guard", record("complexity"))
    monkeypatch.setattr(precommit, "_run_magic_numbers_guard", record("magic numbers"))
    monkeypatch.setattr(precommit, "_run_reachability_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_symbol_reachability_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_assertion_free_test_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_private_internal_coupling_guard", lambda repo_root: None
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


def test_install_hooks_writes_reference_transaction_shim(tmp_path):
    repo = _git_init(tmp_path / "repo")

    rows = install_hooks_for_repo(repo)

    path = hooks_dir(repo) / "reference-transaction"
    assert f"hook reference-transaction -> {path.relative_to(repo).as_posix()}" in rows
    assert path.read_text(encoding="utf-8") == _expected_hook_content(
        'dev reference-transaction "$1"'
    )
    assert path.stat().st_mode & stat.S_IXUSR
    assert (
        _git(repo, "config", "--get", "core.hooksPath").stdout.strip() == ".spice/hooks"
    )


def test_install_hooks_writes_pre_commit_hook_backend_shim(tmp_path):
    repo = _git_init(tmp_path / "repo")

    install_hooks_for_repo(repo)

    content = (hooks_dir(repo) / "pre-commit").read_text(encoding="utf-8")
    assert content == _expected_hook_content("dev pre-commit")


def test_init_repo_reports_generated_worktree_skill_ignore(tmp_path):
    from spice.agent.lifecycle import WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH

    repo = _git_init(tmp_path / "repo")

    rows = init_repo(repo)

    assert f"skill_ignore={WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH.as_posix()}" in rows
    assert (
        (repo / WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH)
        .read_text(encoding="utf-8")
        .startswith("# Autogenerated by spice; do not edit.\n")
    )
    assert _git(repo, "status", "--short").stdout == ""


def test_init_repo_generates_state_gitignore(tmp_path):
    from spice.hooks.install import STATE_GITIGNORE_CONTENT

    repo = _git_init(tmp_path / "repo")

    rows = init_repo(repo)

    assert "state_ignore=.spice/.gitignore" in rows
    assert (repo / ".spice" / ".gitignore").read_text(
        encoding="utf-8"
    ) == STATE_GITIGNORE_CONTENT
    # `.spice/` is excluded by its own generated marker, so init leaves a clean tree.
    assert _git(repo, "status", "--short").stdout == ""


def test_init_repo_keeps_bare_common_linked_worktree_non_bare(tmp_path):
    seed = _git_init(tmp_path / "seed")
    _commit(seed, "README.md", "seed\n", "seed")
    source = tmp_path / "source.git"
    _run(["git", "clone", "--bare", str(seed), str(source)])
    lane = tmp_path / "lane"
    _git(source, "worktree", "add", str(lane), "main")

    rows = init_repo(lane)
    status = _git(lane, "status", "--short")

    assert "core.hooksPath=.spice/hooks" in rows
    assert _git(lane, "config", "--worktree", "--get", "core.bare").stdout.strip() == (
        "false"
    )
    assert _git(lane, "rev-parse", "--is-bare-repository").stdout.strip() == "false"
    assert _git(lane, "rev-parse", "--is-inside-work-tree").stdout.strip() == "true"
    assert status.returncode == 0


def test_init_cli_bootstraps_bare_common_linked_worktree_before_repo_detection(
    tmp_path,
):
    seed = _git_init(tmp_path / "seed")
    _commit(seed, "README.md", "seed\n", "seed")
    source = tmp_path / "source.git"
    _run(["git", "clone", "--bare", str(seed), str(source)])
    lane = tmp_path / "lane"
    _git(source, "worktree", "add", str(lane), "main")

    result = _run([sys.executable, "-m", "spice", "init"], cwd=lane)

    assert result.returncode == 0
    assert "core.hooksPath=.spice/hooks" in result.stdout
    assert _git(lane, "config", "--worktree", "--get", "core.bare").stdout.strip() == (
        "false"
    )
    assert _git(lane, "rev-parse", "--show-toplevel").stdout.strip() == str(lane)
    assert _git(lane, "rev-parse", "--is-inside-work-tree").stdout.strip() == "true"


def test_install_hooks_writes_ambient_spice_shims_for_spice_checkout(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_spice_product_shape(repo)

    install_hooks_for_repo(repo)

    assert (hooks_dir(repo) / "reference-transaction").read_text(
        encoding="utf-8"
    ) == _expected_hook_content('dev reference-transaction "$1"')
    assert (hooks_dir(repo) / "commit-msg").read_text(
        encoding="utf-8"
    ) == _expected_hook_content('dev commit-msg "$1"')


def _expected_hook_content(args: str) -> str:
    return (
        "\n".join(["#!/usr/bin/env sh", "", "set -eu", "", f"exec spice {args}"]) + "\n"
    )


def test_reference_transaction_blocks_upstream_merged_current_branch_rewind(tmp_path):
    repo, base, protected = _repo_with_pushed_tip(tmp_path)
    install_hooks_for_repo(repo)

    result = _git(
        repo,
        "update-ref",
        "refs/heads/main",
        base,
        protected,
        check=False,
    )

    assert result.returncode != 0
    assert (
        "reference-transaction guard refused to abandon upstream-merged commits "
        "on current branch refs/heads/main"
    ) in result.stderr
    assert "task boundary has advanced origin/upstream" in result.stderr
    assert "continue with an append-only commit" in result.stderr
    assert _git(repo, "rev-parse", "HEAD").stdout.strip() == protected


def test_reference_transaction_allows_unmerged_current_branch_rewind(tmp_path):
    repo, _, protected = _repo_with_pushed_tip(tmp_path)
    local = _commit(repo, "story.txt", "base\nprotected\nlocal\n", "local work")
    install_hooks_for_repo(repo)

    result = _git(
        repo,
        "update-ref",
        "refs/heads/main",
        protected,
        local,
        check=False,
    )

    assert result.returncode == 0
    assert _git(repo, "rev-parse", "refs/heads/main").stdout.strip() == protected


def test_dev_serve_web_typecheck_parser_exposes_command():
    from spice.cli.parser import build_parser

    args = build_parser().parse_args(["dev", "serve-web-typecheck"])

    assert args.dev_command == "serve-web-typecheck"


def test_dev_pre_commit_parser_exposes_hook_backend_command():
    from spice.cli.parser import build_parser

    args = build_parser().parse_args(["dev", "pre-commit"])

    assert args.dev_command == "pre-commit"
    assert args.pre_commit_args == []


def test_dev_pre_commit_parser_captures_appended_wrapper_arguments():
    from spice.cli.parser import build_parser

    args = build_parser().parse_args(["dev", "pre-commit", "run", "--all-files"])

    assert args.dev_command == "pre-commit"
    assert args.pre_commit_args == ["run", "--all-files"]


def test_dev_pre_commit_runs_gate(tmp_path, monkeypatch):
    from spice.hooks import cli as hooks_cli

    calls: list[Path] = []
    monkeypatch.setattr(hooks_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        precommit,
        "handle_pre_commit",
        lambda repo_root: calls.append(repo_root) or 0,
    )

    result = hooks_cli.handle_dev(SimpleNamespace(dev_command="pre-commit"))

    assert result == 0
    assert calls == [tmp_path]


def test_dev_pre_commit_reports_repo_gate_replacement_for_upstream_args(
    tmp_path, monkeypatch
):
    from spice.hooks import cli as hooks_cli

    monkeypatch.setattr(hooks_cli, "require_repo_root", lambda: tmp_path)

    with pytest.raises(SpiceError) as exc_info:
        hooks_cli.handle_dev(
            SimpleNamespace(
                dev_command="pre-commit",
                pre_commit_args=["run", "--all-files"],
            )
        )

    message = str(exc_info.value)
    assert "does not accept pre-commit framework arguments: run --all-files" in message
    assert "Run `spice dev pre-commit` for the staged gate" in message


def test_commit_msg_rejects_co_authored_by_trailer(tmp_path):
    message = (
        "Block delegated commit authorship\n"
        "\n"
        "The harness owns the visible commit author contract.\n"
        "\n"
        "Co-Authored-By: Agent <agent@example.test>\n"
    )
    path = tmp_path / "COMMIT_EDITMSG"
    path.write_text(message, encoding="utf-8")

    with pytest.raises(SpiceError) as exc_info:
        commitmsg.handle_commit_msg(str(path))

    error = str(exc_info.value)
    assert "forbidden trailer Co-Authored-By" in error
    assert "commit messages must not add co-authors" in error


def test_serve_web_typecheck_skips_repo_without_sources(tmp_path, monkeypatch):
    from spice.serve import typecheck

    monkeypatch.setattr(typecheck, "find_tool", lambda name: None)

    assert typecheck.run_serve_web_typecheck(tmp_path) is None


def test_serve_web_typecheck_invokes_typescript_checkjs(tmp_path, monkeypatch):
    from spice.serve import typecheck

    for relative in typecheck.SERVE_WEB_JS_PATHS:
        path = tmp_path / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("// serve static source\n", encoding="utf-8")

    calls = []
    monkeypatch.setattr(typecheck, "find_tool", lambda name: "/usr/bin/npm")

    def fake_run(argv, **kwargs):
        calls.append((argv, kwargs))
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(typecheck.subprocess, "run", fake_run)

    typecheck.run_serve_web_typecheck(tmp_path)

    assert len(calls) == 1
    argv, kwargs = calls[0]
    assert argv[:6] == [
        "/usr/bin/npm",
        "exec",
        "--yes",
        "--package",
        "typescript",
        "tsc",
    ]
    assert "--checkJs" in argv
    assert "--noEmit" in argv
    assert "spice/serve/static/app.types.js" in argv
    assert "spice/serve/static/app.menu.js" in argv
    assert "spice/serve/static/app.js" in argv
    assert kwargs["cwd"] == tmp_path


def _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch):
    events = tmp_path / "events.txt"

    def record(label: str) -> None:
        with events.open("a", encoding="utf-8") as handle:
            handle.write(label + "\n")

    monkeypatch.setattr(precommit, "staged_paths", lambda repo_root: [])
    monkeypatch.setattr(
        precommit, "clear_successful_sticky_state", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_shape_guards", lambda repo_root: record("repo shape")
    )
    monkeypatch.setattr(
        precommit, "_run_staging_guard", lambda repo_root: record("staging")
    )
    monkeypatch.setattr(
        precommit, "_run_repo_truth_doc_guard", lambda repo_root: record("repo docs")
    )
    monkeypatch.setattr(
        precommit,
        "_run_python_format_guard",
        lambda repo_root, paths: record("formatters"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_serve_web_typecheck_guard",
        lambda repo_root: record("serve web typecheck"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_local_path_guard",
        lambda repo_root, paths: record("local paths"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_env_policy_guard",
        lambda repo_root, paths: record("env policy"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_file_loc_guard",
        lambda repo_root, paths: record("file shape"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_complexity_guard",
        lambda repo_root, paths: record("complexity"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_magic_numbers_guard",
        lambda repo_root, paths: record("magic numbers"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_reachability_guard",
        lambda repo_root: record("reachability"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_symbol_reachability_guard",
        lambda repo_root: record("symbol reachability"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_assertion_free_test_guard",
        lambda repo_root: record("assertion-free tests"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_private_internal_coupling_guard",
        lambda repo_root: record("private internals"),
    )
    return events


def _patch_pre_commit_builtin_noops_except_local_paths(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr(
        precommit, "staged_paths", lambda repo_root: [Path("sample.md")]
    )
    monkeypatch.setattr(
        precommit, "clear_successful_sticky_state", lambda repo_root: None
    )
    monkeypatch.setattr(precommit, "_run_shape_guards", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_staging_guard", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_repo_truth_doc_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_python_format_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_serve_web_typecheck_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_env_policy_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_file_loc_guard", lambda repo_root, paths: None)
    monkeypatch.setattr(
        precommit, "_run_complexity_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_magic_numbers_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_reachability_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_symbol_reachability_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_assertion_free_test_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_private_internal_coupling_guard", lambda repo_root: None
    )


def _patch_pre_commit_builtin_noops_except_private_internals(monkeypatch) -> None:
    monkeypatch.setattr(precommit, "staged_paths", lambda repo_root: [])
    monkeypatch.setattr(
        precommit, "clear_successful_sticky_state", lambda repo_root: None
    )
    monkeypatch.setattr(precommit, "_run_shape_guards", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_staging_guard", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_repo_truth_doc_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_python_format_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_serve_web_typecheck_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_local_path_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_env_policy_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_file_loc_guard", lambda repo_root, paths: None)
    monkeypatch.setattr(
        precommit, "_run_complexity_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_magic_numbers_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_reachability_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_assertion_free_test_guard", lambda repo_root: None
    )


def _patch_pre_commit_builtin_noops_except_staging(monkeypatch) -> None:
    monkeypatch.setattr(
        precommit, "clear_successful_sticky_state", lambda repo_root: None
    )
    monkeypatch.setattr(precommit, "_run_shape_guards", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_repo_truth_doc_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_python_format_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_serve_web_typecheck_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_local_path_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_env_policy_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_file_loc_guard", lambda repo_root, paths: None)
    monkeypatch.setattr(
        precommit, "_run_complexity_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_magic_numbers_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_reachability_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_symbol_reachability_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_assertion_free_test_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_private_internal_coupling_guard", lambda repo_root: None
    )


def _patch_pre_commit_builtin_noops(monkeypatch) -> None:
    _patch_pre_commit_builtin_noops_except_staging(monkeypatch)
    monkeypatch.setattr(precommit, "_run_staging_guard", lambda repo_root: None)


def _macos_user_path_marker() -> str:
    return "/" + "Users" + "/"


def _write_recorder(tmp_path):
    recorder = tmp_path / "record_step.py"
    recorder.write_text(
        "from pathlib import Path\n"
        "import sys\n"
        "with Path('events.txt').open('a', encoding='utf-8') as handle:\n"
        "    handle.write(sys.argv[1] + '\\n')\n",
        encoding="utf-8",
    )
    return recorder


def _argv_toml(*argv: str) -> str:
    return "[" + ", ".join(_toml_string(item) for item in argv) + "]"


def _toml_string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _failure_program() -> str:
    return "import sys; print('asset failed'); sys.exit(7)"


def _write_staged_paths_recorder(tmp_path):
    recorder = tmp_path / "record_staged_paths.py"
    staged_paths_env = "SPICE_" + "STAGED_PATHS"
    recorder.write_text(
        "import os\n"
        "from pathlib import Path\n"
        "import sys\n"
        f"paths = os.environ[{staged_paths_env!r}].splitlines()\n"
        "with Path(sys.argv[0]).with_name('staged-paths.txt').open("
        "'a', encoding='utf-8') as handle:\n"
        "    handle.write(sys.argv[1] + ':' + '|'.join(paths) + '\\n')\n",
        encoding="utf-8",
    )
    return recorder


def _write_staged_formatter(tmp_path, replacement: str):
    formatter = tmp_path / "format_staged.py"
    staged_paths_env = "SPICE_" + "STAGED_PATHS"
    formatter.write_text(
        "import os\n"
        "from pathlib import Path\n"
        f"for raw in os.environ[{staged_paths_env!r}].splitlines():\n"
        f"    Path(raw).write_text({replacement!r}, encoding='utf-8')\n",
        encoding="utf-8",
    )
    return formatter


def _repo_with_pushed_tip(tmp_path: Path) -> tuple[Path, str, str]:
    remote = tmp_path / "origin.git"
    _run(["git", "init", "--bare", str(remote)])
    repo = _git_init(tmp_path / "repo")
    _git(repo, "remote", "add", "origin", str(remote))
    base = _commit(repo, "story.txt", "base\n", "base")
    _git(repo, "push", "-u", "origin", "main")
    protected = _commit(repo, "story.txt", "base\nprotected\n", "protected")
    _git(repo, "push", "origin", "main")
    _git(repo, "fetch", "origin", "main")
    return repo, base, protected


def _git_init(repo: Path) -> Path:
    _run(["git", "init", "-b", "main", str(repo)])
    _git(repo, "config", "user.email", "spice@example.test")
    _git(repo, "config", "user.name", "Spice Tests")
    return repo


def _commit(repo: Path, name: str, text: str, message: str) -> str:
    path = repo / name
    path.write_text(text, encoding="utf-8")
    _git(repo, "add", name)
    _git(repo, "commit", "-m", message)
    return _git(repo, "rev-parse", "HEAD").stdout.strip()


def _write_repo_file(repo: Path, name: str, text: str) -> None:
    path = repo / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _write_spice_product_shape(repo: Path) -> None:
    for relative in (
        Path("spice") / "__main__.py",
        Path("spice") / "cli" / "entry.py",
        Path("spice") / "agent" / "wrap.py",
    ):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")


def _git(
    repo: Path, *args: str, check: bool = True
) -> subprocess.CompletedProcess[str]:
    return _run(["git", "-C", str(repo), *args], check=check)


def _run(
    args: list[str], *, check: bool = True, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["PYTHONPATH"] = os.pathsep.join(
        entry for entry in (str(PROJECT_ROOT), env.get("PYTHONPATH", "")) if entry
    )
    result = subprocess.run(
        args,
        capture_output=True,
        check=False,
        cwd=cwd,
        env=env,
        text=True,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            f"command failed: {' '.join(args)}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result
