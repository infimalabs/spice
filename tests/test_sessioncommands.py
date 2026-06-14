"""Session command extraction and wrapper/pipeline audit."""

import argparse
import json

from spice.cli.parser import build_parser
from spice.sessions import commandaudit, commandrecords
from spice.sessions.cli import _print_commands


def test_command_records_pair_function_calls_with_exit_outputs(tmp_path):
    transcript = tmp_path / "session.jsonl"
    _write_jsonl(
        transcript,
        [
            _event(
                "2026-01-01T00:00:00Z",
                "event_msg",
                {"type": "task_started", "turn_id": "turn-a"},
            ),
            _call(
                "2026-01-01T00:00:01Z",
                "call-1",
                "spice agent run -- git status",
                "/repo",
            ),
            _output("2026-01-01T00:00:02Z", "call-1", "Process exited with code 0"),
            _call(
                "2026-01-01T00:00:03Z",
                "call-2",
                "spice agent run -- rg absent",
                "/repo",
            ),
            _output("2026-01-01T00:00:04Z", "call-2", "Process exited with code 1"),
            _call(
                "2026-01-01T00:00:05Z",
                "call-3",
                "spice agent run -- sleep 5",
                "/repo",
            ),
            _output(
                "2026-01-01T00:00:06Z",
                "call-3",
                "Process running with session ID 17",
            ),
            _event(
                "2026-01-01T00:00:07Z",
                "event_msg",
                {
                    "type": "exec_command_end",
                    "turn_id": "turn-b",
                    "cwd": "/repo",
                    "command": ["spice", "agent", "run", "--", "pytest"],
                    "exit_code": "0",
                    "status": "completed",
                },
            ),
        ],
    )

    records = commandrecords.collect_command_records([transcript])
    completed = commandrecords.completed_command_records([transcript])

    assert [record.status for record in records] == [
        "completed",
        "completed",
        "running",
        "completed",
    ]
    assert [record.exit_code for record in completed] == [0, 1, 0]
    assert completed[0].turn_id == "turn-a"
    assert completed[2].command == "spice agent run -- pytest"


def test_command_pipeline_audit_uses_spice_wrapper_contract():
    assert commandaudit.command_has_shell_pipeline(
        "spice agent run -- rg foo | spice agent run -- sed -n '1,5p'"
    )
    assert commandaudit.pipeline_is_canonical_wrapper(
        "spice agent run -- rg foo | spice agent run -- sed -n '1,5p'"
    )
    assert commandaudit.pipeline_is_canonical_wrapper(
        "spice agent run -- rg foo | spice agent run -- sed -n '1,5p'"
    )
    assert commandaudit.command_is_noncanonical_pipeline(
        "spice agent run -- rg foo | sed -n '1,5p'"
    )
    assert commandaudit.command_starts_with_wrapper(
        "PATH=/tmp/bin SPICE_PROXY_BIN=rtk-test spice agent run -- git status"
    )
    assert commandaudit.pipeline_is_canonical_wrapper(
        "PATH=/tmp/bin spice agent run -- rg foo | "
        "SPICE_PROXY_BIN=rtk-test spice agent run -- sed -n '1,5p'"
    )
    assert (
        commandaudit.command_label(
            "PATH=/tmp/bin spice agent run -- rg foo | sed -n '1,5p'"
        )
        == "spice agent run"
    )
    assert not commandaudit.command_has_shell_pipeline(
        "spice agent run -- test || true"
    )
    assert not commandaudit.command_starts_with_wrapper("./agent.sh git status")

    audit = commandaudit.audit_command_records(
        [
            _command_record("git status"),
            _command_record("spice agent run -- git status"),
            _command_record("spice agent run -- task status"),
            _command_record(
                "spice agent run -- rg foo | spice agent run -- sed -n '1,5p'"
            ),
            _command_record("spice agent run -- rg foo | sed -n '1,5p'"),
            _command_record("rg foo | sed -n '1,5p'"),
        ]
    )

    assert audit.total == 6
    assert audit.wrapper_commands == 4
    assert audit.non_wrapper_commands == 2
    assert audit.shell_pipelines == 3
    assert audit.canonical_pipelines == 1
    assert audit.noncanonical_pipelines == 2
    assert audit.top_noncanonical == ["spice agent run:1", "rg:1"]


