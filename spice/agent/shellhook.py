"""Shell startup hooks for agent side-channel steering."""

from __future__ import annotations

import os
import re
import shlex
import warnings
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import NamedTuple

from spice.config.values import configured_agent_driver, configured_rtk_executable
from spice.config.layers import (
    SYSTEM_SOURCE,
    contextualize_config_error,
    effective_table,
    enabled_registry_entries,
    load_config,
)
from spice.config.trust import require_repository_config_approval
from spice.errors import SpiceError
from spice.extensions import (
    SPICE_WRAPPER_ENTRY_POINT_GROUP,
    SpiceExtensionEntryPoint,
    extension_entry_points,
)
from spice.paths import shared_state_root
from spice.scopes import (
    SCOPES_KEY,
    WRAPPER_ROUTE_SCOPES,
    WRAPPER_SCOPES,
    ScopeContext,
    ScopeSelector,
)
from spice.tasks import config as task_config

ZDOTDIR_ENV = "ZDOTDIR"
BASH_ENV_ENV = "BASH_ENV"
HISTFILE_ENV = "HISTFILE"
ZSH_COMPDUMP_ENV = "ZSH_COMPDUMP"
TASKRC_ENV = "TASKRC"
BASH_HOOK_NAME = "bash_env"
ZSH_HOOK_NAMES = (".zshenv", ".zprofile", ".zshrc", ".zlogin")
SHELL_HOOK_DIR_NAME = "shellhooks"
STATIC_SHELL_HOOK_DIR_NAME = "staticshellhooks"
AGENT_WRAPPERS_KEY = "wrappers"
WRAPPER_ENTRY_POINT_GROUP = SPICE_WRAPPER_ENTRY_POINT_GROUP
SHELL_HOOK_REPO_ROOT_ENV = "SPICE_SHELL_HOOK_REPO_ROOT"  # env-policy: allow
SHELL_HOOK_WRAPPERS_ENV = "SPICE_SHELL_HOOK_WRAPPERS"  # env-policy: allow
SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV = (
    "SPICE_SHELL_HOOK_ORIGINAL_ZDOTDIR"  # env-policy: allow
)
SHELL_HOOK_ORIGINAL_BASH_ENV_ENV = (
    "SPICE_SHELL_HOOK_ORIGINAL_BASH_ENV"  # env-policy: allow
)
SHELL_HOOK_ORIGINAL_HISTFILE_ENV = (
    "SPICE_SHELL_HOOK_ORIGINAL_HISTFILE"  # env-policy: allow
)
SHELL_HOOK_SURFACE_FILES = {
    BASH_HOOK_NAME: BASH_HOOK_NAME,
    "zshenv": ".zshenv",
    "zprofile": ".zprofile",
    "zshrc": ".zshrc",
    "zlogin": ".zlogin",
}
SHELL_HOOK_SURFACES = tuple(SHELL_HOOK_SURFACE_FILES)
CONFIG_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
SHELL_FUNCTION_NAME_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_-]*\Z")
RTK_CANONICAL_EXECUTABLE = "rtk"
PROJECT_PYTHON_COMMANDS = ("python", "python3")
UV_PYTHON_COMMAND = ("uv", "run", "python")


class _SelectedAgentWrapperGroup(NamedTuple):
    name: str
    group: Mapping[str, object]
    from_extension: bool


def apply_shell_steering_environment(
    repo_root: Path,
    *,
    base_env: Mapping[str, str],
) -> dict[str, str]:
    env = dict(base_env)
    env.update(shell_steering_runtime_environment(base_env=env, repo_root=repo_root))
    env.update(taskwarrior_runtime_environment(base_env=env, repo_root=repo_root))
    env[SHELL_HOOK_WRAPPERS_ENV] = "\n".join(
        render_shell_runtime_wrapper_lines(repo_root)
    )
    hook_dir = packaged_shell_steering_hook_dir()
    env[ZDOTDIR_ENV] = str(hook_dir)
    env[BASH_ENV_ENV] = str(hook_dir / BASH_HOOK_NAME)
    if ZSH_COMPDUMP_ENV not in base_env:
        original_zdotdir = original_shell_startup_value(
            base_env,
            original_name=SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV,
            active_name=ZDOTDIR_ENV,
        )
        dump_base = (
            Path(original_zdotdir).expanduser()
            if original_zdotdir
            else user_home_path(base_env)
        )
        env[ZSH_COMPDUMP_ENV] = str(dump_base / ".zcompdump")
    return env


