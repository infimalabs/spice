"""Mounted commands: validation, built-in precedence, argv shapes."""

from pathlib import Path

import pytest

from spice.cli.mounts import MOUNT_NAME_RE, mounted_commands
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
        "probe": ("python", "-m", "myproj.probe", "--fast")
    }


def test_list_mounts_pass_argv_verbatim(tmp_path):
    repo = _repo_with_commands(
        tmp_path, 'release = ["uv", "run", "python", "-m", "spice.release"]'
    )
    assert mounted_commands(repo) == {
        "release": ("uv", "run", "python", "-m", "spice.release")
    }


def test_tool_family_mounts_use_one_namespace_owner(tmp_path):
    assert MOUNT_NAME_RE.fullmatch("toolbox")
    assert not MOUNT_NAME_RE.fullmatch("lint.css")
    assert not MOUNT_NAME_RE.fullmatch("lint css")
    repo = _repo_with_commands(tmp_path, 'toolbox = ["uv", "run", "toolbox"]')
    assert mounted_commands(repo) == {"toolbox": ("uv", "run", "toolbox")}


def test_mount_shadowing_builtin_fails_loudly(tmp_path):
    repo = _repo_with_commands(tmp_path, 'task = "./scripts/task.sh"')
    with pytest.raises(SpiceError, match="shadows a built-in"):
        mounted_commands(repo)


def test_mount_name_shape_is_enforced(tmp_path):
    assert MOUNT_NAME_RE.fullmatch("lane-tools")
    repo = _repo_with_commands(tmp_path, '"Bad_Name" = "./run.sh"')
    with pytest.raises(SpiceError, match="must match"):
        mounted_commands(repo)


def test_empty_mount_fails_loudly(tmp_path):
    repo = _repo_with_commands(tmp_path, 'noop = ""')
    with pytest.raises(SpiceError, match="empty"):
        mounted_commands(repo)


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
    assert "spice agent run -- proxy <command>" in contract
    assert 'wrappers = ["common", "repo-tools"]' in contract
    assert "[tool.spice.wrappers.repo-tools]" in contract