def test_session_commands_summary_reports_wrapper_and_pipeline_pressure(
    tmp_path, capsys
):
    transcript = _command_fixture(tmp_path)

    _print_commands(_command_args(summary=True), [transcript])

    output = capsys.readouterr().out
    assert "Commands\n" in output
    assert "total=4 matched=4 filters=all failed=1 wrapper=3 non_wrapper=1" in output
    assert "pipelines=2 canonical_pipelines=1 noncanonical_pipelines=1" in output
    assert "top_noncanonical=spice agent run:1" in output


def test_session_commands_filtered_summary_names_population_and_filter(
    tmp_path, capsys
):
    transcript = _command_fixture(tmp_path)

    _print_commands(_command_args(summary=True, failed=True), [transcript])

    output = capsys.readouterr().out
    assert "Commands\n" in output
    assert "total=4 matched=1 filters=failed failed=1 wrapper=1 non_wrapper=0" in output
    assert "pipelines=1 canonical_pipelines=0 noncanonical_pipelines=1" in output


def test_session_commands_filters_since_compaction_and_noncanonical_pipelines(
    tmp_path, capsys
):
    transcript = _command_fixture(tmp_path)

    _print_commands(
        _command_args(since_compaction=True, noncanonical_pipelines=True),
        [transcript],
    )

    output = capsys.readouterr().out
    assert "2026-01-01T00:00:04.000Z turn=turn-a exit=2" in output
    assert "wrapper=yes pipeline=noncanonical" in output
    assert "cmd=spice agent run -- rg foo | sed -n '1,5p'" in output


def test_session_commands_parser_exposes_current_flag_surface(tmp_path):
    parser = build_parser()

    args = parser.parse_args(
        [
            "session",
            "commands",
            str(tmp_path / "session.jsonl"),
            "--failed",
            "--pipelines",
            "--noncanonical-pipelines",
            "--since-compaction",
            "--summary",
        ]
    )

    assert args.session_action == "commands"
    assert args.failed
    assert args.pipelines
    assert args.noncanonical_pipelines
    assert args.since_compaction
    assert args.summary


def _command_fixture(tmp_path):
    transcript = tmp_path / "session.jsonl"
    _write_jsonl(
        transcript,
        [
            _event(
                "2026-01-01T00:00:00Z",
                "event_msg",
                {"type": "task_started", "turn_id": "turn-a"},
            ),
            _command_end("2026-01-01T00:00:01Z", "git status", 0),
            {"timestamp": "2026-01-01T00:00:02Z", "type": "compacted", "payload": {}},
            _command_end(
                "2026-01-01T00:00:03Z",
                "spice agent run -- rg foo | spice agent run -- sed -n '1,5p'",
                0,
            ),
            _command_end(
                "2026-01-01T00:00:04Z",
                "spice agent run -- rg foo | sed -n '1,5p'",
                2,
            ),
            _command_end("2026-01-01T00:00:05Z", "spice agent run -- pytest", 0),
        ],
    )
    return transcript


def _command_args(**overrides):
    values = {
        "failed": False,
        "pipelines": False,
        "noncanonical_pipelines": False,
        "since_compaction": False,
        "summary": False,
        "newest_first": False,
        "limit": 80,
        "max_text": 220,
    }
    values.update(overrides)
    return argparse.Namespace(**values)


def _event(timestamp: str, record_type: str, payload: dict):
    return {"timestamp": timestamp, "type": record_type, "payload": payload}


def _call(timestamp: str, call_id: str, command: str, cwd: str):
    return _event(
        timestamp,
        "response_item",
        {
            "type": "function_call",
            "name": "exec_command",
            "arguments": json.dumps({"cmd": command, "workdir": cwd}),
            "call_id": call_id,
        },
    )


def _output(timestamp: str, call_id: str, output: str):
    return _event(
        timestamp,
        "response_item",
        {"type": "function_call_output", "call_id": call_id, "output": output},
    )


def _command_end(timestamp: str, command: str, exit_code: int):
    return _event(
        timestamp,
        "event_msg",
        {
            "type": "exec_command_end",
            "turn_id": "turn-a",
            "cwd": "/repo",
            "command": command,
            "exit_code": exit_code,
            "status": "completed",
        },
    )


def _command_record(command: str):
    return commandrecords.CommandRecord(
        source_file="session.jsonl",
        ts="2026-01-01T00:00:00.000Z",
        turn_id="turn-a",
        cwd="/repo",
        command=command,
        exit_code=0,
        status="completed",
    )


def _write_jsonl(path, events):
    path.write_text(
        "".join(f"{json.dumps(event)}\n" for event in events), encoding="utf-8"
    )
