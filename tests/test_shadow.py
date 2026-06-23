"""Agent git shadow precedence behavior."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

from spice.agent.shadow import shadow_environment


def test_shadow_environment_exports_native_merge_as_command_backstop(tmp_path):
    repo = _init_lane(tmp_path)
    _git(repo, "config", "branch.main-d.remote", "origin")
    _git(repo, "config", "branch.main-d.merge", "refs/heads/main")

    env = shadow_environment(repo, base_env={"PATH": os.environ["PATH"]})

    assert _config_values(env, "branch.main-d.merge") == ["refs/heads/main"]
    assert (
        _git_stdout(
            repo,
            "rev-parse",
            "--abbrev-ref",
            "main-d@{upstream}",
            env=env,
        )
        == "main-d"
    )
    assert _git_stdout(repo, "config", "--get", "branch.main-d.merge", env=env) == (
        "refs/heads/main"
    )


def test_shadow_environment_derives_true_merge_from_origin_head(tmp_path):
    repo = _init_lane(tmp_path)
    _git(repo, "remote", "add", "origin", str(tmp_path / "origin.git"))
    _git(repo, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk")

    env = shadow_environment(repo, base_env={"PATH": os.environ["PATH"]})

    assert _config_values(env, "branch.main-d.merge") == ["refs/heads/trunk"]
    assert (
        _git_stdout(
            repo,
            "rev-parse",
            "--abbrev-ref",
            "main-d@{upstream}",
            env=env,
        )
        == "main-d"
    )
    assert _git_stdout(repo, "config", "--get", "branch.main-d.merge", env=env) == (
        "refs/heads/trunk"
    )


def _init_lane(tmp_path: Path) -> Path:
    repo = tmp_path / "lane"
    _git(tmp_path, "init", "-q", "-b", "main-d", str(repo))
    _git(repo, "config", "user.email", "spice@example.test")
    _git(repo, "config", "user.name", "Spice Tests")
    _git(repo, "commit", "-q", "--allow-empty", "-m", "c0")
    return repo


def _config_values(env: dict[str, str], key: str) -> list[str]:
    return [
        env[f"GIT_CONFIG_VALUE_{index}"]
        for index in range(int(env.get("GIT_CONFIG_COUNT", "0")))
        if env[f"GIT_CONFIG_KEY_{index}"] == key
    ]


def _git_stdout(repo: Path, *args: str, env: dict[str, str]) -> str:
    return _git(repo, *args, env={**os.environ, **env}).stdout.strip()


def _git(
    repo: Path, *args: str, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        check=True,
        env=env,
        text=True,
    )
