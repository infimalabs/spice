"""Bounded task commands and coordination locks recover after contention."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from spice.agent import lifecycle
from spice.agent.paths import agent_worktree_state_dir
from spice.errors import SpiceError
from spice.locking import FileLockTimeout, exclusive_lock
from spice.mail import inbox
from spice.tasks import config, tw

LOCK_TEST_TIMEOUT_SECONDS = 0.02


def test_taskwarrior_mutation_timeout_keeps_state_and_next_operation_recovers(
    tmp_path, monkeypatch
):
    command_events: list[str] = []
    backend_events: list[str] = []
    state = {"phase": "todo", "claim": "actor-a"}

    def task_process(command, **kwargs):
        command_events.append(f"timeout={kwargs['timeout']:g}")
        if len(command_events) == 1:
            raise subprocess.TimeoutExpired(command, kwargs["timeout"])
        state.update(phase="review", claim="cleared")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(tw, "require_task_binary", lambda: None)
    monkeypatch.setattr(config, "bootstrap", lambda: tmp_path / "taskrc")
    monkeypatch.setattr(config, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(tw.subprocess, "run", task_process)
    monkeypatch.setattr(
        config,
        "mark_task_backend_changed",
        lambda reason, **_kwargs: backend_events.append(reason),
    )

    with pytest.raises(SpiceError) as exc_info:
        tw.run(["task-uuid", "modify", "phase:review", "claim_by:"])
    timed_out_state = dict(state)
    tw.run(["task-uuid", "modify", "phase:review", "claim_by:"])

    assert str(exc_info.value) == (
        f"Taskwarrior modify mutation timed out after "
        f"{tw.TASK_COMMAND_TIMEOUT_SECONDS:g}s: task rc:{tmp_path / 'taskrc'} "
        "rc.confirmation=no rc.bulk=0 rc.verbose=nothing task-uuid modify "
        "phase:review claim_by:"
    )
    assert timed_out_state == {"phase": "todo", "claim": "actor-a"}
    assert state == {"phase": "review", "claim": "cleared"}
    assert command_events == [
        f"timeout={tw.TASK_COMMAND_TIMEOUT_SECONDS:g}",
        f"timeout={tw.TASK_COMMAND_TIMEOUT_SECONDS:g}",
    ]
    assert backend_events == ["modify"]


@pytest.mark.parametrize("helper", ["repo-root", "common-dir", "branch"])
def test_task_local_git_helpers_timeout_with_identity_and_recover(
    helper, tmp_path, monkeypatch
):
    attempts: list[tuple[str, ...]] = []
    outputs = {
        "repo-root": str(tmp_path),
        "common-dir": ".git",
        "branch": "main",
    }

    def git_process(command, **kwargs):
        attempts.append(tuple(command))
        if len(attempts) == 1:
            raise subprocess.TimeoutExpired(command, kwargs["timeout"])
        return subprocess.CompletedProcess(
            command, 0, stdout=f"{outputs[helper]}\n", stderr=""
        )

    monkeypatch.setattr("spice.gitprocess.subprocess.run", git_process)
    if helper == "repo-root":
        operation = config.repo_root
        expected = tmp_path.resolve()
    elif helper == "common-dir":

        def common_dir_operation():
            return config.git_common_dir(tmp_path)

        operation = common_dir_operation
        expected = (tmp_path / ".git").resolve()
    else:
        monkeypatch.setattr(config, "repo_root", lambda: tmp_path)
        operation = tw.current_branch
        expected = "main"

    with pytest.raises(SpiceError) as exc_info:
        operation()
    recovered = operation()

    assert "git command timed out after" in str(exc_info.value)
    assert "git " in str(exc_info.value)
    assert recovered == expected
    assert len(attempts) == 2


def test_task_bootstrap_lock_timeout_names_action_and_recovers(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "backend"
    config.set_backend(str(backend))
    monkeypatch.setattr(config, "repo_root", lambda: repo)
    monkeypatch.setattr(
        config, "TASK_BOOTSTRAP_LOCK_TIMEOUT_SECONDS", LOCK_TEST_TIMEOUT_SECONDS
    )
    lock_path = config.bootstrap_lock_path()
    try:
        with exclusive_lock(lock_path, blocking=True):
            with pytest.raises(FileLockTimeout) as exc_info:
                config.write_taskrc()
        config.write_taskrc()
    finally:
        config.set_backend(None)

    assert str(exc_info.value) == (
        f"bootstrap task backend timed out after {LOCK_TEST_TIMEOUT_SECONDS:g}s "
        f"waiting for lock {lock_path}"
    )
    assert (backend / "taskrc").is_file()


def test_agent_ensure_lock_timeout_names_action_and_recovers(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.setattr(
        lifecycle, "AGENT_ENSURE_LOCK_TIMEOUT_SECONDS", LOCK_TEST_TIMEOUT_SECONDS
    )
    lock_path = agent_worktree_state_dir(repo) / lifecycle.AGENT_LOCK_FILE
    events: list[str] = []

    with exclusive_lock(lock_path, blocking=True):
        with pytest.raises(FileLockTimeout) as exc_info:
            with lifecycle.agent_ensure_lock(repo):
                events.append("contended-acquire")
    with lifecycle.agent_ensure_lock(repo):
        events.append("recovered-acquire")

    assert str(exc_info.value) == (
        f"ensure agent lifecycle timed out after {LOCK_TEST_TIMEOUT_SECONDS:g}s "
        f"waiting for lock {lock_path}"
    )
    assert events == ["recovered-acquire"]


def test_inbox_publish_lock_timeout_cleans_temp_and_recovers(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    directory = inbox.inbox_dir(repo)
    lock_path = directory / inbox.INBOX_PUBLISH_LOCK_NAME
    monkeypatch.setattr(
        inbox, "INBOX_PUBLISH_LOCK_TIMEOUT_SECONDS", LOCK_TEST_TIMEOUT_SECONDS
    )

    with exclusive_lock(lock_path, blocking=True):
        with pytest.raises(FileLockTimeout) as exc_info:
            inbox.write_inbox_item(repo, "deadline.txt", "steering")
    written = inbox.write_inbox_item(repo, "deadline.txt", "steering")

    assert str(exc_info.value) == (
        f"publish inbox item timed out after {LOCK_TEST_TIMEOUT_SECONDS:g}s "
        f"waiting for lock {lock_path}"
    )
    assert written.read_text(encoding="utf-8") == "steering"
    assert sorted(path.name for path in directory.iterdir()) == [
        inbox.INBOX_PUBLISH_LOCK_NAME,
        "deadline.txt",
    ]


def _init_repo(path: Path) -> Path:
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    return path
