"""Layered harness configuration and scoped TOML editing."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tomllib
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from spice import defaults
from spice.configlayer import ConfigLayer as ConfigLayer
from spice.configlayer import LayeredConfig as LayeredConfig
from spice.configlayer import CONFIG_SCOPE_NAMES
from spice.configlayer import PYPROJECT_SOURCE
from spice.configlayer import REPOSITORY_SOURCE
from spice.configlayer import SYSTEM_SOURCE
from spice.configlayer import WORKTREE_SOURCE
from spice.configlayer import effective_mapping, effective_table
from spice.configlayer import layer_table as layer_table
from spice.configlayer import load_config as load_config
from spice.errors import SpiceError
from spice.paths import (
    atomic_write_text,
    repo_root_from_cwd,
    runtime_spice_source,
    state_dir,
)

WORKTREE_CONFIG_RELATIVE_PATH = Path("config") / "spice.toml"

SAY_KEY = "say"
SAY_BACKEND_KEY = "backend"
SAY_BACKEND_CHOICES = defaults.strings("say", "backend_choices")
DEFAULT_SAY_BACKEND = defaults.string("say", "backend")
SAY_COMMAND_KEY = "command"
SAY_CONTENT_TYPE_KEY = "content_type"
DEFAULT_EXTERNAL_SAY_CONTENT_TYPE = defaults.string("say", "external_content_type")
SAY_VOICE_KEY = "voice"
SAY_WORDS_PER_MINUTE_KEY = "words_per_minute"
DEFAULT_SAY_WORDS_PER_MINUTE = defaults.integer("say", "words_per_minute")
SAY_TIMEOUT_SECONDS_KEY = "timeout_seconds"
# Generous ceiling: comfortably covers well over a minute of spoken content
# (plus render overhead) so legitimately-long messages are never clipped, while
# still bounding a wedged speech process instead of blocking forever. A repo or
# worktree ``say.timeout_seconds`` override tunes it through the accessor below.
DEFAULT_SAY_TIMEOUT_SECONDS = 300.0
SAY_MUTABLE_KEYS = (
    SAY_BACKEND_KEY,
    SAY_COMMAND_KEY,
    SAY_CONTENT_TYPE_KEY,
    SAY_VOICE_KEY,
    SAY_WORDS_PER_MINUTE_KEY,
    SAY_TIMEOUT_SECONDS_KEY,
)

AGENT_KEY = "agent"
AGENT_PERSONALITY_KEY = "personality"
AGENT_PERSONALITY_CHOICES = defaults.strings("agent", "personality_choices")
DEFAULT_AGENT_PERSONALITY = defaults.string("agent", "personality")
AGENT_MODEL_KEY = "model"
AGENT_EFFORT_KEY = "effort"
AGENT_DRIVER_KEY = "driver"
AGENT_LAUNCH_KEYS = (AGENT_MODEL_KEY, AGENT_EFFORT_KEY, AGENT_DRIVER_KEY)

JUDGE_KEY = "judge"
JUDGE_BIN_KEY = "bin"
DEFAULT_JUDGE_BIN = defaults.string("judge", "bin")
PORTABLE_JUDGE_BIN = defaults.string("judge", "portable_bin")
_TOML_TABLE_RE = re.compile(r"^\s*\[([^\[\]]+)\]\s*(?:#.*)?$")
_TOML_ASSIGN_RE = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")


def worktree_config_path(repo_root: Path) -> Path:
    """Path to this worktree's local TOML configuration layer."""
    return state_dir(repo_root) / WORKTREE_CONFIG_RELATIVE_PATH


def config_scope_path(repo_root: Path, scope: str) -> Path:
    """Return the canonical TOML path for one explicit mutable scope."""
    if scope == SYSTEM_SOURCE:
        return runtime_spice_source() / "spice.toml"
    if scope == PYPROJECT_SOURCE:
        return repo_root / "pyproject.toml"
    if scope == REPOSITORY_SOURCE:
        return repo_root / "spice.toml"
    if scope == WORKTREE_SOURCE:
        return worktree_config_path(repo_root)
    raise SpiceError(
        f"unknown configuration scope {scope!r}; expected "
        + ", ".join(CONFIG_SCOPE_NAMES)
    )


def set_scope_section(
    repo_root: Path, scope: str, key: str, values: Mapping[str, Any]
) -> Path:
    """Merge values into one scoped table through the shared TOML mutation seam."""
    return _mutate_scope_section(repo_root, scope, key, values=dict(values))


def clear_scope_section(
    repo_root: Path,
    scope: str,
    key: str,
    *,
    keys: tuple[str, ...] | None = None,
) -> Path:
    """Remove one scoped table or selected values without touching other layers."""
    return _mutate_scope_section(repo_root, scope, key, clear_keys=keys)


def _mutate_scope_section(
    repo_root: Path,
    scope: str,
    key: str,
    *,
    values: Mapping[str, Any] | None = None,
    clear_keys: tuple[str, ...] | None = (),
) -> Path:
    path = config_scope_path(repo_root, scope)
    if scope == SYSTEM_SOURCE and (not path.is_file() or not os.access(path, os.W_OK)):
        raise SpiceError(f"configuration scope=system path={path} is not writable")
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        lines = []
    except OSError as exc:
        raise SpiceError(
            f"cannot read configuration scope={scope} path={path}: {exc}"
        ) from exc
    table = f"tool.spice.{key}" if scope == PYPROJECT_SOURCE else key
    if values:
        _apply_worktree_table(lines, table, values)
    elif clear_keys is None:
        _remove_worktree_table(lines, table)
    else:
        _remove_table_keys(lines, table, set(clear_keys))
    text = "\n".join(lines) + "\n" if lines else ""
    try:
        tomllib.loads(text)
        return atomic_write_text(path, text)
    except tomllib.TOMLDecodeError as exc:
        raise SpiceError(
            f"invalid TOML for configuration scope={scope} path={path}: {exc}"
        ) from exc
    except OSError as exc:
        raise SpiceError(
            f"cannot write configuration scope={scope} path={path}: {exc}"
        ) from exc


def _apply_worktree_table(
    lines: list[str], table: str, values: Mapping[str, Any]
) -> None:
    if not values:
        return
    start, end = _toml_table_bounds(lines, table)
    if start is None:
        if lines and lines[-1].strip():
            lines.append("")
        lines.append(f"[{table}]")
        lines.extend(_toml_assignment(key, value) for key, value in values.items())
        return
    rewritten: list[str] = []
    seen: set[str] = set()
    for line in lines[start + 1 : end]:
        key = _toml_assignment_key(line)
        if key is not None and key in values:
            seen.add(key)
            assignment = _toml_assignment(key, values[key])
            comment = _toml_inline_comment(line)
            rewritten.append(f"{assignment} {comment}" if comment else assignment)
            continue
        rewritten.append(line)
    rewritten.extend(
        _toml_assignment(key, value) for key, value in values.items() if key not in seen
    )
    lines[start + 1 : end] = rewritten


def _remove_worktree_table(lines: list[str], table: str) -> None:
    start, end = _toml_table_bounds(lines, table)
    if start is not None:
        del lines[start:end]


def _remove_table_keys(lines: list[str], table: str, keys: set[str]) -> None:
    start, end = _toml_table_bounds(lines, table)
    if start is None:
        return
    lines[start + 1 : end] = [
        line
        for line in lines[start + 1 : end]
        if _toml_assignment_key(line) not in keys
    ]


def _toml_inline_comment(line: str) -> str:
    """Return a TOML comment outside quoted strings, including its ``#``."""
    basic = False
    literal = False
    escaped = False
    for index, character in enumerate(line):
        if basic:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                basic = False
            continue
        if literal:
            if character == "'":
                literal = False
            continue
        if character == '"':
            basic = True
        elif character == "'":
            literal = True
        elif character == "#":
            return line[index:]
    return ""


