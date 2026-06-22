"""Mounted commands: validation, precedence, dotted-path dispatch."""

from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.cli import entry as cli_entry
from spice.cli.mounts import (
    MOUNT_SEGMENT_RE,
    MOUNTED_COMMAND_ENV,
    MountedCommand,
    VISIBLE_PROG_ENV,
    find_mounted_command,
    mounted_commands,
    run_mounted_command,
)
from spice.cli.parser import BUILTIN_COMMANDS, build_parser
from spice.errors import SpiceError


def _repo_with_commands(tmp_path, body: str):
    (tmp_path / ".git").mkdir()
    (tmp_path / "pyproject.toml").write_text(
        f"[tool.spice.commands]\n{body}\n", encoding="utf-8"
    )
    return tmp_path


def test_builtin_commands_match_the_live_parser():
    choices = build_parser()._subparsers._group_actions[0].choices
    assert tuple(choices) == BUILTIN_COMMANDS


def test_string_mounts_shlex_split(tmp_path):
    repo = _repo_with_commands(tmp_path, 'probe = "python -m myproj.probe --fast"')
    assert mounted_commands(repo) == {
        ("probe",): ("python", "-m", "myproj.probe", "--fast")
    }


def test_list_mounts_pass_argv_verbatim(tmp_path):
    repo = _repo_with_commands(
        tmp_path, 'release.notes = ["python", "-m", "spice.release", "notes"]'
    )
    assert mounted_commands(repo) == {
        ("release", "notes"): ("python", "-m", "spice.release", "notes")
    }


def test_dotted_mount_names_require_valid_segments(tmp_path):
    assert MOUNT_SEGMENT_RE.fullmatch("lane-tools")
    repo = _repo_with_commands(tmp_path, '"analyze.Bad_Name" = "./run.sh"')
    with pytest.raises(SpiceError, match="dot-separated segments"):
        mounted_commands(repo)


def test_top_level_mount_shadowing_builtin_fails_loudly(tmp_path):
    repo = _repo_with_commands(tmp_path, 'task = "./scripts/task.sh"')
    with pytest.raises(SpiceError, match="shadows a built-in"):
        mounted_commands(repo)


def test_builtin_nested_mounts_are_allowed(tmp_path):
    repo = _repo_with_commands(tmp_path, 'report.inspect = ["project-tool", "inspect"]')
    assert mounted_commands(repo) == {
        ("report", "inspect"): ("project-tool", "inspect")
    }


def test_empty_mount_fails_loudly(tmp_path):
    repo = _repo_with_commands(tmp_path, 'noop = ""')
    with pytest.raises(SpiceError, match="empty"):
        mounted_commands(repo)


def test_find_mounted_command_uses_longest_matching_path(tmp_path, monkeypatch):
    _repo_with_commands(
        tmp_path,
        'probe = ["tool", "probe"]\nreport.inspect = ["tool", "report", "inspect"]\n',
    )
    monkeypatch.setattr("spice.cli.mounts.repo_root_from_cwd", lambda: tmp_path)
    resolved = find_mounted_command(["report", "inspect", "--limit", "20"])
    assert resolved is not None
    mount, remainder = resolved
    assert mount.path == ("report", "inspect")
    assert mount.argv == ("tool", "report", "inspect")
    assert remainder == ["--limit", "20"]


def test_run_mounted_command_exports_visible_spice_identity(
    monkeypatch: pytest.MonkeyPatch, tmp_path
):
    captured: dict[str, object] = {}
    (tmp_path / ".venv" / "bin").mkdir(parents=True)
    monkeypatch.setenv("PATH", "")
    monkeypatch.setenv("VIRTUAL_ENV", "/tmp/foreign-venv")

    def fake_run(argv, *, cwd, env, check):
        captured["argv"] = tuple(argv)
        captured["cwd"] = cwd
        captured["env"] = {
            "VIRTUAL_ENV": env.get("VIRTUAL_ENV"),
            "PATH": env.get("PATH"),
            MOUNTED_COMMAND_ENV: env.get(MOUNTED_COMMAND_ENV),
            VISIBLE_PROG_ENV: env.get(VISIBLE_PROG_ENV),
        }
        captured["check"] = check
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr("spice.cli.mounts.subprocess.run", fake_run)
    mount = MountedCommand(
        path=("report", "inspect"),
        argv=("project-tool", "report", "inspect"),
        repo_root=tmp_path,
    )

    assert run_mounted_command(mount, ["--limit", "20"]) == 0
    assert captured == {
        "argv": ("project-tool", "report", "inspect", "--limit", "20"),
        "cwd": tmp_path,
        "env": {
            "VIRTUAL_ENV": str(tmp_path / ".venv"),
            "PATH": str(tmp_path / ".venv" / "bin"),
            MOUNTED_COMMAND_ENV: "1",
            VISIBLE_PROG_ENV: "spice report inspect",
        },
        "check": False,
    }


def test_dispatch_prefers_dotted_mount_before_builtin_parse(
    monkeypatch: pytest.MonkeyPatch, tmp_path
):
    _repo_with_commands(
        tmp_path,
        'report.inspect = ["project-tool", "report", "inspect"]\n',
    )
    monkeypatch.setattr("spice.cli.mounts.repo_root_from_cwd", lambda: tmp_path)
    captured: dict[str, object] = {}

    def fake_run_mounted_command(mount, args):
        captured["path"] = mount.path
        captured["argv"] = mount.argv
        captured["args"] = list(args)
        return 0

    monkeypatch.setattr(
        "spice.cli.mounts.run_mounted_command", fake_run_mounted_command
    )
    assert cli_entry._dispatch(["report", "inspect", "--limit", "20"]) == 0
    assert captured == {
        "path": ("report", "inspect"),
        "argv": ("project-tool", "report", "inspect"),
        "args": ["--limit", "20"],
    }


def test_wrapper_command_contract_is_linked_from_readme():
    readme = Path("README.md").read_text(encoding="utf-8")
    contract = Path("docs/cli/wrapper-commands.md").read_text(encoding="utf-8")

    assert "### Agent command wrapper" in readme
    assert 'spice agent run -- <shell> -c "<original command>"' in readme
    assert 'wrappers = ["common", "repo-tools"]' in readme
    assert "[tool.spice.wrappers.common]" in readme
    assert "[tool.spice.wrappers.repo-tools]" in readme
    assert "docs/cli/wrapper-commands.md" in readme
    assert readme.index("### Agent command wrapper") < readme.index(
        "### Repo command mounts"
    )
    assert "spice agent run -- <cmd>" in contract
    assert "[tool.spice.commands]" in contract
    assert "RTK rewrite routing" in contract
    assert 'wrappers = ["common", "repo-tools"]' in contract
    assert "[tool.spice.wrappers.repo-tools]" in contract
