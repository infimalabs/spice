"""Shell startup hooks for agent side-channel steering."""

from __future__ import annotations

import os
import re
import shlex
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path

from spice.errors import SpiceError
from spice.repocfg import agent_table, agent_wrapper_definitions_table

ZDOTDIR_ENV = "ZDOTDIR"
BASH_ENV_ENV = "BASH_ENV"
BASH_HOOK_NAME = "bash_env"
ZSH_HOOK_NAMES = (".zshenv", ".zprofile", ".zlogin")
AGENT_WRAPPERS_KEY = "wrappers"
DEFAULT_AGENT_WRAPPER_GROUP = "common"
BUILTIN_AGENT_WRAPPER_GROUPS = {
    DEFAULT_AGENT_WRAPPER_GROUP: {"rtk": ["run", "proxy", "grep", "find", "git"]},
}
SHELL_HOOK_PYTHON_ENV = "SPICE_SHELL_HOOK_PYTHON"  # env-policy: allow
SHELL_HOOK_REPO_ROOT_ENV = "SPICE_SHELL_HOOK_REPO_ROOT"  # env-policy: allow
SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV = (
    "SPICE_SHELL_HOOK_ORIGINAL_ZDOTDIR"  # env-policy: allow
)
SHELL_HOOK_ORIGINAL_BASH_ENV_ENV = (
    "SPICE_SHELL_HOOK_ORIGINAL_BASH_ENV"  # env-policy: allow
)
SHELL_HOOK_REEXEC_STAGE_ENV = "SPICE_SHELL_HOOK_REEXEC_STAGE"  # env-policy: allow
SHELL_HOOK_SURFACE_FILES = {
    BASH_HOOK_NAME: BASH_HOOK_NAME,
    "zshenv": ".zshenv",
    "zprofile": ".zprofile",
    "zlogin": ".zlogin",
}
SHELL_HOOK_SURFACES = tuple(SHELL_HOOK_SURFACE_FILES)
CONFIG_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
SHELL_FUNCTION_NAME_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")


def apply_shell_steering_environment(
    repo_root: Path,
    *,
    driver_state_dirname: str,
    base_env: Mapping[str, str],
) -> dict[str, str]:
    env = dict(base_env)
    env.update(shell_steering_runtime_environment(base_env=env, repo_root=repo_root))
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
    repo_root: Path | None = None,
) -> dict[str, str]:
    python = single_python_executable(python_command or (sys.executable,))
    env = {
        SHELL_HOOK_PYTHON_ENV: python,
        SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV: original_shell_startup_value(
            base_env,
            original_name=SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV,
            active_name=ZDOTDIR_ENV,
        ),
        SHELL_HOOK_ORIGINAL_BASH_ENV_ENV: original_shell_startup_value(
            base_env,
            original_name=SHELL_HOOK_ORIGINAL_BASH_ENV_ENV,
            active_name=BASH_ENV_ENV,
        ),
    }
    if repo_root is not None:
        env[SHELL_HOOK_REPO_ROOT_ENV] = str(repo_root.resolve())
    return env


def original_shell_startup_value(
    base_env: Mapping[str, str], *, original_name: str, active_name: str
) -> str:
    for name in (original_name, active_name):
        value = base_env.get(name, "")
        if value and not is_generated_shell_hook_path(value):
            return value
    return ""


def is_generated_shell_hook_path(value: str) -> bool:
    path = Path(value).expanduser()
    hook_dir = path.parent if path.name == BASH_HOOK_NAME else path
    parts = hook_dir.parts
    return (
        len(parts) >= 3
        and parts[-1] == "shellhooks"
        and parts[-2] == "agent"
        and parts[-3] == "spice"
    )


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
        wrapper_lines=wrapper_lines_for_environment(environment),
    )


def required_shell_hook_env(environment: Mapping[str, str], name: str) -> str:
    if name not in environment:
        raise SpiceError(f"spice shell hook: missing required {name}")
    return environment[name]


