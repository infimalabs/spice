"""Tracked executable configuration requires one operator-owned approval."""

from __future__ import annotations

import json
import shlex
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.agent.maxims import judge_cli_backend
from spice.agent.rtkhealth import probe_rtk_health
from spice.agent.shellhook import render_agent_wrapper_lines
from spice.cli.mounts import MountedCommand, run_mounted_command
from spice.config import values
from spice.config.trust import (
    repository_config_approval,
    require_repository_config_approval,
)
from spice.errors import SpiceError
from spice.hooks import precommit
from spice.hooks.cli import handle_init
from spice.hooks.initplan import (
    InitializationMode,
    apply_initialization_plan,
    plan_initialization,
)
from spice.operatorstate import WORKTREE_CONFIG_PATH, operator_state_path
from spice.serve.audio import speech_backend
from spice.studies.reachability import (
    ReachabilityScanRequest,
    reachability_provider_registry,
)
from spice.studies.suiteseam import run_suite_seam_gate
from spice.studies.typecheck import run_python_typecheck


def test_hostile_tracked_pre_commit_replacement_refuses_before_execution(tmp_path):
    repo = _repository(tmp_path / "repo")
    marker = tmp_path / "executed"
    command = (
        sys.executable,
        "-c",
        f"from pathlib import Path; Path({str(marker)!r}).write_text('ran')",
    )
    _commit_config(
        repo,
        "[policy.pre_commit_builtins]\n"
        f"formatters = {{ run = {json.dumps(command)} }}\n",
    )

    with pytest.raises(SpiceError) as raised:
        precommit.pre_commit_steps(repo, [])

    message = str(raised.value)
    assert "policy.pre_commit_builtins.formatters" in message
    assert f"refusing command {shlex.join(command)}" in message
    assert "`spice init --apply`" in message
    assert not marker.exists()


def test_approval_is_per_repository_digest_and_reprompts_after_change(tmp_path):
    repo = _repository(tmp_path / "source")
    command = ("tool", "first")
    _commit_config(repo, f"[commands]\nprobe = {json.dumps(command)}\n")

    with pytest.raises(SpiceError, match="has no operator approval"):
        _require_probe_approval(repo, command)

    _approve_repository_config(repo)
    first = repository_config_approval(repo)
    assert first.approved
    assert first.approved_digest == first.digest
    _require_probe_approval(repo, command)

    changed = ("tool", "second")
    (repo / "spice.toml").write_text(
        f"[commands]\nprobe = {json.dumps(changed)}\n",
        encoding="utf-8",
    )
    with pytest.raises(SpiceError) as raised:
        _require_probe_approval(repo, changed)
    assert "changed since operator approval" in str(raised.value)
    assert f"approved={first.digest}" in str(raised.value)

    _approve_repository_config(repo)
    second = repository_config_approval(repo)
    assert second.approved
    assert second.digest != first.digest
    _require_probe_approval(repo, changed)

    clone = tmp_path / "clone"
    _git(tmp_path, "clone", "-q", str(repo), str(clone))
    with pytest.raises(SpiceError, match="has no operator approval"):
        _require_probe_approval(clone, command)


def test_init_apply_records_the_current_repository_approval(
    tmp_path, monkeypatch, capsys
):
    repo = _repository(tmp_path / "repo")
    _commit_config(repo, '[commands]\nprobe = ["tool", "first"]\n')
    monkeypatch.chdir(repo)

    assert (
        handle_init(SimpleNamespace(gates=True, apply=True, json=False, unapply=None))
        == 0
    )

    assert repository_config_approval(repo).approved
    assert "ready: git commit | spice dev pre-commit" in capsys.readouterr().out


def test_repository_mount_refuses_without_starting_its_child(tmp_path, monkeypatch):
    repo = _repository(tmp_path / "repo")
    command = ("hostile-tool", "--write")
    _commit_config(repo, f"[commands]\nprobe = {json.dumps(command)}\n")
    mount = MountedCommand(path=("probe",), argv=command, repo_root=repo)
    started: list[list[str]] = []

    def run_child(argv, **_kwargs):
        started.append(list(argv))
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", run_child)

    with pytest.raises(SpiceError, match="has no operator approval"):
        run_mounted_command(mount, ["target"])

    assert started == []