def taskwarrior_runtime_environment(
    *,
    base_env: Mapping[str, str],
    repo_root: Path,
) -> dict[str, str]:
    """Bind native Taskwarrior to the task backend selected for this agent.

    ``TASKRC`` is Taskwarrior's own configuration seam.  The generated Spice
    taskrc carries the matching ``data.location``, so exporting this one native
    variable keeps every ``task`` command and argument untouched while the
    binding naturally survives descendant shells.
    """
    selector = base_env.get(task_config.TASK_BACKEND_ENV, "").strip()
    if selector:
        backend = Path(selector).expanduser()
        if not backend.is_absolute():
            raise SpiceError(
                f"{task_config.TASK_BACKEND_ENV} requires an absolute path"
            )
        backend = backend.resolve()
    else:
        try:
            backend = shared_state_root(repo_root)
        except SpiceError:
            # Some library-level shell-hook consumers intentionally operate on
            # a product-shaped directory rather than an activated worktree.
            return {}
    return {
        TASKRC_ENV: str(
            task_config.materialize_task_backend(backend, source_root=repo_root)
        )
    }


def packaged_shell_steering_hook_dir() -> Path:
    hook_dir = Path(__file__).resolve().parent / SHELL_HOOK_DIR_NAME
    static_hook_dir = packaged_shell_steering_static_hook_dir()
    missing = [
        str(path.relative_to(Path(__file__).resolve().parent))
        for path in (
            *((hook_dir / name) for name in (*ZSH_HOOK_NAMES, BASH_HOOK_NAME)),
            *((static_hook_dir / name) for name in (*ZSH_HOOK_NAMES, BASH_HOOK_NAME)),
        )
        if not path.is_file()
    ]
    if missing:
        raise SpiceError(
            "spice shell hook: packaged hook files missing: " + ", ".join(missing)
        )
    return hook_dir


def packaged_shell_steering_static_hook_dir() -> Path:
    return Path(__file__).resolve().parent / STATIC_SHELL_HOOK_DIR_NAME


def shell_steering_runtime_environment(
    *,
    base_env: Mapping[str, str],
    repo_root: Path | None = None,
) -> dict[str, str]:
    original_zdotdir = original_shell_startup_value(
        base_env,
        original_name=SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV,
        active_name=ZDOTDIR_ENV,
    )
    original_bash_env = original_shell_startup_value(
        base_env,
        original_name=SHELL_HOOK_ORIGINAL_BASH_ENV_ENV,
        active_name=BASH_ENV_ENV,
    )
    env = {
        SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV: original_zdotdir,
        SHELL_HOOK_ORIGINAL_BASH_ENV_ENV: original_bash_env,
        SHELL_HOOK_ORIGINAL_HISTFILE_ENV: original_zsh_history_value(
            base_env, original_zdotdir=original_zdotdir
        ),
    }
    if repo_root is not None:
        resolved_root = repo_root.resolve()
        env[SHELL_HOOK_REPO_ROOT_ENV] = str(resolved_root)
    return env


def original_zsh_history_value(
    base_env: Mapping[str, str], *, original_zdotdir: str
) -> str:
    for name in (SHELL_HOOK_ORIGINAL_HISTFILE_ENV, HISTFILE_ENV):
        value = base_env.get(name, "")
        if value and not is_generated_shell_hook_history_path(value):
            return value
    history_base = (
        Path(original_zdotdir).expanduser()
        if original_zdotdir
        else user_home_path(base_env)
    )
    return str(history_base / ".zsh_history")


def original_shell_startup_value(
    base_env: Mapping[str, str], *, original_name: str, active_name: str
) -> str:
    for name in (original_name, active_name):
        value = base_env.get(name, "")
        if value and not is_generated_shell_hook_path(value):
            return value
    return ""


def is_generated_shell_hook_history_path(value: str) -> bool:
    path = Path(value).expanduser()
    return path.name == ".zsh_history" and is_generated_shell_hook_path(
        str(path.parent)
    )


def is_generated_shell_hook_path(value: str) -> bool:
    path = Path(value).expanduser()
    hook_dir = path.parent if path.name == BASH_HOOK_NAME else path
    parts = hook_dir.parts
    return (
        len(parts) >= 3
        and parts[-1] in {SHELL_HOOK_DIR_NAME, STATIC_SHELL_HOOK_DIR_NAME}
        and parts[-2] == "agent"
        and parts[-3] == "spice"
    )


def render_agent_wrapper_lines(repo_root: Path) -> list[str]:
    try:
        return _render_agent_wrapper_lines(repo_root)
    except SpiceError as exc:
        raise contextualize_config_error(repo_root, exc, "wrappers") from exc


def render_shell_runtime_wrapper_lines(repo_root: Path) -> list[str]:
    return [
        *render_agent_wrapper_lines(repo_root),
        *render_project_python_wrapper_lines(repo_root),
    ]


def render_project_python_wrapper_lines(repo_root: Path) -> list[str]:
    resolved_root = repo_root.resolve()
    if not project_routes_python(resolved_root):
        return []
    project_pattern = shell_quote(str(resolved_root) + os.sep) + "*"
    uv_command = " ".join(shell_command_word(word) for word in UV_PYTHON_COMMAND)
    lines: list[str] = []
    for command_name in PROJECT_PYTHON_COMMANDS:
        lines.extend(
            [
                "",
                f"{command_name}() {{",
                "  local _spice_python_cwd",
                '  _spice_python_cwd="$(pwd -P)"',
                '  case "$_spice_python_cwd/" in',
                f'    {project_pattern}) command {uv_command} "$@" ;;',
                f'    *) command {command_name} "$@" ;;',
                "  esac",
                "}",
            ]
        )
    return lines


def project_routes_python(repo_root: Path | None) -> bool:
    return repo_root is not None and (repo_root / "pyproject.toml").is_file()


def _render_agent_wrapper_lines(repo_root: Path) -> list[str]:
    driver_name = active_wrapper_driver_name(repo_root)
    rtk_executable = configured_rtk_executable(repo_root)
    require_repository_config_approval(
        repo_root,
        ("rtk", "executable"),
        command=shlex.join((rtk_executable,)),
    )
    lines: list[str] = []
    for selected in _selected_agent_wrapper_groups(repo_root):
        lines.extend(
            render_agent_wrapper_group_lines(
                group_name=selected.name,
                group=selected.group,
                driver_name=driver_name,
                rtk_executable=rtk_executable,
            )
        )
    return lines


def rtk_rewrite_yield_selectors(repo_root: Path) -> frozenset[str]:
    """Wrapper words the agent-run RTK rewrite must leave to the shell.

    A selected direct wrapper whose argv head is not RTK claims its selector
    word regardless of configuration source: the pre-shell rewrite substitutes
    command text before any wrapper function exists, so an RTK claim on such a
    word would shadow the selected expansion. Configuration errors yield the
    empty set here; shell-hook rendering surfaces them loudly.
    """
    try:
        return _rtk_rewrite_yield_selectors(repo_root)
    except SpiceError:
        return frozenset()


def _rtk_rewrite_yield_selectors(repo_root: Path) -> frozenset[str]:
    context = ScopeContext(driver=active_wrapper_driver_name(repo_root))
    rtk_words = {RTK_CANONICAL_EXECUTABLE, configured_rtk_executable(repo_root)}
    selectors: set[str] = set()
    for selected in _selected_agent_wrapper_groups(repo_root):
        group_name = selected.name
        raw_group = selected.group
        if not WRAPPER_SCOPES.parse(raw_group.get(SCOPES_KEY)).matches(context):
            continue
        for raw_wrapper, raw_entry in enabled_registry_entries(
            raw_group, "wrappers", "*"
        ).items():
            wrapper = str(raw_wrapper).strip()
            if wrapper == SCOPES_KEY or not isinstance(raw_entry, Mapping):
                continue
            if not WRAPPER_SCOPES.parse(raw_entry.get(SCOPES_KEY)).matches(context):
                continue
            command_words = command_words_from_config(
                raw_entry.get("argv"),
                label=f"wrappers.{group_name}.{wrapper}.argv",
            )
            if command_words[0] in rtk_words:
                continue
            selectors.add(wrapper)
    return frozenset(selectors)


