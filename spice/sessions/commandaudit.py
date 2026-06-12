"""Wrapper and pipeline audit for reconstructed shell commands."""

from __future__ import annotations

import re
import shlex
from collections import Counter
from dataclasses import dataclass
from typing import Any

from spice.sessions.commandrecords import CommandRecord

WRAPPER_SCRIPT_BASENAME = "spice.sh"


@dataclass(frozen=True)
class CommandAudit:
    total: int
    wrapper_commands: int
    non_wrapper_commands: int
    shell_pipelines: int
    canonical_pipelines: int
    noncanonical_pipelines: int
    top_noncanonical: list[str]


def audit_command_records(
    commands: list[CommandRecord], limit: int = 4
) -> CommandAudit:
    top_noncanonical: Counter[str] = Counter()
    wrapper_commands = 0
    shell_pipelines = 0
    canonical_pipelines = 0
    noncanonical_pipelines = 0
    for record in commands:
        command = record.command.strip()
        if command_starts_with_wrapper(command):
            wrapper_commands += 1
        if not command_has_shell_pipeline(command):
            continue
        shell_pipelines += 1
        if pipeline_is_canonical_wrapper(command):
            canonical_pipelines += 1
            continue
        noncanonical_pipelines += 1
        top_noncanonical[command_label(command)] += 1
    return CommandAudit(
        total=len(commands),
        wrapper_commands=wrapper_commands,
        non_wrapper_commands=len(commands) - wrapper_commands,
        shell_pipelines=shell_pipelines,
        canonical_pipelines=canonical_pipelines,
        noncanonical_pipelines=noncanonical_pipelines,
        top_noncanonical=[
            f"{label}:{count}" for label, count in top_noncanonical.most_common(limit)
        ],
    )


def command_audit_payload(audit: CommandAudit) -> dict[str, Any]:
    return {
        "total": audit.total,
        "wrapper_commands": audit.wrapper_commands,
        "non_wrapper_commands": audit.non_wrapper_commands,
        "shell_pipelines": audit.shell_pipelines,
        "canonical_pipelines": audit.canonical_pipelines,
        "noncanonical_pipelines": audit.noncanonical_pipelines,
        "top_noncanonical": audit.top_noncanonical,
    }


def command_has_shell_pipeline(command: str) -> bool:
    return len(split_shell_pipeline(command)) > 1


def command_is_noncanonical_pipeline(command: str) -> bool:
    return command_has_shell_pipeline(command) and not pipeline_is_canonical_wrapper(
        command
    )


def pipeline_is_canonical_wrapper(command: str) -> bool:
    segments = split_shell_pipeline(command)
    if len(segments) < 2:
        return False
    return all(command_segment_uses_wrapper(segment) for segment in segments)


@dataclass
class _PipelineScanState:
    segments: list[str]
    start: int = 0
    quote: str | None = None
    escaped: bool = False


def split_shell_pipeline(command: str) -> list[str]:
    state = _PipelineScanState(segments=[])
    for index, char in enumerate(command):
        _scan_pipeline_char(command, index, char, state)
    return _finalize_pipeline_segments(command, state)


def _scan_pipeline_char(
    command: str, index: int, char: str, state: _PipelineScanState
) -> None:
    if state.escaped:
        state.escaped = False
        return
    if char == "\\":
        state.escaped = True
        return
    if state.quote:
        if char == state.quote:
            state.quote = None
        return
    if char in {"'", '"'}:
        state.quote = char
        return
    if not _is_pipeline_separator(command, index, char):
        return
    _append_pipeline_segment(command, index, state)


def _is_pipeline_separator(command: str, index: int, char: str) -> bool:
    if char != "|":
        return False
    previous_char = command[index - 1] if index else ""
    next_char = command[index + 1] if index + 1 < len(command) else ""
    return previous_char != "|" and next_char != "|"


def _append_pipeline_segment(
    command: str, index: int, state: _PipelineScanState
) -> None:
    state.segments.append(command[state.start : index].strip())
    state.start = index + 1


def _finalize_pipeline_segments(command: str, state: _PipelineScanState) -> list[str]:
    if not state.segments:
        return [command.strip()]
    state.segments.append(command[state.start :].strip())
    return state.segments


def command_segment_uses_wrapper(command: str) -> bool:
    parts = shell_command_parts(command)
    return _parts_start_with_wrapper(parts)


def command_starts_with_wrapper(command: str) -> bool:
    parts = shell_command_parts(command)
    return _parts_start_with_wrapper(parts)


def shell_command_parts(command: str) -> list[str]:
    try:
        return shlex.split(command)
    except ValueError:
        return command.split()


def _parts_start_with_wrapper(parts: list[str]) -> bool:
    parts = _strip_leading_env_assignments(parts)
    if not parts:
        return False
    if _is_wrapper_script_launcher(parts[0]):
        return True
    return _parts_start_with_spice_agent_run(parts)


def _parts_start_with_spice_agent_run(parts: list[str]) -> bool:
    if len(parts) < 3:
        return False
    if parts[:3] == ["spice", "agent", "run"]:
        return len(parts) == 3 or parts[3] == "--"
    if len(parts) >= 5 and parts[:5] == ["uv", "run", "spice", "agent", "run"]:
        return len(parts) == 5 or parts[5] == "--"
    return False


def _is_wrapper_script_launcher(command_name: str) -> bool:
    return command_name == f"./{WRAPPER_SCRIPT_BASENAME}" or command_name.endswith(
        f"/{WRAPPER_SCRIPT_BASENAME}"
    )


def _strip_leading_env_assignments(parts: list[str]) -> list[str]:
    index = 0
    while index < len(parts) and _is_env_assignment(parts[index]):
        index += 1
    return parts[index:]


def _is_env_assignment(part: str) -> bool:
    name, separator, _value = part.partition("=")
    return bool(separator and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name))


def command_label(command: str) -> str:
    parts = _strip_leading_env_assignments(shell_command_parts(command))
    if not parts:
        return "-"
    if _is_wrapper_script_launcher(parts[0]) and len(parts) > 1:
        return f"{WRAPPER_SCRIPT_BASENAME} {parts[1]}"
    if _parts_start_with_spice_agent_run(parts):
        return "spice agent run"
    return parts[0]
