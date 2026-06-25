"""Agent git shadow precedence behavior."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import io

from spice.agent import shadow
from spice.agent.shadow import (
    append_git_config_pair,
    shadow_environment,
    write_shadow_config,
)


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


def test_duplicate_branch_merge_precedence_is_a_tested_git_assumption(tmp_path):
    # LOAD-BEARING, VERSION-DEPENDENT ASSUMPTION (not a git guarantee):
    # the entire shadow (spice/agent/shadow.py) rests on git resolving a
    # *duplicated* branch.<name>.merge differently for its two readers —
    # `@{upstream}` consumes the FIRST occurrence (the system-scope self-merge)
    # while `git config --get` returns the LAST (the command-scope true merge).
    # The `--get`-last half is documented; the `@{upstream}`-first half is
    # observed, not promised. This test pins both halves against the installed
    # git with distinct sentinel values so a future git that changes duplicate
    # resolution fails here, loudly, at the assumption itself.
    repo = _init_lane(tmp_path)
    _git(repo, "branch", "integration")  # integration at c0
    _git(repo, "commit", "-q", "--allow-empty", "-m", "c1")  # main-d advances past it
    self_rev = _git_stdout(repo, "rev-parse", "main-d", env={})
    integration_rev = _git_stdout(repo, "rev-parse", "integration", env={})
    assert self_rev != integration_rev

    config_path = write_shadow_config(repo, "main-d")  # system scope: merge=self
    assert config_path is not None
    env = {"PATH": os.environ["PATH"], "GIT_CONFIG_SYSTEM": str(config_path)}
    env = append_git_config_pair(env, "branch.main-d.remote", ".")
    # command scope: a *second* branch.main-d.merge, the true integration branch.
    env = append_git_config_pair(env, "branch.main-d.merge", "refs/heads/integration")

    # @{upstream} takes the FIRST duplicate -> the self-merge -> main-d itself.
    assert _git_stdout(repo, "rev-parse", "main-d@{upstream}", env=env) == self_rev
    assert (
        _git_stdout(repo, "rev-parse", "main-d@{upstream}", env=env) != integration_rev
    )
    # `config --get` takes the LAST duplicate -> the command-scope true merge.
    assert _git_stdout(repo, "config", "--get", "branch.main-d.merge", env=env) == (
        "refs/heads/integration"
    )


def test_write_shadow_config_writes_atomically(tmp_path, monkeypatch):
    repo = _init_lane(tmp_path)
    used: list[Path] = []
    real = shadow.atomic_write_text

    def spy(path, text):
        used.append(path)
        return real(path, text)

    monkeypatch.setattr(shadow, "atomic_write_text", spy)

    config_path = write_shadow_config(repo, "main-d")

    assert config_path is not None
    assert used == [config_path]  # routed through the atomic helper
    assert "merge = refs/heads/main-d" in config_path.read_text(encoding="utf-8")
    # No torn-write temp file is left behind beside the config.
    assert list(config_path.parent.glob(f"{config_path.name}.*.tmp")) == []


def test_shadow_environment_notes_detached_head_instead_of_silent_passthrough(
    tmp_path,
):
    repo = _init_lane(tmp_path)
    head = _git_stdout(repo, "rev-parse", "HEAD", env={})
    _git(repo, "checkout", "-q", "--detach", head)
    stderr = io.StringIO()

    env = shadow_environment(repo, base_env={"PATH": os.environ["PATH"]}, stderr=stderr)

    assert "GIT_CONFIG_SYSTEM" not in env
    assert "detached HEAD" in stderr.getvalue()


def test_system_config_discovery_failure_is_logged_and_falls_back(
    tmp_path, monkeypatch
):
    repo = _init_lane(tmp_path)
    monkeypatch.setattr(shadow, "real_system_config_path", lambda _repo: None)
    stderr = io.StringIO()

    config_path = write_shadow_config(repo, "main-d", stderr=stderr)

    assert config_path is not None
    assert "path = /etc/gitconfig" in config_path.read_text(encoding="utf-8")
    assert "could not resolve git's system config path" in stderr.getvalue()


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
