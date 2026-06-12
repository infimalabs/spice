"""Shell command records reconstructed from transcript tool calls."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.sessions.records import iter_events
from spice.sessions.util import normalize_timestamp

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
    for obj in iter_events(path):
        ts = normalize_timestamp(obj.get("timestamp")) or ""
        payload = obj.get("payload") or {}
        record_type = obj.get("type")
        if record_type == "event_msg":
            current_turn_id = _apply_event_message_record(
                records, path, ts, payload, current_turn_id
            )
            continue
        if record_type != "response_item":
            continue
        _apply_response_item_record(records, calls, path, ts, payload, current_turn_id)
    return records


def _apply_event_message_record(
    records: list[CommandRecord],
    path: Path,
    ts: str,
    payload: dict[str, Any],
    current_turn_id: str | None,
) -> str | None:
    inner = payload.get("type")
    if inner == "task_started":
        return (
            payload.get("turn_id") if isinstance(payload.get("turn_id"), str) else None
        )
    if inner == "task_complete":
        return None
    if inner == "exec_command_end" and ts:
        records.append(_command_record_from_event_payload(str(path), ts, payload))
    return current_turn_id


def _command_record_from_event_payload(
    source_file: str, ts: str, payload: dict[str, Any]
) -> CommandRecord:
    return CommandRecord(
        source_file=source_file,
        ts=ts,
        turn_id=_string_or_none(payload.get("turn_id")),
        cwd=_string_or_none(payload.get("cwd") or payload.get("workdir")),
        command=_render_command_value(payload.get("command") or payload.get("cmd")),
        exit_code=_coerce_command_int(payload.get("exit_code")),
        status=_string_or_none(payload.get("status")) or "completed",
    )


def _apply_response_item_record(
    records: list[CommandRecord],
    calls: dict[str, CommandRecord],
    path: Path,
    ts: str,
    payload: dict[str, Any],
    current_turn_id: str | None,
) -> None:
    payload_type = payload.get("type")
    if (
        payload_type in ("function_call", "custom_tool_call")
        and payload.get("name") in COMMAND_TOOL_NAMES
    ):
        _append_function_call_command_record(
            records, calls, path, ts, payload, current_turn_id
        )
        return
    if payload_type in ("function_call_output", "custom_tool_call_output"):
        _update_function_call_command_record(calls, payload)


def _append_function_call_command_record(
    records: list[CommandRecord],
    calls: dict[str, CommandRecord],
    path: Path,
    ts: str,
    payload: dict[str, Any],
    current_turn_id: str | None,
) -> None:
    if not ts:
        return
    arguments = _load_json(payload.get("arguments"))
    if not isinstance(arguments, dict):
        arguments = {}
    record = CommandRecord(
        source_file=str(path),
        ts=ts,
        turn_id=_string_or_none(payload.get("turn_id")) or current_turn_id,
        cwd=_string_or_none(arguments.get("workdir") or arguments.get("cwd")),
        command=_command_from_arguments(arguments),
        exit_code=None,
        status="called",
    )
    records.append(record)
    call_id = payload.get("call_id")
    if call_id:
        calls[str(call_id)] = record


def _update_function_call_command_record(
    calls: dict[str, CommandRecord], payload: dict[str, Any]
) -> None:
    call_id = payload.get("call_id")
    if not call_id:
        return
    record = calls.get(str(call_id))
    if record is None:
        return
    output = _render_command_value(payload.get("output"))
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