def agent_run_command_from_env(environment: Mapping[str, str]) -> str:
    python = required_shell_hook_env(environment, SHELL_HOOK_PYTHON_ENV).strip()
    if not python:
        raise SpiceError(f"spice shell hook: {SHELL_HOOK_PYTHON_ENV} must be non-empty")
    return f"{shell_quote(python)} -m spice agent run"


def single_python_executable(command: Sequence[str]) -> str:
    if len(command) != 1:
        raise SpiceError(
            f"spice shell hook: {SHELL_HOOK_PYTHON_ENV} must be one executable path"
        )
    python = str(command[0]).strip()
    if not python:
        raise SpiceError(f"spice shell hook: {SHELL_HOOK_PYTHON_ENV} must be non-empty")
    return python


def wrapper_lines_for_environment(environment: Mapping[str, str]) -> list[str]:
    raw_root = environment.get(SHELL_HOOK_REPO_ROOT_ENV, "").strip()
    if not raw_root:
        return []
    return render_agent_wrapper_lines(Path(raw_root).expanduser())


def render_agent_wrapper_lines(repo_root: Path) -> list[str]:
    agent_settings = agent_table(repo_root)
    definitions = {
        **BUILTIN_AGENT_WRAPPER_GROUPS,
        **agent_wrapper_definitions_table(repo_root),
    }
    if AGENT_WRAPPERS_KEY in agent_settings:
        ordered_groups = config_string_list(
            agent_settings.get(AGENT_WRAPPERS_KEY),
            label=f"tool.spice.agent.{AGENT_WRAPPERS_KEY}",
        )
    else:
        ordered_groups = [DEFAULT_AGENT_WRAPPER_GROUP]
    if not ordered_groups:
        return []
    lines: list[str] = []
    seen_selectors: dict[str, str] = {}
    for group_name in ordered_groups:
        require_config_name(
            group_name,
            label=f"tool.spice.agent.{AGENT_WRAPPERS_KEY} group",
        )
        raw_group = definitions.get(group_name)
        if not isinstance(raw_group, dict):
            raise SpiceError(
                f"spice shell hook: missing tool.spice.wrappers.{group_name}"
            )
        lines.extend(
            render_agent_wrapper_group_lines(
                group_name=group_name,
                group=raw_group,
                seen_selectors=seen_selectors,
            )
        )
    return lines


def render_agent_wrapper_group_lines(
    *,
    group_name: str,
    group: Mapping[str, object],
    seen_selectors: dict[str, str],
) -> list[str]:
    lines: list[str] = []
    for raw_wrapper, raw_entry in group.items():
        wrapper = str(raw_wrapper).strip()
        if isinstance(raw_entry, Mapping):
            lines.extend(
                render_agent_direct_wrapper_lines(
                    group_name=group_name,
                    selector=wrapper,
                    entry=raw_entry,
                    seen_selectors=seen_selectors,
                )
            )
            continue
        require_shell_function_name(
            wrapper,
            label=f"tool.spice.wrappers.{group_name} wrapper",
        )
        if not isinstance(raw_entry, list):
            raise SpiceError(
                "spice shell hook: "
                f"tool.spice.wrappers.{group_name}.{wrapper} must be a list or table"
            )
        selectors = config_string_list(
            raw_entry,
            label=f"tool.spice.wrappers.{group_name}.{wrapper}",
        )
        if not selectors:
            raise SpiceError(
                "spice shell hook: "
                f"tool.spice.wrappers.{group_name}.{wrapper} has no commands"
            )
        for selector in selectors:
            lines.extend(
                render_agent_wrapper_selector_lines(
                    group_name=group_name,
                    wrapper=wrapper,
                    selector=selector,
                    seen_selectors=seen_selectors,
                )
            )
    return lines


