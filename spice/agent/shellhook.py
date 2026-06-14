"""Shell startup hooks for agent side-channel steering."""

from __future__ import annotations

import json
import os
import shlex
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path

from spice.errors import SpiceError

ZDOTDIR_ENV = "ZDOTDIR"
BASH_ENV_ENV = "BASH_ENV"
BASH_HOOK_NAME = "bash_env"
ZSH_HOOK_NAMES = (".zshenv", ".zprofile", ".zlogin")
SHELL_HOOK_PYTHON_ENV = "SPICE_SHELL_HOOK_PYTHON"  # env-policy: allow
SHELL_HOOK_AGENT_RUN_ARGV_ENV = "SPICE_SHELL_HOOK_AGENT_RUN_ARGV"  # env-policy: allow
SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV = (
    "SPICE_SHELL_HOOK_ORIGINAL_ZDOTDIR"  # env-policy: allow
)
SHELL_HOOK_ORIGINAL_BASH_ENV_ENV = (
    "SPICE_SHELL_HOOK_ORIGINAL_BASH_ENV"  # env-policy: allow
)
SHELL_HOOK_SURFACE_FILES = {
    BASH_HOOK_NAME: BASH_HOOK_NAME,
    "zshenv": ".zshenv",
    "zprofile": ".zprofile",
    "zlogin": ".zlogin",
}
SHELL_HOOK_SURFACES = tuple(SHELL_HOOK_SURFACE_FILES)


def apply_shell_steering_environment(
    repo_root: Path,
    *,
    driver_state_dirname: str,
    base_env: Mapping[str, str],
) -> dict[str, str]:
    env = dict(base_env)
    env.update(shell_steering_runtime_environment(base_env=env))
    hook_dir = packaged_shell_steering_hook_dir()
    env[ZDOTDIR_ENV] = str(hook_dir)
    env[BASH_ENV_ENV] = str(hook_dir / BASH_HOOK_NAME)
    return env


def packaged_shell_steering_hook_dir() -> Path:
    hook_dir = Path(__file__).resolve().parent / "shellhooks"
    missing = [
        name
        for name in (*ZSH_HOOK_NAMES, BASH_HOOK_NAME)
        if not (hook_dir / name).is_file()
    ]
    if missing:
        raise SpiceError(
            "spice shell hook: packaged hook files missing: " + ", ".join(missing)
        )
    return hook_dir


def shell_steering_runtime_environment(
    *,
    base_env: Mapping[str, str],
    python_command: Sequence[str] | None = None,
) -> dict[str, str]:
    agent_run_command = [
        *(python_command or (sys.executable,)),
        "-m",
        "spice",
        "agent",
        "run",
        "--",
    ]
    return {
        SHELL_HOOK_PYTHON_ENV: sys.executable,
        SHELL_HOOK_AGENT_RUN_ARGV_ENV: json.dumps(
            agent_run_command, separators=(",", ":")
        ),
        SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV: base_env.get(ZDOTDIR_ENV, ""),
        SHELL_HOOK_ORIGINAL_BASH_ENV_ENV: base_env.get(BASH_ENV_ENV, ""),
    }


def render_shell_steering_hook_for_surface(
    surface: str, *, env: Mapping[str, str] | None = None
) -> str:
    environment = os.environ if env is None else env
    if surface not in SHELL_HOOK_SURFACE_FILES:
        raise SpiceError(
            "unsupported shell-hook surface "
            f"{surface!r}; expected one of {', '.join(SHELL_HOOK_SURFACES)}"
        )
    original_zdotdir = required_shell_hook_env(
        environment, SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV
    )
    original_bash_env = required_shell_hook_env(
        environment, SHELL_HOOK_ORIGINAL_BASH_ENV_ENV
    )
    agent_run_command = agent_run_command_from_env(environment)
    restore_lines = [
        restore_env_line(ZDOTDIR_ENV, original_zdotdir),
        restore_env_line(BASH_ENV_ENV, original_bash_env),
    ]
    return render_shell_steering_hook(
        agent_run_command=agent_run_command,
        restore_lines=restore_lines,
        real_source_path=real_source_path_for_surface(surface, environment),
        self_path=self_path_for_surface(surface, environment),
    )


def required_shell_hook_env(environment: Mapping[str, str], name: str) -> str:
    if name not in environment:
        raise SpiceError(f"spice shell hook: missing required {name}")
    return environment[name]


def agent_run_command_from_env(environment: Mapping[str, str]) -> list[str]:
    raw = required_shell_hook_env(environment, SHELL_HOOK_AGENT_RUN_ARGV_ENV)
    try:
        loaded = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SpiceError(
            f"spice shell hook: {SHELL_HOOK_AGENT_RUN_ARGV_ENV} must be JSON argv"
        ) from exc
    if not isinstance(loaded, list):
        raise SpiceError(
            f"spice shell hook: {SHELL_HOOK_AGENT_RUN_ARGV_ENV} must be JSON argv"
        )
    command = [str(part) for part in loaded if isinstance(part, str) and part]
    if not command:
        raise SpiceError(
            f"spice shell hook: {SHELL_HOOK_AGENT_RUN_ARGV_ENV} must be non-empty"
        )
    return command


