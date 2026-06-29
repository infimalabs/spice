from __future__ import annotations

import os
import subprocess
from pathlib import Path

from spice.hooks import precommit

PROJECT_ROOT = Path(__file__).resolve().parents[1]

BUILTIN_PRE_COMMIT_LABELS = [
    "repo shape",
    "staging",
    "repo docs",
    "formatters",
    "local paths",
    "serve web typecheck",
    "env policy",
    "env name ledger",
    "file shape",
    "complexity",
    "magic numbers",
    "reachability",
    "symbol reachability",
    "assertion-free tests",
    "private internals",
]


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
        "_run_env_name_ledger_guard",
        lambda repo_root: record("env name ledger"),
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
        lambda repo_root, paths=None: record("reachability"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_symbol_reachability_guard",
        lambda repo_root, paths=None: record("symbol reachability"),
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
    monkeypatch.setattr(precommit, "_run_env_name_ledger_guard", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_file_loc_guard", lambda repo_root, paths: None)
    monkeypatch.setattr(
        precommit, "_run_complexity_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_magic_numbers_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_reachability_guard", lambda repo_root, paths=None: None
    )
    monkeypatch.setattr(
        precommit, "_run_symbol_reachability_guard", lambda repo_root, paths=None: None
    )
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
    monkeypatch.setattr(precommit, "_run_env_name_ledger_guard", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_file_loc_guard", lambda repo_root, paths: None)
    monkeypatch.setattr(
        precommit, "_run_complexity_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_magic_numbers_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_reachability_guard", lambda repo_root, paths=None: None
    )
    monkeypatch.setattr(
        precommit, "_run_symbol_reachability_guard", lambda repo_root, paths=None: None
    )
    monkeypatch.setattr(
        precommit, "_run_assertion_free_test_guard", lambda repo_root: None
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
        f"paths = os.environ[{staged_paths_env!r}].splitlines()\n"  # env-policy: allow
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
        f"for raw in os.environ[{staged_paths_env!r}].splitlines():\n"  # env-policy: allow
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
    env = os.environ.copy()  # env-policy: allow
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