def config_overview(repo_root: Path) -> dict[str, Any]:
    loaded = load_config(repo_root)
    return {
        "layers": {
            layer.name: {
                "path": str(layer.path),
                "present": layer.present,
                "values": _json_value(layer.values),
            }
            for layer in loaded.layers
        },
        "effective": _json_value(loaded.effective),
        "provenance": {
            ".".join(path): {"scope": layer.name, "path": str(layer.path)}
            for path, layer in sorted(loaded.sources.items())
        },
    }


def agent_config_overview(repo_root: Path) -> dict[str, Any]:
    overview = config_overview(repo_root)
    provenance = overview["provenance"]
    return {
        "effective": effective_agent_config(repo_root),
        "provenance": {
            key: value
            for key, value in provenance.items()
            if key.startswith(f"{AGENT_KEY}.")
        },
    }


def _json_value(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _json_value(item) for key, item in value.items()}
    if isinstance(value, tuple):
        return [_json_value(item) for item in value]
    return value


def default_classifications() -> dict[str, str]:
    """Classify exported defaults for configuration diagnostics."""
    return defaults.export_classifications()


def _root_or_current(repo_root: Path | None) -> Path | None:
    return repo_root if repo_root is not None else repo_root_from_cwd()


def _effective_section(root: Path | None, key: str) -> dict[str, Any]:
    raw = effective_mapping(root).get(key)
    return raw if isinstance(raw, dict) else {}


def _configured_value(root: Path | None, section: str, key: str) -> Any:
    return _effective_section(root, section).get(key)


def configured_say_voice(repo_root: Path | None = None) -> str | None:
    root = _root_or_current(repo_root)
    raw = _configured_value(root, SAY_KEY, SAY_VOICE_KEY)
    return str(raw).strip() or None if raw else None


