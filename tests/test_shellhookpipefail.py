"""Shell hook pipefail contracts."""

from __future__ import annotations

import os
import shutil
import subprocess

import pytest

from spice.agent import shellhook


def test_packaged_shell_hooks_set_pipefail():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()

    for name in (*shellhook.ZSH_HOOK_NAMES, shellhook.BASH_HOOK_NAME):
        assert "set -o pipefail" in (hook_dir / name).read_text(encoding="utf-8")


@pytest.mark.parametrize(
    ("shell_name", "env_name", "env_value"),
    [
        ("bash", shellhook.BASH_ENV_ENV, shellhook.BASH_HOOK_NAME),
        ("zsh", shellhook.ZDOTDIR_ENV, ""),
    ],
)
def test_stage_two_shell_hooks_enable_pipefail(
    tmp_path, shell_name: str, env_name: str, env_value: str
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    home = tmp_path / "home"
    home.mkdir()
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    hook_path = hook_dir / env_value if env_value else hook_dir
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        env_name: str(hook_path),
        shellhook.SHELL_HOOK_REEXEC_STAGE_ENV: "1",
        shellhook.SHELL_HOOK_WRAPPERS_ENV: "",
        **shellhook.shell_steering_runtime_environment(base_env={"HOME": str(home)}),
    }

    completed = subprocess.run(
        [shell, "-c", "false | true"],
        check=False,
        capture_output=True,
        env=env,
        text=True,
        timeout=2,
    )

    assert completed.returncode == 1
