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
    hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    hook_path = hook_dir / env_value if env_value else hook_dir
    env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),  # env-policy: allow
        env_name: str(hook_path),
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


@pytest.mark.parametrize(
    ("shell_name", "env_name", "env_value"),
    [
        ("bash", shellhook.BASH_ENV_ENV, shellhook.BASH_HOOK_NAME),
        ("zsh", shellhook.ZDOTDIR_ENV, ""),
    ],
)
def test_stage_two_shell_snapshot_restores_original_startup_paths(
    tmp_path, shell_name: str, env_name: str, env_value: str
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    home = tmp_path / "home"
    home.mkdir()
    real_zdotdir = tmp_path / "real-zdotdir"
    real_zdotdir.mkdir()
    (real_zdotdir / ".zshenv").write_text(":\n", encoding="utf-8")
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text(":\n", encoding="utf-8")
    hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    hook_path = hook_dir / env_value if env_value else hook_dir
    base_env = {
        "HOME": str(home),
        shellhook.ZDOTDIR_ENV: str(real_zdotdir),
        shellhook.BASH_ENV_ENV: str(real_bash_env),
    }
    env = {
        **base_env,
        "PATH": os.environ.get("PATH", ""),  # env-policy: allow
        env_name: str(hook_path),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: "",
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    completed = subprocess.run(
        [
            shell,
            "-c",
            'printf \'%s\\n%s\\n\' "$ZDOTDIR" "$BASH_ENV"',
        ],
        check=False,
        capture_output=True,
        env=env,
        text=True,
        timeout=2,
    )

    assert completed.returncode == 0, completed.stderr
    assert completed.stdout.splitlines() == [
        str(real_zdotdir),
        str(real_bash_env),
    ]


@pytest.mark.parametrize(
    ("shell_name", "env_name", "env_value"),
    [
        ("bash", shellhook.BASH_ENV_ENV, shellhook.BASH_HOOK_NAME),
        ("zsh", shellhook.ZDOTDIR_ENV, ""),
    ],
)
def test_stage_two_wrappers_stop_before_descendant_shells(
    tmp_path, shell_name: str, env_name: str, env_value: str
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    home = tmp_path / "home"
    home.mkdir()
    real_zdotdir = tmp_path / "real-zdotdir"
    real_zdotdir.mkdir()
    (real_zdotdir / ".zshenv").write_text(":\n", encoding="utf-8")
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text(":\n", encoding="utf-8")
    hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    hook_path = hook_dir / env_value if env_value else hook_dir
    base_env = {
        "HOME": str(home),
        shellhook.ZDOTDIR_ENV: str(real_zdotdir),
        shellhook.BASH_ENV_ENV: str(real_bash_env),
    }
    env = {
        **base_env,
        "CHILD_SHELL": shell,
        "PATH": os.environ.get("PATH", ""),  # env-policy: allow
        env_name: str(hook_path),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: (
            "spice_test_wrapper() { printf 'outer-wrapper'; }"
        ),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    completed = subprocess.run(
        [
            shell,
            "-c",
            (
                "spice_test_wrapper; printf '\\n'; "
                '"$CHILD_SHELL" -c \'if command -v spice_test_wrapper '
                ">/dev/null 2>&1; then exit 43; fi; false | true; "
                "if [ $? -ne 0 ]; then exit 44; fi; printf descendant-native'"
            ),
        ],
        check=False,
        capture_output=True,
        env=env,
        text=True,
        timeout=2,
    )

    assert completed.returncode == 0, completed.stderr
    assert completed.stdout == "outer-wrapper\ndescendant-native"


@pytest.mark.parametrize(
    ("shell_name", "env_name", "env_value"),
    [
        ("bash", shellhook.BASH_ENV_ENV, shellhook.BASH_HOOK_NAME),
        ("zsh", shellhook.ZDOTDIR_ENV, ""),
    ],
)
def test_stage_two_sourced_script_shares_wrappers_but_executed_script_does_not(
    tmp_path, shell_name: str, env_name: str, env_value: str
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    home = tmp_path / "home"
    home.mkdir()
    real_zdotdir = tmp_path / "real-zdotdir"
    real_zdotdir.mkdir()
    (real_zdotdir / ".zshenv").write_text(":\n", encoding="utf-8")
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text(":\n", encoding="utf-8")
    sourced_script = tmp_path / "sourced-script"
    sourced_script.write_text("spice_test_wrapper\n", encoding="utf-8")
    executed_script = tmp_path / "executed-script"
    executed_script.write_text(
        "\n".join(
            [
                f"#!{shell}",
                "if command -v spice_test_wrapper >/dev/null 2>&1; then exit 43; fi",
                "false | true",
                "if [ $? -ne 0 ]; then exit 44; fi",
                "printf executed-native",
                "",
            ]
        ),
        encoding="utf-8",
    )
    executed_script.chmod(0o755)
    hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    hook_path = hook_dir / env_value if env_value else hook_dir
    base_env = {
        "HOME": str(home),
        shellhook.ZDOTDIR_ENV: str(real_zdotdir),
        shellhook.BASH_ENV_ENV: str(real_bash_env),
    }
    env = {
        **base_env,
        "EXECUTED_SCRIPT": str(executed_script),
        "PATH": os.environ.get("PATH", ""),  # env-policy: allow
        "SOURCED_SCRIPT": str(sourced_script),
        env_name: str(hook_path),
        shellhook.SHELL_HOOK_WRAPPERS_ENV: (
            "spice_test_wrapper() { printf 'sourced-wrapper'; }"
        ),
        **shellhook.shell_steering_runtime_environment(base_env=base_env),
    }

    completed = subprocess.run(
        [shell, "-c", '. "$SOURCED_SCRIPT"; "$EXECUTED_SCRIPT"'],
        check=False,
        capture_output=True,
        env=env,
        text=True,
        timeout=2,
    )

    assert completed.returncode == 0, completed.stderr
    assert completed.stdout == "sourced-wrapperexecuted-native"
