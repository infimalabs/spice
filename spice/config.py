"""Harness configuration from project truth and worktree-local state."""

from __future__ import annotations

import json
import re
import subprocess
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from spice.paths import (
    atomic_write_json,
    atomic_write_text,
    read_json,
    repo_root_from_cwd,
    state_dir,
)
from spice.repocfg import agent_table

CONFIG_RELATIVE_PATH = Path("config") / "state.json"
CONFIG_SCHEMA_VERSION = 1

SAY_KEY = "say"
SAY_VOICE_KEY = "voice"
SAY_WORDS_PER_MINUTE_KEY = "words_per_minute"
DEFAULT_SAY_WORDS_PER_MINUTE = 175

AGENT_KEY = "agent"
AGENT_PERSONALITY_KEY = "personality"
AGENT_PERSONALITY_CHOICES = ("none", "friendly", "pragmatic")
DEFAULT_AGENT_PERSONALITY = "pragmatic"
AGENT_MODEL_KEY = "model"
AGENT_THINKING_KEY = "thinking"
AGENT_DRIVER_KEY = "driver"

JUDGE_KEY = "judge"
JUDGE_BIN_KEY = "bin"
DEFAULT_JUDGE_BIN = "afm-cli"
PROJECT_AGENT_TABLE = "tool.spice.agent"
_TOML_TABLE_RE = re.compile(r"^\s*\[([^\[\]]+)\]\s*(?:#.*)?$")
_TOML_ASSIGN_RE = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")


def config_state_path(repo_root: Path) -> Path:
    return state_dir(repo_root) / CONFIG_RELATIVE_PATH


def read_config_state(repo_root: Path) -> dict[str, Any]:
    data = read_json(config_state_path(repo_root))
    data.setdefault("schema", CONFIG_SCHEMA_VERSION)
    return data


def write_config_state(repo_root: Path, state: dict[str, Any]) -> Path:
    return atomic_write_json(config_state_path(repo_root), state)


def _section(repo_root: Path, key: str) -> dict[str, Any]:
    value = read_config_state(repo_root).get(key)
    return value if isinstance(value, dict) else {}


def update_section(repo_root: Path, key: str, values: dict[str, Any]) -> Path:
    state = read_config_state(repo_root)
    section = dict(_section(repo_root, key))
    section.update(values)
    state[key] = section
    return write_config_state(repo_root, state)


def clear_section(repo_root: Path, key: str) -> Path:
    state = read_config_state(repo_root)
    state.pop(key, None)
    return write_config_state(repo_root, state)


def config_overview(repo_root: Path) -> dict[str, Any]:
    return {
        "schema": CONFIG_SCHEMA_VERSION,
        "project": {AGENT_KEY: project_agent_config(repo_root)},
        "worktree": read_config_state(repo_root),
        "effective": {AGENT_KEY: effective_agent_config(repo_root)},
    }


def _root_or_current(repo_root: Path | None) -> Path | None:
    return repo_root if repo_root is not None else repo_root_from_cwd()


def configured_say_voice(repo_root: Path | None = None) -> str | None:
    root = _root_or_current(repo_root)
    if root is None:
        return None
    raw = _section(root, SAY_KEY).get(SAY_VOICE_KEY)
    return str(raw).strip() or None if raw else None


def configured_say_words_per_minute(repo_root: Path | None = None) -> int | None:
    root = _root_or_current(repo_root)
    if root is None:
        return None
    raw = _section(root, SAY_KEY).get(SAY_WORDS_PER_MINUTE_KEY)
    try:
        value = int(raw)
    except (TypeError, ValueError):
        return None
    return value if value > 0 else None


def configured_agent_personality(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    if root is None:
        return DEFAULT_AGENT_PERSONALITY
    raw = str(_section(root, AGENT_KEY).get(AGENT_PERSONALITY_KEY) or "").strip()
    return raw if raw in AGENT_PERSONALITY_CHOICES else DEFAULT_AGENT_PERSONALITY


def configured_agent_model(repo_root: Path | None = None) -> str:
    """Agent launch model override: worktree first, then tracked project."""
    root = _root_or_current(repo_root)
    if root is None:
        return ""
    return (
        _agent_worktree_value(root, AGENT_MODEL_KEY)
        or _agent_project_value(root, AGENT_MODEL_KEY)
        or ""
    )


def configured_agent_thinking(repo_root: Path | None = None) -> str:
    """Codex reasoning effort from the configured spice thinking setting."""
    root = _root_or_current(repo_root)
    if root is None:
        return ""
    return (
        _agent_worktree_value(root, AGENT_THINKING_KEY)
        or _agent_project_value(root, AGENT_THINKING_KEY)
        or ""
    )


def configured_agent_driver(repo_root: Path | None = None) -> str:
    """Which agent driver this worktree binds: worktree state, then project.

    Selects the agent CLI (`codex` | `claude`) when `SPICE_AGENT_DRIVER` is
    unset. Worktree-local state wins so one clone can run a different driver
    than the tracked project default without editing tracked history.
    """
    root = _root_or_current(repo_root)
    if root is None:
        return ""
    return (
        _agent_worktree_value(root, AGENT_DRIVER_KEY)
        or _agent_project_value(root, AGENT_DRIVER_KEY)
        or ""
    )


def worktree_agent_config(repo_root: Path) -> dict[str, str]:
    return {
        key: value
        for key in (AGENT_MODEL_KEY, AGENT_THINKING_KEY)
        if (value := _agent_worktree_value(repo_root, key))
    }


def project_agent_config(repo_root: Path) -> dict[str, str]:
    return {
        key: value
        for key in (AGENT_MODEL_KEY, AGENT_THINKING_KEY)
        if (value := _agent_project_value(repo_root, key))
    }


def effective_agent_config(repo_root: Path) -> dict[str, str]:
    return {
        key: value
        for key, value in {
            AGENT_MODEL_KEY: configured_agent_model(repo_root),
            AGENT_THINKING_KEY: configured_agent_thinking(repo_root),
        }.items()
        if value
    }


def _agent_worktree_value(repo_root: Path, key: str) -> str:
    return str(_section(repo_root, AGENT_KEY).get(key) or "").strip()


def _agent_project_value(repo_root: Path, key: str) -> str:
    return str(agent_table(repo_root).get(key) or "").strip()


def update_project_agent_config(repo_root: Path, values: Mapping[str, str]) -> Path:
    project_values = {
        key: value.strip()
        for key, value in values.items()
        if key in (AGENT_MODEL_KEY, AGENT_THINKING_KEY) and value.strip()
    }
    if not project_values:
        return repo_root / "pyproject.toml"
    return _rewrite_project_agent_table(repo_root, project_values, clear=False)


def clear_project_agent_config(repo_root: Path) -> Path:
    return _rewrite_project_agent_table(
        repo_root,
        {AGENT_MODEL_KEY: "", AGENT_THINKING_KEY: ""},
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
        if key in values:
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


def _toml_assignment(key: str, value: str) -> str:
    return f"{key} = {json.dumps(value)}"


def configured_judge_bin(repo_root: Path | None = None) -> str:
    root = _root_or_current(repo_root)
    if root is None:
        return DEFAULT_JUDGE_BIN
    raw = str(_section(root, JUDGE_KEY).get(JUDGE_BIN_KEY) or "").strip()
    return raw or DEFAULT_JUDGE_BIN


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
