"""Tracked repo configuration: the `[tool.spice]` table in pyproject.toml.

Two kinds of configuration, two homes. Constitution parameters and task
vocabulary are *project truth* — they belong in tracked history, so every clone
and every agent sees the same opinions. Operator-local state (speech voice,
judge binary, personality, worktree agent overrides) is *worktree truth* and
lives in `.spice/config/`.

Library seam: target-repo tools may import the public tracked-config table
readers and `string_list`; underscored names remain private.
"""

from __future__ import annotations

import tomllib
from pathlib import Path
from typing import Any


def read_tool_table(repo_root: Path) -> dict[str, Any]:
    pyproject = repo_root / "pyproject.toml"
    try:
        with pyproject.open("rb") as handle:
            loaded = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError):
        return {}
    tool = loaded.get("tool")
    if not isinstance(tool, dict):
        return {}
    table = tool.get("spice")
    return table if isinstance(table, dict) else {}


def policy_table(repo_root: Path) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("policy")
    return value if isinstance(value, dict) else {}


def tasks_table(repo_root: Path) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("tasks")
    return value if isinstance(value, dict) else {}


def agent_table(repo_root: Path) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("agent")
    return value if isinstance(value, dict) else {}


def commands_table(repo_root: Path) -> dict[str, Any]:
    value = read_tool_table(repo_root).get("commands")
    return value if isinstance(value, dict) else {}


def string_list(raw: Any) -> list[str]:
    if not isinstance(raw, list):
        return []
    values: list[str] = []
    for item in raw:
        value = str(item or "").strip()
        if value and value not in values:
            values.append(value)
    return values