def real_source_path_for_surface(
    surface: str, environment: Mapping[str, str]
) -> Path | None:
    if surface == BASH_HOOK_NAME:
        original_bash_env = required_shell_hook_env(
            environment, SHELL_HOOK_ORIGINAL_BASH_ENV_ENV
        )
        return Path(original_bash_env).expanduser() if original_bash_env else None
    original_zdotdir = required_shell_hook_env(
        environment, SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV
    )
    base = (
        Path(original_zdotdir).expanduser()
        if original_zdotdir
        else real_zdotdir_path(environment)
    )
    return base / SHELL_HOOK_SURFACE_FILES[surface]


def self_path_for_surface(surface: str, environment: Mapping[str, str]) -> Path:
    if surface == BASH_HOOK_NAME:
        current_bash_env = required_shell_hook_env(environment, BASH_ENV_ENV)
        return Path(current_bash_env).expanduser()
    current_zdotdir = required_shell_hook_env(environment, ZDOTDIR_ENV)
    return Path(current_zdotdir).expanduser() / SHELL_HOOK_SURFACE_FILES[surface]


def real_zdotdir_path(base_env: Mapping[str, str]) -> Path:
    if raw := base_env.get(ZDOTDIR_ENV):
        return Path(raw).expanduser()
    if home := base_env.get("HOME"):
        return Path(home).expanduser()
    return Path.home()


def restore_env_line(name: str, value: str) -> str:
    if value:
        return f"export {name}={shell_quote(value)}"
    return f"unset {name}"


def render_shell_steering_hook(
    *,
    agent_run_command: Sequence[str],
    restore_lines: Sequence[str],
    real_source_path: Path | None,
    self_path: Path,
) -> str:
    lines = [
        "# Generated by spice; do not edit.",
        *agent_run_reexec_lines(
            agent_run_command=agent_run_command,
            restore_lines=restore_lines,
        ),
        *restore_lines,
    ]
    if real_source_path is not None:
        source = shell_quote(str(real_source_path))
        self_file = shell_quote(str(self_path))
        lines.extend(
            [
                f"if [ -r {source} ] && [ {source} != {self_file} ]; then",
                f"  . {source}",
                "fi",
            ]
        )
    return "\n".join(lines) + "\n"


def agent_run_reexec_lines(
    *, agent_run_command: Sequence[str], restore_lines: Sequence[str]
) -> list[str]:
    command = shell_command(agent_run_command)
    restored = [f"  {line}" for line in restore_lines]
    return [
        'if [ -n "${ZSH_EXECUTION_STRING-}" ]; then',
        *restored,
        '  _spice_shell_bin="${SHELL:-${ZSH_NAME:-zsh}}"',
        '  case "$_spice_shell_bin" in',
        "    *zsh) ;;",
        '    *) _spice_shell_bin="${ZSH_NAME:-zsh}" ;;',
        "  esac",
        '  if ! command -v "$_spice_shell_bin" >/dev/null 2>&1; then',
        (
            '    printf "%s\\n" '
            '"spice shell hook: cannot resolve zsh for agent-run reexec" >&2'
        ),
        "    exit 127",
        "  fi",
        "  if [[ -o login ]]; then",
        f'    exec {command} "$_spice_shell_bin" -lc "$ZSH_EXECUTION_STRING"',
        ('    printf "%s\\n" "spice shell hook: failed to exec agent run" >&2'),
        "    exit 127",
        "  fi",
        f'  exec {command} "$_spice_shell_bin" -c "$ZSH_EXECUTION_STRING"',
        '  printf "%s\\n" "spice shell hook: failed to exec agent run" >&2',
        "  exit 127",
        "fi",
        'if [ -n "${BASH_EXECUTION_STRING-}" ]; then',
        *restored,
        '  _spice_shell_bin="${BASH:-bash}"',
        '  if ! command -v "$_spice_shell_bin" >/dev/null 2>&1; then',
        (
            '    printf "%s\\n" '
            '"spice shell hook: cannot resolve bash for agent-run reexec" >&2'
        ),
        "    exit 127",
        "  fi",
        "  if shopt -q login_shell; then",
        f'    exec {command} "$_spice_shell_bin" -lc "$BASH_EXECUTION_STRING"',
        ('    printf "%s\\n" "spice shell hook: failed to exec agent run" >&2'),
        "    exit 127",
        "  fi",
        f'  exec {command} "$_spice_shell_bin" -c "$BASH_EXECUTION_STRING"',
        '  printf "%s\\n" "spice shell hook: failed to exec agent run" >&2',
        "  exit 127",
        "fi",
        "case $- in",
        "  *i*) ;;",
        "  *)",
        (
            '    printf "%s\\n" '
            '"spice shell hook: cannot agent-run reexec noninteractive shell '
            'without an execution string" >&2'
        ),
        "    exit 127",
        "    ;;",
        "esac",
    ]


def shell_command(command: Sequence[str]) -> str:
    return " ".join(shell_quote(part) for part in command)


def shell_quote(value: str) -> str:
    return shlex.quote(value)