@pytest.mark.parametrize(
    "changed_config",
    (
        '[commands]\nprobe = ["tool", "changed"]\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        '[wrappers.extra.probe]\nargv = ["wrapper-tool"]\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        '[policy]\npre_commit = [{ label = "pre", run = ["pre-tool"] }]\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        "[policy]\npre_commit_success = ["
        '{ label = "post", run = ["post-tool"] }]\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        "[policy.pre_commit_builtins]\n"
        'formatters = { run = ["replacement-tool"] }\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        '[say]\nbackend = "external"\ncommand = "speech-tool --audio"\n',
        '[commands]\nprobe = ["tool", "first"]\n\n[judge]\nbin = "judge-tool"\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        '[rtk]\nexecutable = "alternate-rtk"\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        '[policy.suite_seam]\npaths = ["spice"]\nrun = ["suite-tool"]\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        "[[policy.reachability_providers]]\n"
        'name = "external"\nrun = ["provider-tool"]\n',
        '[commands]\nprobe = ["tool", "first"]\n\n'
        '[policy]\npython_typecheck_interpreter = ".venv/bin/python"\n',
    ),
)
def test_every_executable_surface_change_invalidates_approval(tmp_path, changed_config):
    repo = _repository(tmp_path / "repo")
    _commit_config(repo, '[commands]\nprobe = ["tool", "first"]\n')
    _approve_repository_config(repo)
    approved = repository_config_approval(repo)
    assert approved.approved

    (repo / "spice.toml").write_text(changed_config, encoding="utf-8")

    changed = repository_config_approval(repo)
    assert not changed.approved
    assert changed.digest != approved.digest


def test_tracked_wrapper_group_refuses_with_its_command_words(tmp_path):
    repo = _repository(tmp_path / "repo")
    command = (sys.executable, "-m", "hostile_wrapper")
    _commit_config(
        repo,
        "[agent]\n"
        'wrappers = ["hostile"]\n\n'
        "[wrappers.hostile.probe]\n"
        f"argv = {json.dumps(command)}\n",
    )

    with pytest.raises(SpiceError) as raised:
        render_agent_wrapper_lines(repo)

    message = str(raised.value)
    assert "wrappers.hostile" in message
    assert f"refusing command {shlex.join(command)}" in message


def test_hostile_tracked_suite_seam_refuses_before_execution(tmp_path):
    repo = _repository(tmp_path / "repo")
    marker = tmp_path / "suite-executed"
    command = (
        sys.executable,
        "-c",
        f"from pathlib import Path; Path({str(marker)!r}).write_text('ran')",
    )
    _commit_config(
        repo,
        f'[policy.suite_seam]\npaths = ["target"]\nrun = {json.dumps(command)}\n',
    )

    with pytest.raises(SpiceError) as raised:
        run_suite_seam_gate(repo, ["target"], label="TASK-hostile")

    message = str(raised.value)
    assert "policy.suite_seam.run" in message
    assert f"refusing command {shlex.join(command)}" in message
    assert not marker.exists()


def test_tracked_external_speech_refuses_at_render_before_starting_child(
    tmp_path, monkeypatch
):
    repo = _repository(tmp_path / "repo")
    command = ("speech-tool", "--rate", "{words_per_minute}")
    _commit_config(
        repo,
        f'[say]\nbackend = "external"\ncommand = {json.dumps(shlex.join(command))}\n',
    )
    started: list[list[str]] = []
    monkeypatch.setattr(
        "spice.serve.audio.run_bounded_process_group",
        lambda argv, **_kwargs: started.append(list(argv)),
    )

    backend = speech_backend(repo)
    with pytest.raises(SpiceError) as raised:
        backend.render("hostile")

    assert "say.command" in str(raised.value)
    assert f"speech-tool --rate {values.DEFAULT_SAY_WORDS_PER_MINUTE}" in str(
        raised.value
    )
    assert started == []


def test_tracked_judge_refuses_before_starting_child(tmp_path, monkeypatch):
    repo = _repository(tmp_path / "repo")
    _commit_config(repo, '[judge]\nbin = "judge-tool"\n')
    monkeypatch.chdir(repo)
    started: list[list[str]] = []

    def run(argv, **_kwargs):
        started.append(list(argv))
        return SimpleNamespace(returncode=0, stdout="YES", stderr="")

    with pytest.raises(SpiceError) as raised:
        judge_cli_backend("prompt", run=run)

    assert "judge.bin" in str(raised.value)
    assert "refusing command judge-tool" in str(raised.value)
    assert started == []


def test_tracked_rtk_refuses_before_health_probe(tmp_path):
    repo = _repository(tmp_path / "repo")
    _commit_config(repo, '[rtk]\nexecutable = "hostile-rtk"\n')
    started: list[list[str]] = []

    def run(argv, **_kwargs):
        started.append(list(argv))
        return subprocess.CompletedProcess(argv, 0, stdout="rtk 0.42.4", stderr="")

    with pytest.raises(SpiceError) as raised:
        probe_rtk_health(repo, run=run)

    assert "rtk.executable" in str(raised.value)
    assert "refusing command hostile-rtk --version" in str(raised.value)
    assert started == []


