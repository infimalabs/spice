"""Harness configuration from project truth and worktree-local state."""

from __future__ import annotations

import json
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
from spice.configlayer import PYPROJECT_SOURCE
from spice.configlayer import effective_mapping, effective_table, layer_table
from spice.configlayer import load_config as load_config
from spice.errors import SpiceError
from spice.paths import (
    atomic_write_text,
    repo_root_from_cwd,
    state_dir,
)

WORKTREE_CONFIG_RELATIVE_PATH = Path("config") / "spice.toml"
LEGACY_CONFIG_STATE_RELATIVE_PATH = Path("config") / "state.json"
LEGACY_CONFIG_SCHEMA = 1
WORKTREE_CONFIG_SECTIONS = ("say", "judge", "agent")

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
PROJECT_AGENT_TABLE = "tool.spice.agent"
_TOML_TABLE_RE = re.compile(r"^\s*\[([^\[\]]+)\]\s*(?:#.*)?$")
_TOML_ASSIGN_RE = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")


def worktree_config_path(repo_root: Path) -> Path:
    """Path to this worktree's local TOML configuration layer."""
    return state_dir(repo_root) / WORKTREE_CONFIG_RELATIVE_PATH


def _legacy_config_state_path(repo_root: Path) -> Path:
    return state_dir(repo_root) / LEGACY_CONFIG_STATE_RELATIVE_PATH


def read_worktree_config(repo_root: Path) -> dict[str, Any]:
    """Return the worktree TOML config, migrating legacy JSON state first."""
    _ensure_worktree_config_migrated(repo_root)
    return _load_worktree_toml(repo_root)


def _load_worktree_toml(repo_root: Path) -> dict[str, Any]:
    path = worktree_config_path(repo_root)
    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except FileNotFoundError:
        return {}
    except tomllib.TOMLDecodeError as exc:
        raise SpiceError(f"invalid TOML in {path}: {exc}") from exc
    except OSError as exc:
        raise SpiceError(f"cannot read configuration {path}: {exc}") from exc


def _section(repo_root: Path, key: str) -> dict[str, Any]:
    value = read_worktree_config(repo_root).get(key)
    return value if isinstance(value, dict) else {}


def set_worktree_section(repo_root: Path, key: str, values: Mapping[str, Any]) -> Path:
    """Merge `values` into the worktree TOML `[key]` table, preserving the rest.

    Unrelated tables, comments, key ordering, and scalar types outside the
    touched keys survive the round trip; a structured line edit rewrites only
    the `[key]` table's assignments.
    """
    _ensure_worktree_config_migrated(repo_root)
    lines = _read_worktree_config_lines(repo_root)
    _apply_worktree_table(lines, key, dict(values))
    return _write_worktree_config_lines(repo_root, lines)


def clear_worktree_section(repo_root: Path, key: str) -> Path:
    """Remove the worktree TOML `[key]` table, preserving unrelated content."""
    _ensure_worktree_config_migrated(repo_root)
    lines = _read_worktree_config_lines(repo_root)
    _remove_worktree_table(lines, key)
    return _write_worktree_config_lines(repo_root, lines)


