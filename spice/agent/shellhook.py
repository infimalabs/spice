"""Shell startup hooks for agent side-channel steering."""

from __future__ import annotations

import os
import re
import shlex
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import NamedTuple

from spice.config import configured_agent_driver, configured_rtk_executable
from spice.configlayer import (
    SYSTEM_SOURCE,
    contextualize_config_error,
    effective_table,
    load_config,
)
from spice.errors import SpiceError
from spice.extensions import (
    SPICE_WRAPPER_ENTRY_POINT_GROUP,
    SpiceExtensionEntryPoint,
    extension_entry_points,
)
from spice.scopes import (
    SCOPES_KEY,
    WRAPPER_ROUTE_SCOPES,
    WRAPPER_SCOPES,
    ScopeContext,
    ScopeSelector,
)

ZDOTDIR_ENV = "ZDOTDIR"
BASH_ENV_ENV = "BASH_ENV"
HISTFILE_ENV = "HISTFILE"
ZSH_COMPDUMP_ENV = "ZSH_COMPDUMP"
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
    env[SHELL_HOOK_WRAPPERS_ENV] = "\n".join(render_agent_wrapper_lines(repo_root))
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


def _render_agent_wrapper_lines(repo_root: Path) -> list[str]:
    driver_name = active_wrapper_driver_name(repo_root)
    rtk_executable = configured_rtk_executable(repo_root)
    lines: list[str] = []
    seen_selectors: dict[str, str] = {}
    for selected in _selected_agent_wrapper_groups(repo_root):
        lines.extend(
            render_agent_wrapper_group_lines(
                group_name=selected.name,
                group=selected.group,
                seen_selectors=seen_selectors,
                driver_name=driver_name,
                rtk_executable=rtk_executable,
            )
        )
    return lines


def rtk_rewrite_yield_selectors(repo_root: Path) -> frozenset[str]:
    """Wrapper words the agent-run RTK rewrite must leave to the shell.

    A selected non-system direct wrapper whose argv head is not RTK claims its
    selector word: the pre-shell rewrite substitutes command text before any
    wrapper function exists, so an RTK claim on such a word would shadow the
    configured or extension-provided expansion. Installed system wrappers stay
    rewritable because they are designed around RTK. Configuration errors
    yield the empty set here; shell-hook rendering surfaces them loudly.
    """
    try:
        return _rtk_rewrite_yield_selectors(repo_root)
    except SpiceError:
        return frozenset()


def _rtk_rewrite_yield_selectors(repo_root: Path) -> frozenset[str]:
    layered = load_config(repo_root)
    context = ScopeContext(driver=active_wrapper_driver_name(repo_root))
    rtk_words = {RTK_CANONICAL_EXECUTABLE, configured_rtk_executable(repo_root)}
    selectors: set[str] = set()
    for selected in _selected_agent_wrapper_groups(repo_root):
        group_name = selected.name
        raw_group = selected.group
        if not WRAPPER_SCOPES.parse(raw_group.get(SCOPES_KEY)).matches(context):
            continue
        for raw_wrapper, raw_entry in raw_group.items():
            wrapper = str(raw_wrapper).strip()
            if wrapper == SCOPES_KEY or not isinstance(raw_entry, Mapping):
                continue
            if not selected.from_extension:
                source = layered.source_for(("wrappers", group_name, wrapper))
                if source is None or source.name == SYSTEM_SOURCE:
                    continue
            if not WRAPPER_SCOPES.parse(raw_entry.get(SCOPES_KEY)).matches(context):
                continue
            command_words = command_words_from_config(
                raw_entry.get("argv"),
                label=f"tool.spice.wrappers.{group_name}.{wrapper}.argv",
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
        label=f"tool.spice.agent.{AGENT_WRAPPERS_KEY}",
    )
    definitions, configured_sources = configured_agent_wrapper_definitions(repo_root)
    extension_entries = entry_point_agent_wrapper_entries(
        configured_sources=configured_sources
    )
    selected: list[_SelectedAgentWrapperGroup] = []
    for group_name in ordered_groups:
        require_config_name(
            group_name,
            label=f"tool.spice.agent.{AGENT_WRAPPERS_KEY} group",
        )
        raw_group = definitions.get(group_name)
        from_extension = raw_group is None and group_name in extension_entries
        if from_extension:
            raw_group = entry_point_wrapper_group_from_entry(
                extension_entries[group_name]
            )
        if raw_group is False:
            continue
        if not isinstance(raw_group, Mapping):
            raise SpiceError(
                f"spice shell hook: missing tool.spice.wrappers.{group_name}"
            )
        selected.append(
            _SelectedAgentWrapperGroup(
                name=group_name,
                group=raw_group,
                from_extension=from_extension,
            )
        )
    return tuple(selected)


def configured_agent_wrapper_definitions(
    repo_root: Path,
) -> tuple[dict[str, object], dict[str, str]]:
    definitions: dict[str, object] = dict(effective_table(repo_root, "wrappers"))
    sources: dict[str, str] = {
        name: f"tool.spice.wrappers.{name}" for name in definitions
    }
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
    seen_selectors: dict[str, str],
    driver_name: str,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
) -> list[str]:
    context = ScopeContext(driver=driver_name)
    group_scope = WRAPPER_SCOPES.parse(group.get(SCOPES_KEY))
    if not group_scope.matches(context):
        return []
    lines: list[str] = []
    for raw_wrapper, raw_entry in group.items():
        wrapper = str(raw_wrapper).strip()
        if wrapper == SCOPES_KEY:
            continue
        if raw_entry is False:
            continue
        if isinstance(raw_entry, Mapping):
            lines.extend(
                render_agent_direct_wrapper_lines(
                    group_name=group_name,
                    selector=wrapper,
                    entry=raw_entry,
                    seen_selectors=seen_selectors,
                    driver_name=driver_name,
                    rtk_executable=rtk_executable,
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
    driver_name: str,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
) -> list[str]:
    config_path = f"tool.spice.wrappers.{group_name}.{selector}"
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
    record_agent_wrapper_selector(
        selector,
        config_path,
        seen_selectors=seen_selectors,
    )
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
        "    local _spice_grep_seen_head=",
        "    local _spice_grep_seen_pattern=",
        "    local _spice_grep_expect=",
        "    local _spice_grep_positional=",
        "    local _spice_grep_has_operand=",
        '    for _spice_word in "$@"; do',
        '      if [ -z "$_spice_grep_seen_head" ]; then',
        "        _spice_grep_seen_head=1",
        "        continue",
        "      fi",
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
        '    if [ -n "$_spice_grep_has_operand" ]; then',
        "      shift",
        f'      command {argv} "$@"',
        "      return",
        "    fi",
        "  fi",
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
    if flag.endswith("*") and len(flag) > 1:
        return shell_quote(flag[:-1]) + "*"
    return shell_quote(flag)


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
    if not words:
        raise SpiceError(f"spice shell hook: {label} has no entries")
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


def user_home_path(base_env: Mapping[str, str]) -> Path:
    if home := base_env.get("HOME"):
        return Path(home).expanduser()
    return Path.home()


def shell_quote(value: str) -> str:
    return shlex.quote(value)
