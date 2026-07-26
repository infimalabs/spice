"""Shell command records reconstructed from transcript tool calls."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.sessions import records as session_records
from spice.transcript.events import (
    CommandExecution,
    ToolCall,
    ToolOutput,
    TurnBoundary,
)
from spice.transcript.reader import TranscriptEventReader
from spice.transcript.timestamps import normalize_timestamp

EXEC_EXIT_RE = re.compile(r"Process exited with code (-?\d+)")
COMMAND_TOOL_NAMES = {"exec_command", "shell", "local_shell", "container.exec"}


@dataclass(slots=True)
class CommandRecord:
    source_file: str
    ts: str
    turn_id: str | None
    cwd: str | None
    command: str
    exit_code: int | None
    status: str | None


def collect_command_records(files: list[Path]) -> list[CommandRecord]:
    records: list[CommandRecord] = []
    for path in files:
        records.extend(_collect_command_records_for_file(path))
    records.sort(key=lambda record: (record.ts, record.source_file))
    return records


def completed_command_records(files: list[Path]) -> list[CommandRecord]:
    return [
        record
        for record in collect_command_records(files)
        if command_record_completed(record)
    ]


def command_record_completed(record: CommandRecord) -> bool:
    return (record.status or "").lower() == "completed" or record.exit_code is not None


def command_record_failed(record: CommandRecord) -> bool:
    return command_record_completed(record) and record.exit_code not in (None, 0)


def _collect_command_records_for_file(path: Path) -> list[CommandRecord]:
    records: list[CommandRecord] = []
    calls: dict[str, CommandRecord] = {}
    current_turn_id: str | None = None
    driver = session_records.driver_for_transcript(path)
    read = TranscriptEventReader(path, driver, source_actor=None).read("forward")
    for event in read.events:
        ts = normalize_timestamp(event.at.timestamp) or ""
        if isinstance(event, TurnBoundary):
            current_turn_id = event.turn_id if event.kind == "started" else None
            continue
        if isinstance(event, CommandExecution):
            if ts:
                records.append(_command_record_from_execution(path, ts, event))
            continue
        if isinstance(event, ToolCall):
            _append_function_call_command_record(
                records, calls, path, ts, event, current_turn_id
            )
            continue
        if isinstance(event, ToolOutput):
            _update_function_call_command_record(calls, event)
    return records


def _command_record_from_execution(
    path: Path,
    ts: str,
    event: CommandExecution,
) -> CommandRecord:
    return CommandRecord(
        source_file=str(path),
        ts=ts,
        turn_id=event.turn_id,
        cwd=event.cwd,
        command=event.command,
        exit_code=event.exit_code,
        status=event.status,
    )


def _append_function_call_command_record(
    records: list[CommandRecord],
    calls: dict[str, CommandRecord],
    path: Path,
    ts: str,
    event: ToolCall,
    current_turn_id: str | None,
) -> None:
    if not ts or event.name not in COMMAND_TOOL_NAMES:
        return
    arguments = _load_json(event.arguments)
    if not isinstance(arguments, dict):
        arguments = {}
    record = CommandRecord(
        source_file=str(path),
        ts=ts,
        turn_id=event.turn_id or current_turn_id,
        cwd=_string_or_none(arguments.get("workdir") or arguments.get("cwd")),
        command=_command_from_arguments(arguments),
        exit_code=None,
        status="called",
    )
    records.append(record)
    if event.call_id:
        calls[event.call_id] = record


def _update_function_call_command_record(
    calls: dict[str, CommandRecord], event: ToolOutput
) -> None:
    if not event.call_id:
        return
    record = calls.get(event.call_id)
    if record is None:
        return
    output = event.content
    if match := EXEC_EXIT_RE.search(output):
        record.exit_code = _coerce_command_int(match.group(1))
        record.status = "completed"
        return
    if "Process running with session ID" in output:
        record.status = "running"


def _command_from_arguments(arguments: dict[str, Any]) -> str:
    for key in ("cmd", "command"):
        value = arguments.get(key)
        rendered = _render_command_value(value)
        if rendered != "-":
            return rendered
    return "-"


def _render_command_value(command: Any) -> str:
    if isinstance(command, list):
        return " ".join(str(part) for part in command)
    if isinstance(command, str):
        return command
    return "-"


def _load_json(value: Any) -> Any:
    if not isinstance(value, str):
        return None
    try:
        return json.loads(value)
    except json.JSONDecodeError:
        return None


def _coerce_command_int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError:
            return None
    return None


def _string_or_none(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None