def render_agent_direct_wrapper_lines(
    *,
    group_name: str,
    selector: str,
    entry: Mapping[str, object],
    seen_selectors: dict[str, str],
) -> list[str]:
    config_path = f"tool.spice.wrappers.{group_name}.{selector}"
    require_shell_function_name(selector, label=f"{config_path} command")
    extra = sorted(set(entry) - {"command"})
    if extra:
        raise SpiceError(
            f"spice shell hook: {config_path} has unsupported keys: {', '.join(extra)}"
        )
    command_words = command_words_from_config(
        entry.get("command"),
        label=f"{config_path}.command",
    )
    if selector == command_words[0]:
        raise SpiceError(
            "spice shell hook: wrapper "
            f"{selector!r} cannot intercept itself in {config_path}.command"
        )
    record_agent_wrapper_selector(
        selector,
        config_path,
        seen_selectors=seen_selectors,
    )
    command = " ".join(shell_command_word(word) for word in command_words)
    return [
        "",
        f"{selector}() {{",
        f'  {command} "$@"',
        "}",
    ]


def render_agent_wrapper_selector_lines(
    *,
    group_name: str,
    wrapper: str,
    selector: str,
    seen_selectors: dict[str, str],
) -> list[str]:
    config_path = f"tool.spice.wrappers.{group_name}.{wrapper}"
    if "/" in selector:
        raise SpiceError(
            "spice shell hook: path selector "
            f"{selector!r} in {config_path} requires the redirector stage"
        )
    require_shell_function_name(
        selector,
        label=f"{config_path} command",
    )
    if selector == wrapper:
        raise SpiceError(
            "spice shell hook: wrapper "
            f"{wrapper!r} cannot intercept itself in {config_path}"
        )
    record_agent_wrapper_selector(
        selector,
        config_path,
        seen_selectors=seen_selectors,
    )
    return [
        "",
        f"{selector}() {{",
        f'  {shell_quote(wrapper)} {shell_quote(selector)} "$@"',
        "}",
    ]


def record_agent_wrapper_selector(
    selector: str, config_path: str, *, seen_selectors: dict[str, str]
) -> None:
    previous = seen_selectors.get(selector)
    if previous is not None:
        raise SpiceError(
            "spice shell hook: command "
            f"{selector!r} is configured by both {previous} and {config_path}"
        )
    seen_selectors[selector] = config_path


def command_words_from_config(raw: object, *, label: str) -> list[str]:
    words = config_string_list(raw, label=label)
    for word in words:
        if "/" in word:
            raise SpiceError(
                "spice shell hook: path wrapper command "
                f"{word!r} in {label} requires the redirector stage"
            )
    return words


def shell_command_word(word: str) -> str:
    match = re.fullmatch(r"\$([A-Za-z_][A-Za-z0-9_]*)", word)
    if match:
        return '"$' + match.group(1) + '"'
    return shell_quote(word)


def require_shell_function_name(value: str, *, label: str) -> None:
    if SHELL_FUNCTION_NAME_RE.fullmatch(value):
        return
    raise SpiceError(f"spice shell hook: {label} {value!r} is not a shell function")


def require_config_name(value: str, *, label: str) -> None:
    if CONFIG_NAME_RE.fullmatch(value):
        return
    raise SpiceError(f"spice shell hook: {label} {value!r} is not a config name")


def config_string_list(raw: object, *, label: str) -> list[str]:
    if not isinstance(raw, list):
        raise SpiceError(f"spice shell hook: {label} must be a list")
    values: list[str] = []
    for item in raw:
        if not isinstance(item, str):
            raise SpiceError(f"spice shell hook: {label} entries must be strings")
        value = item.strip()
        if not value:
            raise SpiceError(f"spice shell hook: {label} entries must be non-empty")
        if value in values:
            raise SpiceError(f"spice shell hook: {label} repeats entry {value!r}")
        values.append(value)
    return values


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
        else user_home_path(environment)
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
    return user_home_path(base_env)


def user_home_path(base_env: Mapping[str, str]) -> Path:
    if home := base_env.get("HOME"):
        return Path(home).expanduser()
    return Path.home()