def _selected_agent_wrapper_groups(
    repo_root: Path,
) -> tuple[_SelectedAgentWrapperGroup, ...]:
    """Resolve the ordered wrapper-group universe for every shell consumer."""
    agent_settings = effective_table(repo_root, "agent")
    if AGENT_WRAPPERS_KEY not in agent_settings:
        raise SpiceError(
            f"spice shell hook: effective agent configuration requires "
            f"{AGENT_WRAPPERS_KEY}"
        )
    ordered_groups = config_string_list(
        agent_settings.get(AGENT_WRAPPERS_KEY),
        label=f"agent.{AGENT_WRAPPERS_KEY}",
    )
    definitions, configured_sources = configured_agent_wrapper_definitions(repo_root)
    extension_entries = entry_point_agent_wrapper_entries(
        configured_sources=configured_sources
    )
    enabled_definitions = enabled_registry_entries(definitions, "wrappers")
    selected: list[_SelectedAgentWrapperGroup] = []
    for group_name in ordered_groups:
        require_config_name(
            group_name,
            label=f"agent.{AGENT_WRAPPERS_KEY} group",
        )
        raw_group = enabled_definitions.get(group_name)
        if raw_group is None and group_name in definitions:
            _warn_dropped_packaged_wrappers(repo_root, group_name, {})
            continue
        from_extension = raw_group is None and group_name in extension_entries
        if from_extension:
            raw_group = entry_point_wrapper_group_from_entry(
                extension_entries[group_name]
            )
        if not isinstance(raw_group, Mapping):
            raise SpiceError(f"spice shell hook: missing wrappers.{group_name}")
        if not from_extension:
            require_repository_config_approval(
                repo_root,
                ("wrappers", group_name),
                command=_wrapper_group_command_summary(group_name, raw_group),
            )
            _warn_dropped_packaged_wrappers(repo_root, group_name, raw_group)
        selected.append(
            _SelectedAgentWrapperGroup(
                name=group_name,
                group=raw_group,
                from_extension=from_extension,
            )
        )
    return tuple(selected)


def _warn_dropped_packaged_wrappers(
    repo_root: Path,
    group_name: str,
    replacement: Mapping[str, object],
) -> None:
    loaded = load_config(repo_root)
    source = loaded.source_for(("wrappers", group_name))
    if source is None or source.name == SYSTEM_SOURCE:
        return
    packaged_wrappers = loaded.layer(SYSTEM_SOURCE).values.get("wrappers")
    if not isinstance(packaged_wrappers, Mapping):
        return
    packaged_group = packaged_wrappers.get(group_name)
    if not isinstance(packaged_group, Mapping):
        return
    dropped = sorted(
        _enabled_wrapper_names(packaged_group) - _enabled_wrapper_names(replacement)
    )
    if not dropped:
        return
    warnings.warn(
        "spice shell hook: "
        f"wrappers.{group_name} from {source.name} replaces the packaged "
        "wrapper group and drops packaged wrappers: " + ", ".join(dropped),
        stacklevel=3,
    )


def _enabled_wrapper_names(group: Mapping[str, object]) -> set[str]:
    return set(enabled_registry_entries(group, "wrappers", "*")) - {SCOPES_KEY}


def _wrapper_group_command_summary(
    group_name: str,
    group: Mapping[str, object],
) -> str:
    commands: list[str] = []
    for raw_wrapper, raw_entry in group.items():
        wrapper = str(raw_wrapper)
        if wrapper == SCOPES_KEY:
            continue
        if isinstance(raw_entry, Mapping):
            _append_wrapper_argv(commands, raw_entry.get("argv"))
            routes = raw_entry.get("match")
            if isinstance(routes, list):
                for route in routes:
                    if isinstance(route, Mapping):
                        _append_wrapper_argv(commands, route.get("argv"))
            continue
        if isinstance(raw_entry, list) and all(
            isinstance(selector, str) for selector in raw_entry
        ):
            commands.extend(shlex.join((wrapper, selector)) for selector in raw_entry)
    listed = "; ".join(dict.fromkeys(commands))
    return listed or f"wrapper group {group_name}"


def _append_wrapper_argv(commands: list[str], raw: object) -> None:
    if isinstance(raw, list) and raw and all(isinstance(word, str) for word in raw):
        commands.append(shlex.join(raw))


def configured_agent_wrapper_definitions(
    repo_root: Path,
) -> tuple[dict[str, object], dict[str, str]]:
    definitions: dict[str, object] = dict(effective_table(repo_root, "wrappers"))
    sources: dict[str, str] = {name: f"wrappers.{name}" for name in definitions}
    return definitions, sources


def entry_point_agent_wrapper_entries(
    *, configured_sources: Mapping[str, str]
) -> dict[str, SpiceExtensionEntryPoint]:
    entries: dict[str, SpiceExtensionEntryPoint] = {}
    for entry in extension_entry_points(WRAPPER_ENTRY_POINT_GROUP):
        name = entry.name
        require_config_name(name, label=f"{WRAPPER_ENTRY_POINT_GROUP} entry point")
        source = entry_point_wrapper_source(entry)
        previous = configured_sources.get(name)
        if previous is not None:
            raise wrapper_shadowing_error(name, previous, source)
        entries[name] = entry
    return entries


def entry_point_wrapper_group_from_entry(entry: SpiceExtensionEntryPoint) -> object:
    source = entry_point_wrapper_source(entry)
    try:
        loaded = entry.load()
        raw = loaded() if callable(loaded) else loaded
    except Exception as exc:
        raise SpiceError(f"spice shell hook: failed to load {source}: {exc}") from exc
    return entry_point_wrapper_group(entry.name, raw, source=source)


def entry_point_wrapper_source(entry: SpiceExtensionEntryPoint) -> str:
    return f"{WRAPPER_ENTRY_POINT_GROUP} entry point {entry.name}"


def entry_point_wrapper_group(name: str, raw: object, *, source: str) -> object:
    if not isinstance(raw, Mapping):
        raise SpiceError(f"spice shell hook: {source} must load a wrapper table")
    if "argv" in raw:
        return {name: dict(raw)}
    return dict(raw)


def wrapper_shadowing_error(name: str, first: str, second: str) -> SpiceError:
    return SpiceError(
        "spice shell hook: wrapper group "
        f"{name!r} is configured by both {first} and {second}"
    )


def render_agent_wrapper_group_lines(
    *,
    group_name: str,
    group: Mapping[str, object],
    driver_name: str,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
) -> list[str]:
    context = ScopeContext(driver=driver_name)
    group_scope = WRAPPER_SCOPES.parse(group.get(SCOPES_KEY))
    if not group_scope.matches(context):
        return []
    lines: list[str] = []
    for raw_wrapper, raw_entry in enabled_registry_entries(
        group, "wrappers", "*"
    ).items():
        wrapper = str(raw_wrapper).strip()
        if wrapper == SCOPES_KEY:
            continue
        if isinstance(raw_entry, Mapping):
            lines.extend(
                render_agent_direct_wrapper_lines(
                    group_name=group_name,
                    selector=wrapper,
                    entry=raw_entry,
                    driver_name=driver_name,
                    rtk_executable=rtk_executable,
                )
            )
            continue
        require_shell_function_name(
            wrapper,
            label=f"wrappers.{group_name} wrapper",
        )
        if not isinstance(raw_entry, list):
            raise SpiceError(
                "spice shell hook: "
                f"wrappers.{group_name}.{wrapper} must be a list or table"
            )
        selectors = config_string_list(
            raw_entry,
            label=f"wrappers.{group_name}.{wrapper}",
        )
        if not selectors:
            raise SpiceError(
                f"spice shell hook: wrappers.{group_name}.{wrapper} has no commands"
            )
        for selector in selectors:
            lines.extend(
                render_agent_wrapper_selector_lines(
                    group_name=group_name,
                    wrapper=wrapper,
                    selector=selector,
                )
            )
    return lines


def render_agent_direct_wrapper_lines(
    *,
    group_name: str,
    selector: str,
    entry: Mapping[str, object],
    driver_name: str,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
) -> list[str]:
    config_path = f"wrappers.{group_name}.{selector}"
    require_shell_function_name(selector, label=f"{config_path} command")
    extra = sorted(set(entry) - {"argv", SCOPES_KEY, "match"})
    if extra:
        raise SpiceError(
            f"spice shell hook: {config_path} has unsupported keys: {', '.join(extra)}"
        )
    command_words = command_words_from_config(
        entry.get("argv"),
        label=f"{config_path}.argv",
    )
    scope = WRAPPER_SCOPES.parse(entry.get(SCOPES_KEY))
    routes = agent_wrapper_match_routes(entry.get("match"), config_path=config_path)
    if selector == RTK_CANONICAL_EXECUTABLE:
        command_words = resolved_rtk_command_words(command_words, rtk_executable)
        routes = tuple(
            route._replace(
                argv=tuple(resolved_rtk_command_words(route.argv, rtk_executable))
            )
            for route in routes
        )
    context = ScopeContext(driver=driver_name)
    active_routes = tuple(route for route in routes if route.scope.matches(context))
    if not routes and selector == command_words[0]:
        raise SpiceError(
            "spice shell hook: wrapper "
            f"{selector!r} cannot intercept itself in {config_path}.argv"
        )
    if not scope.matches(context):
        return []
    if routes and not active_routes and selector == command_words[0]:
        return []
    if active_routes:
        return render_agent_match_wrapper_lines(selector, command_words, active_routes)
    command = " ".join(shell_command_word(word) for word in command_words)
    return [
        "",
        f"{selector}() {{",
        f'  {command} "$@"',
        "}",
    ]