def test_tracked_reachability_provider_refuses_before_starting_child(
    tmp_path, monkeypatch
):
    repo = _repository(tmp_path / "repo")
    command = ("provider-tool", "--scan")
    _commit_config(
        repo,
        "[[policy.reachability_providers]]\n"
        'name = "external"\n'
        f"run = {json.dumps(command)}\n",
    )
    started: list[list[str]] = []
    monkeypatch.setattr(
        "spice.studies.reachability.run_tool_command",
        lambda argv, **_kwargs: started.append(list(argv)),
    )
    provider = next(
        item for item in reachability_provider_registry(repo) if item.name == "external"
    )
    request = ReachabilityScanRequest(repo, "spice", (), ())

    with pytest.raises(SpiceError) as raised:
        provider.scan(request)

    assert "policy.reachability_providers" in str(raised.value)
    assert f"refusing command {shlex.join(command)}" in str(raised.value)
    assert started == []


def test_tracked_typecheck_interpreter_refuses_before_starting_child(
    tmp_path, monkeypatch
):
    repo = _repository(tmp_path / "repo")
    _commit_config(
        repo,
        "[policy]\n"
        'package_roots = ["pkg"]\n'
        'python_typecheck_interpreter = ".venv/bin/python"\n',
    )
    argv = ("pyright", "--pythonpath", ".venv/bin/python", "pkg")
    started: list[tuple[str, ...]] = []
    monkeypatch.setattr(
        "spice.studies.typecheck.python_typecheck_targets",
        lambda _repo: ("pkg",),
    )
    monkeypatch.setattr(
        "spice.studies.typecheck.python_typecheck_argv",
        lambda _repo, _targets: argv,
    )
    monkeypatch.setattr(
        "spice.studies.typecheck.run_typecheck_command",
        lambda command, **_kwargs: started.append(tuple(command)),
    )

    with pytest.raises(SpiceError) as raised:
        run_python_typecheck(repo)

    assert "policy.python_typecheck_interpreter" in str(raised.value)
    assert f"refusing command {shlex.join(argv)}" in str(raised.value)
    assert started == []


def test_clone_cannot_supply_operator_config_and_local_operator_mount_is_exempt(
    tmp_path, monkeypatch
):
    source = _repository(tmp_path / "source")
    _commit_config(source, '[agent]\nwrappers = ["common"]\n')
    source_operator_config = operator_state_path(source, WORKTREE_CONFIG_PATH)
    source_operator_config.parent.mkdir(parents=True, exist_ok=True)
    source_operator_config.write_text(
        '[commands]\nprobe = ["source-only"]\n',
        encoding="utf-8",
    )

    clone = tmp_path / "clone"
    _git(tmp_path, "clone", "-q", str(source), str(clone))
    clone_operator_config = operator_state_path(clone, WORKTREE_CONFIG_PATH)
    assert not clone_operator_config.exists()

    local_command = ("operator-tool", "--safe")
    clone_operator_config.parent.mkdir(parents=True, exist_ok=True)
    clone_operator_config.write_text(
        f"[commands]\nprobe = {json.dumps(local_command)}\n",
        encoding="utf-8",
    )
    mount = MountedCommand(path=("probe",), argv=local_command, repo_root=clone)
    started: list[list[str]] = []

    def run_child(argv, **_kwargs):
        started.append(list(argv))
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", run_child)

    assert run_mounted_command(mount, ["target"]) == 0
    assert started == [["operator-tool", "--safe", "target"]]
    assert repository_config_approval(clone).approved is False


def _require_probe_approval(repo: Path, command: tuple[str, ...]) -> None:
    require_repository_config_approval(
        repo,
        ("commands", "probe"),
        command=shlex.join(command),
    )


def _approve_repository_config(repo: Path) -> None:
    plan = plan_initialization(
        repo,
        InitializationMode.GATES_ONLY,
        include_agent_skill=False,
    )
    apply_initialization_plan(plan, approve_repository_config=True)


def _repository(path: Path) -> Path:
    path.mkdir(parents=True)
    _git(path, "init", "-q", "-b", "main")
    _git(path, "config", "user.email", "fixture@example.com")
    _git(path, "config", "user.name", "Fixture")
    return path


def _commit_config(repo: Path, content: str) -> None:
    (repo / "spice.toml").write_text(content, encoding="utf-8")
    _git(repo, "add", "spice.toml")
    _git(repo, "commit", "-q", "-m", "config")


def _git(cwd: Path, *args: str) -> None:
    subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )
