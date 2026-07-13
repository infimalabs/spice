"""Effective repository configuration from the canonical layered view.

Two kinds of configuration, two homes. Constitution parameters and task
vocabulary are *project truth* — they belong in tracked history, so every clone
and every agent sees the same opinions. Operator-local state (speech voice,
judge binary, personality, worktree agent overrides) is *worktree truth* and
lives in `.spice/config/`.

"""

from __future__ import annotations

import tomllib
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from spice.configlayer import (
    PYPROJECT_SOURCE,
    REPOSITORY_SOURCE,
    load_config,
    load_packaged_config,
)


def read_pyproject(repo_root: Path) -> dict[str, Any]:
    """The whole parsed `pyproject.toml`, or {} when missing/malformed."""
    pyproject = repo_root / "pyproject.toml"
    try:
        with pyproject.open("rb") as handle:
            loaded = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


def read_tool_table(repo_root: Path | None) -> dict[str, Any]:
    """Return a mutable copy of the effective Spice configuration."""
    values = (
        load_config(repo_root).effective
        if repo_root is not None
        else load_packaged_config().values
    )
    return _thaw_mapping(values)


def policy_table(repo_root: Path | None) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("policy")
    return value if isinstance(value, dict) else {}


def maxims_table(repo_root: Path | None) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("maxims")
    return value if isinstance(value, dict) else {}


def tasks_table(repo_root: Path | None) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("tasks")
    return value if isinstance(value, dict) else {}


def agent_table(repo_root: Path | None) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("agent")
    return value if isinstance(value, dict) else {}


def project_agent_table(repo_root: Path) -> dict[str, Any]:
    """Tracked agent values only: pyproject, then repository overrides."""
    layered = load_config(repo_root)
    merged: dict[str, Any] = {}
    for source in (PYPROJECT_SOURCE, REPOSITORY_SOURCE):
        value = layered.layer(source).values.get("agent")
        if isinstance(value, Mapping):
            merged.update(_thaw_mapping(value))
    return merged


def agent_wrapper_definitions_table(repo_root: Path | None) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("wrappers")
    return value if isinstance(value, dict) else {}


def commands_table(repo_root: Path | None) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("commands")
    if not isinstance(value, dict):
        return {}
    flattened: dict[str, Any] = {}
    _flatten_commands_table(value, flattened)
    return flattened


def locks_table(repo_root: Path | None) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("locks")
    return value if isinstance(value, dict) else {}


def _flatten_commands_table(
    source: dict[str, Any], destination: dict[str, Any], *, prefix: str = ""
) -> None:
    for key, value in source.items():
        name = f"{prefix}.{key}" if prefix else str(key)
        if isinstance(value, dict):
            _flatten_commands_table(value, destination, prefix=name)
            continue
        destination[name] = value


def string_list(raw: Any) -> list[str]:
    if not isinstance(raw, list):
        return []
    values: list[str] = []
    for item in raw:
        value = str(item or "").strip()
        if value and value not in values:
            values.append(value)
    return values


def _thaw_mapping(raw: Mapping[str, Any]) -> dict[str, Any]:
    return {str(key): _thaw(value) for key, value in raw.items()}


def _thaw(raw: Any) -> Any:
    if isinstance(raw, Mapping):
        return _thaw_mapping(raw)
    if isinstance(raw, Sequence) and not isinstance(raw, (str, bytes)):
        return [_thaw(item) for item in raw]
    return raw