def resolved_rtk_command_words(
    command_words: Sequence[str], rtk_executable: str
) -> list[str]:
    words = list(command_words)
    if words[:1] == [RTK_CANONICAL_EXECUTABLE]:
        words[0] = rtk_executable
    return words


class WrapperMatchRoute(NamedTuple):
    head: str | None
    flags: tuple[str, ...]
    keep: tuple[str, ...]
    search_operands: bool
    argv: tuple[str, ...]
    scope: ScopeSelector


def render_agent_match_wrapper_lines(
    selector: str,
    command_words: Sequence[str],
    routes: Sequence[WrapperMatchRoute],
) -> list[str]:
    lines = ["", f"{selector}() {{"]
    for route in routes:
        lines.extend(match_route_guard_lines(route))
    command = " ".join(shell_command_word(word) for word in command_words)
    lines.append(f'  command {command} "$@"')
    lines.append("}")
    return lines


def match_route_guard_lines(route: WrapperMatchRoute) -> list[str]:
    argv = " ".join(shell_command_word(word) for word in route.argv)
    if route.search_operands:
        assert route.head == "grep"
        return grep_search_operand_route_guard_lines(argv)
    if not route.flags:
        assert route.head is not None
        return [
            f'  if [ "${{1-}}" = {shell_quote(route.head)} ]; then',
            "    shift",
            f'    command {argv} "$@"',
            "    return",
            "  fi",
        ]
    pattern = "|".join(match_route_pattern(flag) for flag in route.flags)
    keep = "|".join(match_route_pattern(word) for word in route.keep)
    scan = [
        'for _spice_word in "$@"; do',
        '  case "$_spice_word" in',
        *([f"    {keep}) ;;"] if keep else []),
        f"    {pattern})",
        *(["      shift"] if route.head is not None else []),
        f'      command {argv} "$@"',
        "      return",
        "      ;;",
        "  esac",
        "done",
    ]
    if route.head is None:
        return ["  " + line for line in scan]
    return [
        f'  if [ "${{1-}}" = {shell_quote(route.head)} ]; then',
        *("    " + line for line in scan),
        "  fi",
    ]


def grep_search_operand_route_guard_lines(argv: str) -> list[str]:
    """Route grep only when argv contains a file/directory search operand."""
    return [
        '  if [ "${1-}" = grep ]; then',
        *grep_search_operand_scan_lines(),
        *grep_search_operand_dispatch_lines(argv),
        "  fi",
    ]


def grep_search_operand_scan_lines() -> list[str]:
    """Render grep argv classification, including rewritten dash-patterns."""
    return [
        "    local _spice_grep_seen_head=",
        "    local _spice_grep_seen_pattern=",
        "    local _spice_grep_expect=",
        "    local _spice_grep_positional=",
        "    local _spice_grep_has_operand=",
        "    local _spice_grep_word_index=0",
        "    local _spice_grep_dash_pattern_index=",
        '    for _spice_word in "$@"; do',
        '      if [ -z "$_spice_grep_seen_head" ]; then',
        "        _spice_grep_seen_head=1",
        "        continue",
        "      fi",
        "      _spice_grep_word_index=$((_spice_grep_word_index + 1))",
        '      if [ -n "$_spice_grep_expect" ]; then',
        '        if [ "$_spice_grep_expect" = pattern ]; then',
        "          _spice_grep_seen_pattern=1",
        "        fi",
        "        _spice_grep_expect=",
        "        continue",
        "      fi",
        '      if [ -n "$_spice_grep_positional" ]; then',
        '        if [ -n "$_spice_grep_seen_pattern" ]; then',
        "          _spice_grep_has_operand=1",
        "          break",
        "        fi",
        "        _spice_grep_seen_pattern=1",
        "        continue",
        "      fi",
        '      case "$_spice_word" in',
        "        --)",
        "          _spice_grep_positional=1",
        "          ;;",
        "        -e|--regexp|-f|--file)",
        "          _spice_grep_expect=pattern",
        "          ;;",
        (
            "        -A|-B|-C|-D|-d|-m|--after-context|--before-context|"
            "--binary-files|--devices|--directories|--exclude|"
            "--exclude-from|--exclude-dir|--group-separator|--include|--label|"
            "--max-count)"
        ),
        "          _spice_grep_expect=value",
        "          ;;",
        "        --regexp=*|--file=*|-e?*|-f?*)",
        "          _spice_grep_seen_pattern=1",
        "          ;;",
        "        -A?*|-B?*|-C?*|-D?*|-d?*|-m?*|--*=*) ;;",
        (
            "        --basic-regexp|--binary|--byte-offset|--bz2decompress|"
            "--color|--colour|--context|--count|--decompress|"
            "--dereference-recursive|--extended-regexp|--files-with-matches|"
            "--files-without-match|--fixed-strings|--help|--ignore-case|"
            "--initial-tab|--invert-match|--line-buffered|--line-number|"
            "--line-regexp|--lzmadecompress|--mmap|--no-filename|"
            "--no-group-separator|--no-ignore-case|--no-messages|--null|"
            "--null-data|--only-matching|--perl-regexp|--quiet|--recursive|"
            "--silent|--text|--unix-byte-offsets|--version|--with-filename|"
            "--word-regexp) ;;"
        ),
        "        --*)",
        '          if [ -z "$_spice_grep_seen_pattern" ]; then',
        "            _spice_grep_seen_pattern=1",
        ('            _spice_grep_dash_pattern_index="$_spice_grep_word_index"'),
        "          fi",
        "          ;;",
        "        -*) ;;",
        "        *)",
        '          if [ -n "$_spice_grep_seen_pattern" ]; then',
        "            _spice_grep_has_operand=1",
        "            break",
        "          fi",
        "          _spice_grep_seen_pattern=1",
        "          ;;",
        "      esac",
        "    done",
    ]


def grep_search_operand_dispatch_lines(argv: str) -> list[str]:
    """Render a routed grep call, restoring -e lost by RTK's rg rewrite."""
    return [
        (
            '    if [ -n "$_spice_grep_has_operand" ] || '
            '[ -n "$_spice_grep_dash_pattern_index" ]; then'
        ),
        "      shift",
        '      if [ -n "$_spice_grep_dash_pattern_index" ]; then',
        "        local -a _spice_grep_route_args=()",
        "        local _spice_grep_rebuild_index=0",
        '        for _spice_word in "$@"; do',
        ("          _spice_grep_rebuild_index=$((_spice_grep_rebuild_index + 1))"),
        (
            '          if [ "$_spice_grep_rebuild_index" -eq '
            '"$_spice_grep_dash_pattern_index" ]; then'
        ),
        "            _spice_grep_route_args+=(-e)",
        "          fi",
        '          _spice_grep_route_args+=("$_spice_word")',
        "        done",
        f'        command {argv} "${{_spice_grep_route_args[@]}}"',
        "      else",
        f'        command {argv} "$@"',
        "      fi",
        "      return",
        "    fi",
    ]


def agent_wrapper_match_routes(
    raw: object, *, config_path: str
) -> tuple[WrapperMatchRoute, ...]:
    if raw is None:
        return ()
    label = f"{config_path}.match"
    if not isinstance(raw, list) or not raw:
        raise SpiceError(f"spice shell hook: {label} must be a non-empty list")
    return tuple(
        agent_wrapper_match_route(item, label=f"{label}[{index}]")
        for index, item in enumerate(raw)
    )