def configured_say_backend(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    raw = str(_configured_value(root, SAY_KEY, SAY_BACKEND_KEY) or "").strip()
    return raw if raw in SAY_BACKEND_CHOICES else DEFAULT_SAY_BACKEND


def configured_say_command(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    return str(_configured_value(root, SAY_KEY, SAY_COMMAND_KEY) or "").strip()


def configured_say_content_type(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    raw = str(_configured_value(root, SAY_KEY, SAY_CONTENT_TYPE_KEY) or "").strip()
    return raw or DEFAULT_EXTERNAL_SAY_CONTENT_TYPE


def configured_say_words_per_minute(repo_root: Path | None = None) -> int | None:
    root = _root_or_current(repo_root)
    raw = _configured_value(root, SAY_KEY, SAY_WORDS_PER_MINUTE_KEY)
    if raw is None:
        return None
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return None
    return value if value > 0 else None


def configured_say_timeout(repo_root: Path | None = None) -> float:
    """Seconds a speech subprocess may run before it is bounded and reported.

    Falls back to the generous default when unset or non-positive so a valid
    long message is never clipped; a positive override lets operators tune it.
    """
    root = _root_or_current(repo_root)
    raw = _configured_value(root, SAY_KEY, SAY_TIMEOUT_SECONDS_KEY)
    if raw is None:
        return DEFAULT_SAY_TIMEOUT_SECONDS
    try:
        value = float(raw)
    except (TypeError, ValueError):
        return DEFAULT_SAY_TIMEOUT_SECONDS
    if value != value:  # NaN
        return DEFAULT_SAY_TIMEOUT_SECONDS
    return value if value > 0 else DEFAULT_SAY_TIMEOUT_SECONDS


def configured_agent_personality(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    raw = str(_configured_value(root, AGENT_KEY, AGENT_PERSONALITY_KEY) or "").strip()
    return raw if raw in AGENT_PERSONALITY_CHOICES else DEFAULT_AGENT_PERSONALITY


def configured_agent_model(repo_root: Path | None = None) -> str:
    """Agent launch model from the canonical layered configuration."""
    root = _root_or_current(repo_root)
    if root is None:
        return ""
    return _agent_effective_value(root, AGENT_MODEL_KEY)


def configured_agent_effort(repo_root: Path | None = None) -> str:
    """Codex reasoning effort from the configured spice effort setting."""
    root = _root_or_current(repo_root)
    if root is None:
        return ""
    return _agent_effective_value(root, AGENT_EFFORT_KEY)


def configured_agent_driver(repo_root: Path | None = None) -> str:
    """Which agent driver this worktree binds: worktree state, then project.

    Selects the agent CLI (`codex` | `claude`) when `SPICE_AGENT_DRIVER` is
    unset. Worktree-local state wins so one clone can run a different driver
    than the tracked project default without editing tracked history.
    """
    root = _root_or_current(repo_root)
    if root is None:
        return ""
    return _agent_effective_value(root, AGENT_DRIVER_KEY)


def effective_agent_config(repo_root: Path) -> dict[str, str]:
    from spice.agent.driver import driver_for

    driver = driver_for(repo_root)
    return {
        AGENT_DRIVER_KEY: driver.name,
        AGENT_MODEL_KEY: driver.resolve_model(configured_agent_model(repo_root)),
        AGENT_EFFORT_KEY: (
            configured_agent_effort(repo_root) or driver.default_reasoning_effort
        ),
    }


def _agent_effective_value(repo_root: Path, key: str) -> str:
    return str(effective_table(repo_root, "agent").get(key) or "").strip()


def _toml_table_bounds(lines: list[str], table: str) -> tuple[int | None, int | None]:
    start: int | None = None
    for index, line in enumerate(lines):
        name = _toml_table_name(line)
        if name == table:
            start = index
            continue
        if start is not None and name is not None:
            return start, index
    return (start, len(lines)) if start is not None else (None, None)


def _toml_table_name(line: str) -> str | None:
    match = _TOML_TABLE_RE.match(line)
    return match.group(1).strip() if match else None


def _toml_assignment_key(line: str) -> str | None:
    match = _TOML_ASSIGN_RE.match(line)
    return match.group(1) if match else None


def _toml_assignment(key: str, value: Any) -> str:
    return f"{key} = {_toml_scalar(value)}"


def _toml_scalar(value: Any) -> str:
    """Render a scalar as valid TOML, preserving bool/int types across the trip."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return repr(value)
    return json.dumps(str(value))


def default_judge_bin() -> str:
    """Return the built-in judge bin for this platform.

    macOS keeps the Apple Foundation Models ``afm-cli`` default; every other
    platform, where ``afm-cli`` does not exist, defaults to the portable
    ``spice-judge`` adapter so the conscience works out of the box off macOS.
    """
    return DEFAULT_JUDGE_BIN if sys.platform == "darwin" else PORTABLE_JUDGE_BIN


def configured_judge_bin(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    raw = str(_configured_value(root, JUDGE_KEY, JUDGE_BIN_KEY) or "").strip()
    if raw == DEFAULT_JUDGE_BIN and sys.platform != "darwin":
        return PORTABLE_JUDGE_BIN
    return raw or default_judge_bin()


def say_command_args(
    repo_root: Path | None = None, *, rate_multiplier: float = 1.0
) -> list[str]:
    """Build the macOS `say` argv from repo-local config.

    Unset config emits only `["say"]` so the system voice and rate apply.
    """
    args = ["say"]
    voice = configured_say_voice(repo_root)
    if voice:
        args.extend(["-v", voice])
    words_per_minute = configured_say_words_per_minute(repo_root)
    if words_per_minute is None and rate_multiplier != 1.0:
        words_per_minute = DEFAULT_SAY_WORDS_PER_MINUTE
    if words_per_minute is not None:
        effective = max(1, int(words_per_minute * rate_multiplier + 0.5))
        args.extend(["-r", str(effective)])
    return args


def git_worktree_config_get(repo_root: Path, key: str) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo_root), "config", "--worktree", "--get", key],
            capture_output=True,
            check=False,
            text=True,
        )
    except OSError:
        return None
    if result.returncode != 0:
        return None
    return result.stdout.strip() or None


def git_worktree_config_set(repo_root: Path, key: str, value: str) -> None:
    """Set a real Git worktree config value (settings Git itself owns)."""
    subprocess.run(
        ["git", "-C", str(repo_root), "config", "--worktree", key, value],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
