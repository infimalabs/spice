"""Bounded git subprocess execution."""

from __future__ import annotations

import os
import subprocess
import sys

import pytest

from spice import gitprocess
from spice.errors import SpiceError
from spice.worktrees import list_worktrees


def test_worktree_git_stall_expires_under_the_configured_deadline(
    tmp_path, monkeypatch
):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    git = bin_dir / "git"
    git.write_text(
        f"#!{sys.executable}\nimport threading\nthreading.Event().wait()\n",
        encoding="utf-8",
    )
    git.chmod(0o755)
    current_path = os.environ["PATH"]  # env-policy: allow
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{current_path}")
    monkeypatch.setenv(gitprocess.GIT_TIMEOUT_ENV, "0.05")

    with pytest.raises(SpiceError) as exc_info:
        list_worktrees(cwd=tmp_path)

    assert str(exc_info.value) == (
        "git command timed out after 0.05s: git worktree list --porcelain; "
        f"increase {gitprocess.GIT_TIMEOUT_ENV} for a slower repository"
    )


def test_git_timeout_environment_applies_to_local_commands(monkeypatch):
    seen: dict[str, object] = {}

    def fake_run(command: list[str], **kwargs):
        seen["command"] = command
        seen["timeout"] = kwargs["timeout"]
        return subprocess.CompletedProcess(command, 0, stdout="")

    monkeypatch.setenv(gitprocess.GIT_TIMEOUT_ENV, "37.5")
    monkeypatch.setattr(gitprocess.subprocess, "run", fake_run)

    gitprocess.run_git_command(["git", "status"], capture_output=True, text=True)

    assert seen == {"command": ["git", "status"], "timeout": 37.5}