def agent_wrapper_match_route(raw: object, *, label: str) -> WrapperMatchRoute:
    if not isinstance(raw, Mapping):
        raise SpiceError(f"spice shell hook: {label} must be a table")
    extra = sorted(
        set(raw) - {"head", "flags", "keep", "search_operands", "argv", SCOPES_KEY}
    )
    if extra:
        raise SpiceError(
            f"spice shell hook: {label} has unsupported keys: {', '.join(extra)}"
        )
    head = raw.get("head")
    if head is not None:
        head = match_route_word(head, label=f"{label}.head")
    if raw.get("flags") is None:
        # A head-only route reroutes every argv under that head unconditionally.
        if head is None:
            raise SpiceError(f"spice shell hook: {label} needs a head or flags")
        flags: list[str] = []
    else:
        flags = config_string_list(raw.get("flags"), label=f"{label}.flags")
        if not flags:
            raise SpiceError(f"spice shell hook: {label}.flags has no entries")
        for word in flags:
            match_route_word(word, label=f"{label}.flags")
    if raw.get("keep") is None:
        keep: list[str] = []
    else:
        # Keep words are exempted from the flags patterns and stay on the
        # wrapped command; without flags there is nothing to exempt from.
        if not flags:
            raise SpiceError(f"spice shell hook: {label}.keep requires flags")
        keep = config_string_list(raw.get("keep"), label=f"{label}.keep")
        if not keep:
            raise SpiceError(f"spice shell hook: {label}.keep has no entries")
        for word in keep:
            match_route_word(word, label=f"{label}.keep")
    search_operands = raw.get("search_operands", False)
    if not isinstance(search_operands, bool):
        raise SpiceError(f"spice shell hook: {label}.search_operands must be boolean")
    if search_operands and (head != "grep" or flags):
        raise SpiceError(
            f"spice shell hook: {label}.search_operands requires a head-only grep route"
        )
    argv = command_words_from_config(raw.get("argv"), label=f"{label}.argv")
    scope = WRAPPER_ROUTE_SCOPES.parse(raw.get(SCOPES_KEY))
    return WrapperMatchRoute(
        head=head,
        flags=tuple(flags),
        keep=tuple(keep),
        search_operands=search_operands,
        argv=tuple(argv),
        scope=scope,
    )


def active_wrapper_driver_name(repo_root: Path) -> str:
    from spice.agent.driver import CODEX_DRIVER, SPICE_AGENT_DRIVER_ENV

    name = (
        os.environ.get(SPICE_AGENT_DRIVER_ENV, "").strip().casefold()
        or configured_agent_driver(repo_root).casefold()
        or CODEX_DRIVER.name
    )  # env-policy: allow
    known = known_wrapper_driver_names()
    if name not in known:
        expected = ", ".join(sorted(known))
        raise SpiceError(
            f"spice shell hook: active driver {name!r} must be one of: {expected}"
        )
    return name


def known_wrapper_driver_names() -> frozenset[str]:
    from spice.agent.driver import driver_scope_choices

    return frozenset(driver_scope_choices())


def match_route_word(value: object, *, label: str) -> str:
    if isinstance(value, str) and value and not any(ch.isspace() for ch in value):
        return value
    raise SpiceError(f"spice shell hook: {label} {value!r} must be a single word")


def match_route_pattern(flag: str) -> str:
    """Compile one match flag into a shell ``case`` glob.

    A literal ``*`` anywhere in the flag stays an unquoted wildcard while every
    other segment is shell-quoted literally. This lets a trailing wildcard like
    ``--glob=*`` match any ``--glob=`` value. Directory operands are matched by
    the dedicated ``search_operands`` scanner, not a leading-wildcard glob, so a
    grep-dialect directory search never reaches ``rg`` through this compiler.
    """
    segments = flag.split("*")
    return "*".join(shell_quote(segment) if segment else "" for segment in segments)


def render_agent_wrapper_selector_lines(
    *,
    group_name: str,
    wrapper: str,
    selector: str,
) -> list[str]:
    config_path = f"wrappers.{group_name}.{wrapper}"
    require_shell_function_name(
        selector,
        label=f"{config_path} command",
    )
    if selector == wrapper:
        raise SpiceError(
            "spice shell hook: wrapper "
            f"{wrapper!r} cannot intercept itself in {config_path}"
        )
    return [
        "",
        f"{selector}() {{",
        f'  {shell_quote(wrapper)} {shell_quote(selector)} "$@"',
        "}",
    ]


def command_words_from_config(raw: object, *, label: str) -> list[str]:
    words = config_string_list(raw, label=label)
    if not words:
        raise SpiceError(f"spice shell hook: {label} has no entries")
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


def user_home_path(base_env: Mapping[str, str]) -> Path:
    if home := base_env.get("HOME"):
        return Path(home).expanduser()
    return Path.home()


def shell_quote(value: str) -> str:
    return shlex.quote(value)