def restore_env_line(name: str, value: str) -> str:
    if value:
        return f"export {name}={shell_quote(value)}"
    return f"unset {name}"


def render_shell_steering_hook(
    *,
    agent_run_command: str,
    restore_lines: Sequence[str],
    real_source_path: Path | None,
    self_path: Path,
    wrapper_lines: Sequence[str] = (),
) -> str:
    lines = [
        "# Generated by spice; do not edit.",
        *agent_run_reexec_lines(
            agent_run_command=agent_run_command,
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
    lines.extend(wrapper_lines)
    return "\n".join(lines) + "\n"


def agent_run_reexec_lines(*, agent_run_command: str) -> list[str]:
    return [
        f'if [ -z "${{{SHELL_HOOK_REEXEC_STAGE_ENV}-}}" ]; then',
        '  if [ -n "${ZSH_EXECUTION_STRING-}" ]; then',
        f"    export {SHELL_HOOK_REEXEC_STAGE_ENV}=1",
        '    _spice_shell_bin="${SHELL:-${ZSH_NAME:-zsh}}"',
        '    case "$_spice_shell_bin" in',
        "      *zsh) ;;",
        '      *) _spice_shell_bin="${ZSH_NAME:-zsh}" ;;',
        "    esac",
        '    if ! command -v "$_spice_shell_bin" >/dev/null 2>&1; then',
        (
            '      printf "%s\\n" '
            '"spice shell hook: cannot resolve zsh for agent-run reexec" >&2'
        ),
        "      exit 127",
        "    fi",
        "    if [[ -o login ]]; then",
        (
            f'      exec {agent_run_command} --preserve-shell-hook-env -- "$_spice_shell_bin" '
            '-lc "$ZSH_EXECUTION_STRING"'
        ),
        ('      printf "%s\\n" "spice shell hook: failed to exec agent run" >&2'),
        "      exit 127",
        "    fi",
        (
            f'    exec {agent_run_command} --preserve-shell-hook-env -- "$_spice_shell_bin" '
            '-c "$ZSH_EXECUTION_STRING"'
        ),
        '    printf "%s\\n" "spice shell hook: failed to exec agent run" >&2',
        "    exit 127",
        "  fi",
        '  if [ -n "${BASH_EXECUTION_STRING-}" ]; then',
        f"    export {SHELL_HOOK_REEXEC_STAGE_ENV}=1",
        '    _spice_shell_bin="${BASH:-bash}"',
        '    if ! command -v "$_spice_shell_bin" >/dev/null 2>&1; then',
        (
            '      printf "%s\\n" '
            '"spice shell hook: cannot resolve bash for agent-run reexec" >&2'
        ),
        "      exit 127",
        "    fi",
        "    if shopt -q login_shell; then",
        (
            f'      exec {agent_run_command} --preserve-shell-hook-env -- "$_spice_shell_bin" '
            '-lc "$BASH_EXECUTION_STRING"'
        ),
        ('      printf "%s\\n" "spice shell hook: failed to exec agent run" >&2'),
        "      exit 127",
        "    fi",
        (
            f'    exec {agent_run_command} --preserve-shell-hook-env -- "$_spice_shell_bin" '
            '-c "$BASH_EXECUTION_STRING"'
        ),
        '    printf "%s\\n" "spice shell hook: failed to exec agent run" >&2',
        "    exit 127",
        "  fi",
        "  case $- in",
        "    *i*) ;;",
        "    *)",
        (
            '      printf "%s\\n" '
            '"spice shell hook: cannot agent-run reexec noninteractive shell '
            'without an execution string" >&2'
        ),
        "      exit 127",
        "      ;;",
        "  esac",
        "fi",
    ]


def shell_command(command: Sequence[str]) -> str:
    return " ".join(shell_quote(part) for part in command)


def shell_quote(value: str) -> str:
    return shlex.quote(value)
