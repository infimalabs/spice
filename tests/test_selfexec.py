"""`spice dev ...` re-exec: gate backends must run the worktree's own checkout."""

from pathlib import Path
from types import SimpleNamespace

from spice.cli import entry as cli_entry


def _make_worktree_checkout(tmp_path: Path, *, with_venv: bool = True) -> Path:
    repo = tmp_path / "repo"
    (repo / "spice" / "cli").mkdir(parents=True)
    (repo / "spice" / "cli" / "entry.py").write_text("", encoding="utf-8")
    if with_venv:
        venv_bin = repo / ".venv" / "bin"
        venv_bin.mkdir(parents=True)
        (venv_bin / "python").write_text("#!/usr/bin/env sh\n", encoding="utf-8")
    return repo


def test_worktree_local_python_none_without_spice_package(tmp_path):
    # spice/ is a namespace package (no __init__.py); a target repo that
    # merely has a .venv but isn't a spice checkout must not match.
    repo = tmp_path / "target-repo"
    (repo / ".venv" / "bin").mkdir(parents=True)
    (repo / ".venv" / "bin" / "python").write_text("", encoding="utf-8")
    assert cli_entry._worktree_local_python(repo) is None


def test_worktree_local_python_none_without_venv(tmp_path):
    repo = _make_worktree_checkout(tmp_path, with_venv=False)
    assert cli_entry._worktree_local_python(repo) is None


def test_worktree_local_python_found_for_a_spice_checkout_with_venv(tmp_path):
    repo = _make_worktree_checkout(tmp_path)
    python = cli_entry._worktree_local_python(repo)
    assert python == repo / ".venv" / "bin" / "python"


def test_reexec_noop_when_cwd_is_not_a_spice_checkout(tmp_path, monkeypatch):
    repo = tmp_path / "target-repo"
    repo.mkdir()
    monkeypatch.setattr(cli_entry, "repo_root_from_cwd", lambda: repo)

    def fail_run(*args, **kwargs):
        raise AssertionError("must not subprocess.run when there is nothing local")

    monkeypatch.setattr(cli_entry.subprocess, "run", fail_run)
    assert (
        cli_entry._reexec_dev_command_for_worktree_checkout(["dev", "doctor"]) is None
    )


def test_reexec_noop_when_already_running_from_the_local_venv(tmp_path, monkeypatch):
    repo = _make_worktree_checkout(tmp_path)
    monkeypatch.setattr(cli_entry, "repo_root_from_cwd", lambda: repo)
    monkeypatch.setattr(cli_entry.sys, "prefix", str(repo / ".venv"))

    def fail_run(*args, **kwargs):
        raise AssertionError("must not relaunch when already the local interpreter")

    monkeypatch.setattr(cli_entry.subprocess, "run", fail_run)
    assert (
        cli_entry._reexec_dev_command_for_worktree_checkout(["dev", "pre-commit"])
        is None
    )


def test_reexec_fires_even_when_both_venvs_share_one_symlinked_interpreter(
    tmp_path, monkeypatch
):
    # Regression: a venv's `python` is commonly a symlink to one shared system
    # interpreter, so comparing *resolved interpreter binaries* would wrongly
    # call two different worktrees' venvs "the same" -- the decision must key
    # on the venv root (sys.prefix), which does not collapse that way.
    shared_interpreter = tmp_path / "shared-python"
    shared_interpreter.write_text("", encoding="utf-8")
    repo = _make_worktree_checkout(tmp_path, with_venv=False)
    venv_bin = repo / ".venv" / "bin"
    venv_bin.mkdir(parents=True)
    (venv_bin / "python").symlink_to(shared_interpreter)
    other_venv = tmp_path / "other-worktree" / ".venv" / "bin"
    other_venv.mkdir(parents=True)
    (other_venv / "python").symlink_to(shared_interpreter)
    # `spice dev pytest` itself arrives through the re-exec seam, so the
    # sentinel may already be in this process's environment; the gate under
    # test needs it absent.
    monkeypatch.delenv(cli_entry.SELFEXEC_ENV, raising=False)
    monkeypatch.setattr(cli_entry, "repo_root_from_cwd", lambda: repo)
    monkeypatch.setattr(cli_entry.sys, "prefix", str(other_venv.parent))
    captured: dict[str, object] = {}

    def fake_run(argv, **kwargs):
        captured["argv"] = list(argv)
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(cli_entry.subprocess, "run", fake_run)
    code = cli_entry._reexec_dev_command_for_worktree_checkout(["dev", "doctor"])
    assert code == 0
    assert captured["argv"][0] == str(repo / ".venv" / "bin" / "python")


def test_reexec_relaunches_through_the_worktree_venv(tmp_path, monkeypatch):
    repo = _make_worktree_checkout(tmp_path)
    monkeypatch.delenv(cli_entry.SELFEXEC_ENV, raising=False)
    monkeypatch.setattr(cli_entry, "repo_root_from_cwd", lambda: repo)
    monkeypatch.setattr(cli_entry.sys, "prefix", str(tmp_path / "some-other-venv"))
    captured: dict[str, object] = {}

    def fake_run(argv, *, cwd, env, check):
        captured["argv"] = list(argv)
        captured["cwd"] = cwd
        captured["env"] = env
        captured["check"] = check
        return SimpleNamespace(returncode=7)

    monkeypatch.setattr(cli_entry.subprocess, "run", fake_run)
    code = cli_entry._reexec_dev_command_for_worktree_checkout(
        ["dev", "serve-web-typecheck"]
    )
    assert code == 7
    assert captured["argv"] == [
        str(repo / ".venv" / "bin" / "python"),
        "-m",
        "spice",
        "dev",
        "serve-web-typecheck",
    ]
    assert captured["cwd"] == repo
    assert captured["check"] is False
    assert captured["env"][cli_entry.SELFEXEC_ENV] == str(repo)


def test_reexec_sentinel_prevents_a_relaunch_loop(tmp_path, monkeypatch):
    repo = _make_worktree_checkout(tmp_path)
    monkeypatch.setenv(cli_entry.SELFEXEC_ENV, str(repo))
    monkeypatch.setattr(cli_entry, "repo_root_from_cwd", lambda: repo)

    def fail_run(*args, **kwargs):
        raise AssertionError("must not relaunch once the sentinel is set")

    monkeypatch.setattr(cli_entry.subprocess, "run", fail_run)
    assert (
        cli_entry._reexec_dev_command_for_worktree_checkout(["dev", "doctor"]) is None
    )


def test_main_only_checks_reexec_for_dev_commands(monkeypatch):
    def fail_reexec(argv):
        raise AssertionError("must not consider reexec off the dev namespace")

    monkeypatch.setattr(
        cli_entry, "_reexec_dev_command_for_worktree_checkout", fail_reexec
    )
    monkeypatch.setattr(cli_entry, "_dispatch", lambda argv: 0)
    assert cli_entry.main(["agent", "status"]) == 0
