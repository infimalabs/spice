"""Scoped TOML editing for the mutable configuration layers."""

from __future__ import annotations

import json
import os
import re
import tomllib
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from spice.config.layers import (
    CONFIG_SCOPE_NAMES,
    REPOSITORY_SOURCE,
    SYSTEM_SOURCE,
    WORKTREE_SOURCE,
)
from spice.config.schema import validate_config_keys
from spice.errors import SpiceError
from spice.operatorstate import (
    WORKTREE_CONFIG_PATH,
    operator_state_path,
    prepare_operator_state_path,
)
from spice.process.git import run_git_command
from spice.paths import atomic_write_text, runtime_spice_source

_TOML_TABLE_RE = re.compile(r"^\s*\[([^\[\]]+)\]\s*(?:#.*)?$")
_TOML_ASSIGN_RE = re.compile(
    r"""^\s*((?:"(?:\\.|[^"])*"|'[^']*'|[A-Za-z0-9_-]+))\s*="""
)
_TOML_BARE_KEY_RE = re.compile(r"^[A-Za-z0-9_-]+$")
_TOML_NUMBER_RE = re.compile(
    r"^[+-]?(?:inf|nan|0x[0-9A-Fa-f_]+|0o[0-7_]+|0b[01_]+|"
    r"(?:\d[\d_]*)(?:\.[\d_]+)?(?:[eE][+-]?[\d_]+)?)$"
)


def worktree_config_path(repo_root: Path) -> Path:
    """Path to this worktree's local TOML configuration layer."""
    return operator_state_path(repo_root, WORKTREE_CONFIG_PATH)


def config_scope_path(repo_root: Path, scope: str) -> Path:
    """Return the canonical TOML path for one explicit mutable scope."""
    if scope == SYSTEM_SOURCE:
        return runtime_spice_source() / "spice.toml"
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


def set_scope_value(
    repo_root: Path, scope: str, key_path: Sequence[str], value: Any
) -> Path:
    """Set one explicitly segmented leaf in a scoped configuration layer."""
    path = tuple(key_path)
    if len(path) < 2 or any(not part for part in path):
        raise SpiceError("configuration set key must name a dotted table leaf")
    return _mutate_scope_section(
        repo_root,
        scope,
        path[:-1],
        values={path[-1]: value},
    )


def parse_dotted_key(raw: str) -> tuple[str, ...]:
    """Parse one TOML dotted key, preserving quoted segments containing dots."""
    key = raw.strip()
    if not key:
        raise SpiceError("configuration set key must not be empty")
    try:
        parsed = tomllib.loads(f"{key} = 0")
    except tomllib.TOMLDecodeError as exc:
        raise SpiceError(f"invalid configuration dotted key {raw!r}: {exc}") from exc
    path: list[str] = []
    value: Any = parsed
    while isinstance(value, Mapping) and len(value) == 1:
        part, value = next(iter(value.items()))
        path.append(str(part))
    if value != 0 or len(path) < 2 or any(not part for part in path):
        raise SpiceError(f"configuration set key {raw!r} must name a dotted table leaf")
    return tuple(path)


def parse_toml_value(raw: str) -> Any:
    """Parse one authored CLI value while preserving unquoted text as a string."""
    stripped = raw.strip()
    structured = (
        stripped in {"true", "false"}
        or stripped.startswith(('"', "'", "[", "{"))
        or _TOML_NUMBER_RE.fullmatch(stripped) is not None
    )
    if not structured:
        return raw
    try:
        parsed = tomllib.loads(f"value = {stripped}")["value"]
    except tomllib.TOMLDecodeError as exc:
        raise SpiceError(f"invalid TOML configuration value {raw!r}: {exc}") from exc
    if isinstance(parsed, (str, bool, int, float, list, dict)):
        return parsed
    raise SpiceError(
        f"unsupported TOML configuration value {raw!r}; "
        "expected a string, boolean, number, array, or inline table"
    )


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
    key: str | tuple[str, ...],
    *,
    values: Mapping[str, Any] | None = None,
    clear_keys: tuple[str, ...] | None = (),
) -> Path:
    path = config_scope_path(repo_root, scope)
    if scope == WORKTREE_SOURCE:
        path = prepare_operator_state_path(repo_root, WORKTREE_CONFIG_PATH)
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
    table = key
    if values:
        _apply_worktree_table(lines, table, values)
    elif clear_keys is None:
        _remove_worktree_table(lines, table)
    else:
        _remove_table_keys(lines, table, set(clear_keys))
    text = "\n".join(lines) + "\n" if lines else ""
    try:
        parsed = tomllib.loads(text)
        validate_config_keys(parsed, source_name=scope, source_path=path)
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
    lines: list[str], table: str | tuple[str, ...], values: Mapping[str, Any]
) -> None:
    if not values:
        return
    start, end = _toml_table_bounds(lines, table)
    if start is None:
        if lines and lines[-1].strip():
            lines.append("")
        lines.append(_toml_table_header(table))
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


def _remove_worktree_table(lines: list[str], table: str | tuple[str, ...]) -> None:
    start, end = _toml_table_bounds(lines, table)
    if start is not None:
        del lines[start:end]


def _remove_table_keys(
    lines: list[str], table: str | tuple[str, ...], keys: set[str]
) -> None:
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


def _toml_table_bounds(
    lines: list[str], table: str | tuple[str, ...]
) -> tuple[int | None, int | None]:
    expected = (table,) if isinstance(table, str) else table
    start: int | None = None
    for index, line in enumerate(lines):
        name = _toml_table_name(line)
        if name == expected:
            start = index
            continue
        if start is not None and name is not None:
            return start, index
    return (start, len(lines)) if start is not None else (None, None)


def _toml_table_name(line: str) -> tuple[str, ...] | None:
    match = _TOML_TABLE_RE.match(line)
    if match is None:
        return None
    try:
        parsed = tomllib.loads(f"[{match.group(1)}]\n")
    except tomllib.TOMLDecodeError:
        return None
    path: list[str] = []
    value: Any = parsed
    while isinstance(value, Mapping) and len(value) == 1:
        part, value = next(iter(value.items()))
        path.append(str(part))
    return tuple(path) if isinstance(value, Mapping) and not value else None


def _toml_assignment_key(line: str) -> str | None:
    match = _TOML_ASSIGN_RE.match(line)
    if match is None:
        return None
    token = match.group(1)
    try:
        return str(next(iter(tomllib.loads(f"{token} = 0"))))
    except tomllib.TOMLDecodeError:
        return None


def _toml_assignment(key: str, value: Any) -> str:
    return f"{_toml_key(key)} = {_toml_scalar(value)}"


def _toml_table_header(table: str | tuple[str, ...]) -> str:
    path = (table,) if isinstance(table, str) else table
    return "[" + ".".join(_toml_key(part) for part in path) + "]"


def _toml_key(key: str) -> str:
    return key if _TOML_BARE_KEY_RE.fullmatch(key) else json.dumps(key)


def _toml_scalar(value: Any) -> str:
    """Render one CLI value as TOML without changing its parsed data shape."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return repr(value)
    if isinstance(value, Mapping):
        body = ", ".join(
            f"{_toml_key(str(key))} = {_toml_scalar(item)}"
            for key, item in value.items()
        )
        return f"{{ {body} }}"
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return "[" + ", ".join(_toml_scalar(item) for item in value) + "]"
    return json.dumps(str(value))


def git_worktree_config_get(repo_root: Path, key: str) -> str | None:
    result = run_git_command(
        ["git", "-C", str(repo_root), "config", "--worktree", "--get", key],
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        return None
    return result.stdout.strip() or None