def _ensure_worktree_config_migrated(repo_root: Path) -> None:
    """Migrate a schema-1 ``state.json`` into the worktree TOML exactly once.

    The TOML write lands durably before the JSON source is deleted, so a failed
    migration leaves ``state.json`` intact and raises an actionable error; once
    the JSON file is gone no caller reads it again.
    """
    legacy = _legacy_config_state_path(repo_root)
    if not legacy.exists():
        return
    try:
        raw = json.loads(legacy.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise SpiceError(f"cannot migrate legacy config state {legacy}: {exc}") from exc
    if not isinstance(raw, Mapping) or raw.get("schema") != LEGACY_CONFIG_SCHEMA:
        raise SpiceError(
            f"cannot migrate legacy config state {legacy}: expected schema "
            f"{LEGACY_CONFIG_SCHEMA}"
        )
    sections = {
        key: dict(value)
        for key in WORKTREE_CONFIG_SECTIONS
        if isinstance(value := raw.get(key), Mapping) and value
    }
    try:
        _load_worktree_toml(repo_root)
        if sections:
            lines = _read_worktree_config_lines(repo_root)
            for table, values in sections.items():
                _apply_worktree_table(lines, table, values)
            _write_worktree_config_lines(repo_root, lines)
        legacy.unlink()
    except SpiceError:
        raise
    except OSError as exc:
        raise SpiceError(
            "cannot migrate legacy config state to "
            f"{worktree_config_path(repo_root)}: {exc}"
        ) from exc


def _read_worktree_config_lines(repo_root: Path) -> list[str]:
    path = worktree_config_path(repo_root)
    try:
        return path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        return []
    except OSError as exc:
        raise SpiceError(f"cannot read configuration {path}: {exc}") from exc


def _write_worktree_config_lines(repo_root: Path, lines: list[str]) -> Path:
    text = "\n".join(lines) + "\n" if lines else ""
    path = worktree_config_path(repo_root)
    try:
        tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        raise SpiceError(f"invalid TOML in {path}: {exc}") from exc
    return atomic_write_text(path, text)


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
    return {
        "project": {AGENT_KEY: project_agent_config(repo_root)},
        "worktree": read_worktree_config(repo_root),
        "effective": {AGENT_KEY: effective_agent_config(repo_root)},
    }


def default_classifications() -> dict[str, str]:
    """Classify exported defaults for configuration diagnostics."""
    return defaults.export_classifications()


def _root_or_current(repo_root: Path | None) -> Path | None:
    return repo_root if repo_root is not None else repo_root_from_cwd()


def _effective_section(root: Path | None, key: str) -> dict[str, Any]:
    raw = effective_mapping(root).get(key)
    return raw if isinstance(raw, dict) else {}


def _configured_value(root: Path | None, section: str, key: str) -> Any:
    local = _section(root, section).get(key) if root is not None else None
    return local if local is not None else _effective_section(root, section).get(key)


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


def worktree_agent_config(repo_root: Path) -> dict[str, str]:
    return {
        key: value
        for key in AGENT_LAUNCH_KEYS
        if (value := _agent_worktree_value(repo_root, key))
    }


def project_agent_config(repo_root: Path) -> dict[str, str]:
    return {
        key: value
        for key in AGENT_LAUNCH_KEYS
        if (value := _agent_project_value(repo_root, key))
    }


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


def _agent_worktree_value(repo_root: Path, key: str) -> str:
    return str(_section(repo_root, AGENT_KEY).get(key) or "").strip()


def _agent_project_value(repo_root: Path, key: str) -> str:
    return str(layer_table(repo_root, PYPROJECT_SOURCE, "agent").get(key) or "").strip()


def _agent_effective_value(repo_root: Path, key: str) -> str:
    return str(effective_table(repo_root, "agent").get(key) or "").strip()


def update_project_agent_config(repo_root: Path, values: Mapping[str, str]) -> Path:
    project_values = {
        key: value.strip()
        for key, value in values.items()
        if key in AGENT_LAUNCH_KEYS and value.strip()
    }
    if not project_values:
        return repo_root / "pyproject.toml"
    return _rewrite_project_agent_table(repo_root, project_values, clear=False)


def clear_project_agent_config(repo_root: Path) -> Path:
    return _rewrite_project_agent_table(
        repo_root,
        dict.fromkeys(AGENT_LAUNCH_KEYS, ""),
        clear=True,
    )


def _rewrite_project_agent_table(
    repo_root: Path, values: Mapping[str, str], *, clear: bool
) -> Path:
    pyproject = repo_root / "pyproject.toml"
    try:
        original = pyproject.read_text(encoding="utf-8")
    except OSError:
        original = ""
    lines = original.splitlines()
    start, end = _toml_table_bounds(lines, PROJECT_AGENT_TABLE)
    if start is None:
        if clear:
            return pyproject
        if lines and lines[-1].strip():
            lines.append("")
        lines.append(f"[{PROJECT_AGENT_TABLE}]")
        lines.extend(_toml_assignments(values).values())
        return atomic_write_text(pyproject, "\n".join(lines) + "\n")

    rewritten: list[str] = []
    seen: set[str] = set()
    for line in lines[start + 1 : end]:
        key = _toml_assignment_key(line)
        if key is not None and key in values:
            seen.add(key)
            if not clear:
                rewritten.append(_toml_assignment(key, values[key]))
            continue
        rewritten.append(line)
    if not clear:
        for key, line in _toml_assignments(values).items():
            if key not in seen:
                rewritten.append(line)
    lines[start + 1 : end] = rewritten
    return atomic_write_text(pyproject, "\n".join(lines) + "\n")


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


def _toml_assignments(values: Mapping[str, str]) -> dict[str, str]:
    return {key: _toml_assignment(key, value) for key, value in values.items()}


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
