"""Mounted commands: validation, precedence, dotted-path dispatch."""

from pathlib import Path
import sys
from types import SimpleNamespace

import pytest

from spice import extensions as extension_loader
from spice.cli import entry as cli_entry
from spice.cli.mounts import (
    MOUNT_SEGMENT_RE,
    MOUNTED_COMMAND_ENV,
    MountedCommand,
    RUNTIME_PYTHON_ENV,
    VISIBLE_PROG_ENV,
    find_mounted_command,
    mounted_commands,
    resolve_mounted_commands,
    run_mounted_command,
)
from spice.cli.parser import BUILTIN_COMMANDS, build_parser
from spice.errors import SpiceError
from tests.test_extensionhelpers import (
    FilteredExtensionDistribution,
    build_fixture_distribution,
)


def _repo_with_commands(tmp_path, body: str):
    (tmp_path / ".git").mkdir()
    (tmp_path / "spice.toml").write_text(f"[commands]\n{body}\n", encoding="utf-8")
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


def test_top_level_mount_shadowing_builtin_is_refused_without_hiding_siblings(
    tmp_path,
):
    repo = _repo_with_commands(
        tmp_path,
        'task = "./scripts/task.sh"\nprobe = "./scripts/probe.sh"',
    )

    resolution = resolve_mounted_commands(repo)

    assert resolution.commands == {("probe",): ("./scripts/probe.sh",)}
    assert len(resolution.refusals) == 1
    assert (
        f"commands (source=repository path={repo / 'spice.toml'})"
        in (resolution.refusals[0])
    )
    assert "entry 'task'" in resolution.refusals[0]
    assert "shadows a built-in" in resolution.refusals[0]
    assert mounted_commands(repo) == {("probe",): ("./scripts/probe.sh",)}


@pytest.mark.parametrize("command", BUILTIN_COMMANDS)
def test_builtin_help_survives_top_level_mount_shadowing(
    tmp_path, monkeypatch, command
):
    repo = _repo_with_commands(tmp_path, 'task = "./scripts/task.sh"')
    monkeypatch.setattr("spice.cli.mounts.repo_root_from_cwd", lambda: repo)

    with pytest.raises(SystemExit) as exc_info:
        cli_entry._dispatch([command, "--help"])

    assert exc_info.value.code == 0


def test_top_level_help_survives_top_level_mount_shadowing(tmp_path, monkeypatch):
    repo = _repo_with_commands(tmp_path, 'task = "./scripts/task.sh"')
    monkeypatch.setattr("spice.cli.mounts.repo_root_from_cwd", lambda: repo)

    with pytest.raises(SystemExit) as exc_info:
        cli_entry._dispatch(["--help"])

    assert exc_info.value.code == 0


def test_study_mount_shadowing_builtin_action_is_refused(tmp_path):
    repo = _repo_with_commands(
        tmp_path, '"study.csharp-members" = "./scripts/csharp-members.sh"'
    )
    resolution = resolve_mounted_commands(repo)

    assert resolution.commands == {}
    assert len(resolution.refusals) == 1
    message = resolution.refusals[0]
    assert f"commands (source=repository path={repo / 'spice.toml'})" in message
    assert "'study.csharp-members' shadows" in message
    assert "spice action 'spice study csharp-members'" in message


def test_dev_mount_shadowing_builtin_action_is_refused(tmp_path):
    repo = _repo_with_commands(tmp_path, '"dev.pre-commit" = "./scripts/pre-commit.sh"')
    resolution = resolve_mounted_commands(repo)

    assert resolution.commands == {}
    assert len(resolution.refusals) == 1
    message = resolution.refusals[0]
    assert f"commands (source=repository path={repo / 'spice.toml'})" in message
    assert "'dev.pre-commit' shadows" in message
    assert "spice action 'spice dev pre-commit'" in message


def test_mount_shadowing_extension_study_action_is_refused(tmp_path, monkeypatch):
    _, distribution = build_fixture_distribution(tmp_path)
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
    repo = _repo_with_commands(tmp_path, '"study.toy-study" = "./scripts/toy-study.sh"')

    resolution = resolve_mounted_commands(repo)

    assert resolution.commands == {}
    assert len(resolution.refusals) == 1
    message = resolution.refusals[0]
    assert f"commands (source=repository path={repo / 'spice.toml'})" in message
    assert "'study.toy-study' shadows" in message
    assert "extension-provided spice action 'spice study toy-study'" in message
    assert "spice-extension-fixture" in message


def test_dotted_mount_under_builtin_with_novel_action_dispatches(tmp_path, monkeypatch):
    _repo_with_commands(
        tmp_path,
        '"study.repo-tool" = ["project-tool", "study", "repo-tool"]',
    )
    monkeypatch.setattr("spice.cli.mounts.repo_root_from_cwd", lambda: tmp_path)

    resolved = find_mounted_command(["study", "repo-tool", "--limit", "20"])

    assert resolved is not None
    mount, remainder = resolved
    assert mount.path == ("study", "repo-tool")
    assert mount.argv == ("project-tool", "study", "repo-tool")
    assert remainder == ["--limit", "20"]


def test_non_builtin_nested_mounts_are_allowed(tmp_path):
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
    monkeypatch.setenv("PATH", "/usr/bin")
    monkeypatch.setenv("VIRTUAL_ENV", "/tmp/foreign-venv")

    def fake_run(argv, *, cwd, env, capture_output, text, check):
        captured["argv"] = tuple(argv)
        captured["cwd"] = cwd
        captured["env"] = {
            "VIRTUAL_ENV": env.get("VIRTUAL_ENV"),
            "PATH": env.get("PATH"),
            MOUNTED_COMMAND_ENV: env.get(MOUNTED_COMMAND_ENV),
            RUNTIME_PYTHON_ENV: env.get(RUNTIME_PYTHON_ENV),
            VISIBLE_PROG_ENV: env.get(VISIBLE_PROG_ENV),
        }
        captured["capture_output"] = capture_output
        captured["text"] = text
        captured["check"] = check
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
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
            "VIRTUAL_ENV": "/tmp/foreign-venv",
            "PATH": "/usr/bin",
            MOUNTED_COMMAND_ENV: "1",
            RUNTIME_PYTHON_ENV: sys.executable,
            VISIBLE_PROG_ENV: "spice report inspect",
        },
        "capture_output": True,
        "text": True,
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

    assert "spice agent run -- <cmd>" in readme
    assert "docs/cli/wrapper-commands.md" in readme
    assert "spice agent run -- <cmd>" in contract
    assert "[commands]" in contract
    assert "RTK rewrite routing" in contract
    assert 'spice agent run -- <shell> -c "<original command>"' in contract
    assert 'wrappers = ["common", "repo-tools"]' in contract
    assert "[wrappers.common]" in contract
    assert "[wrappers.repo-tools]" in contract
