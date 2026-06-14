"""The Claude Code driver: launch argv, transcript location, normalization.

Claude is the second shipped driver. These assert the seam Codex already
satisfies — command shape, file-based transcript resolution, the canonical
event vocabulary every transcript consumer reads, and the per-message token
usage the context meter folds into pressure.
"""

from __future__ import annotations

import json
import subprocess

import pytest

from spice.agent.driver import (
    CLAUDE_DRIVER,
    CLAUDE_FALLBACK_CONTEXT_WINDOW,
    CODEX_DRIVER,
    PLAYWRIGHT_MCP_COMMAND,
    PLAYWRIGHT_MCP_SERVER_NAME,
    SPICE_AGENT_DRIVER_ENV,
    driver_for,
    playwright_mcp_args,
    select_driver,
)


def test_select_driver_defaults_to_codex_and_resolves_claude(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    assert select_driver().name == "codex"
    assert select_driver("claude") is CLAUDE_DRIVER
    assert select_driver("CODEX") is CODEX_DRIVER
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    assert select_driver().name == "claude"


def test_select_driver_reads_worktree_config(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    from spice.config import update_section

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    update_section(tmp_path, "agent", {"driver": "claude"})
    assert select_driver().name == "claude"


def test_driver_for_reads_each_worktree_config(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    from spice.config import update_section

    codex_repo = tmp_path / "codex-repo"
    claude_repo = tmp_path / "claude-repo"
    codex_repo.mkdir()
    claude_repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=codex_repo, check=True)
    subprocess.run(["git", "init", "-q"], cwd=claude_repo, check=True)
    update_section(claude_repo, "agent", {"driver": "claude"})

    assert driver_for(codex_repo).name == "codex"
    assert driver_for(claude_repo).name == "claude"


def test_driver_for_rejects_unknown_configured_driver(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    from spice.config import update_section

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    update_section(tmp_path, "agent", {"driver": "cloude"})

    with pytest.raises(RuntimeError, match="unknown agent driver 'cloude'"):
        driver_for(tmp_path)


def test_claude_command_starts_headless_stream_json_with_effort(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
        model="haiku",
        reasoning_effort="xhigh",
    )
    assert command[0] == "claude"
    assert command[1:5] == ["--print", "--output-format", "stream-json", "--verbose"]
    assert command[command.index("--model") + 1] == "haiku"
    assert command[command.index("--permission-mode") + 1] == "bypassPermissions"
    # Codex-shaped `xhigh` maps onto Claude's `max` rather than crashing launch.
    assert command[command.index("--effort") + 1] == "max"
    assert command[-1] == "follow the skill"


def test_claude_command_registers_playwright_mcp_server(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
        model="haiku",
    )
    payload = json.loads(command[command.index("--mcp-config") + 1])
    server = payload["mcpServers"][PLAYWRIGHT_MCP_SERVER_NAME]

    assert server["command"] == PLAYWRIGHT_MCP_COMMAND
    assert server["args"] == playwright_mcp_args(tmp_path)
    # The MCP config is a flag, not the trailing prompt.
    assert command[-1] == "follow the skill"


def test_claude_command_resumes_with_dashed_session_id(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="continue",
        thread_id="768bcba1a66f4d229ce7bcf65b5d16aa",
        model="haiku",
    )
    assert command[command.index("--resume") + 1] == (
        "768bcba1-a66f-4d22-9ce7-bcf65b5d16aa"
    )
    assert command[-1] == "continue"


def test_claude_skill_prompt_instructs_reading_the_file(tmp_path):
    skill = tmp_path / "SKILL.md"
    prompt = CLAUDE_DRIVER.skill_invocation_prompt(skill)
    assert str(skill) in prompt
    assert prompt.lower().startswith("read ")


def test_claude_transcript_resolves_by_session_glob(tmp_path, monkeypatch):
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path))
    dashed = "768bcba1-a66f-4d22-9ce7-bcf65b5d16aa"
    project = tmp_path / "projects" / "-private-tmp-spice-sup"
    project.mkdir(parents=True)
    transcript = project / f"{dashed}.jsonl"
    transcript.write_text("{}\n", encoding="utf-8")

    resolved = CLAUDE_DRIVER.thread_transcript_path("768bcba1a66f4d229ce7bcf65b5d16aa")

    assert resolved == transcript.resolve()


def test_claude_normalizes_assistant_text_into_final_message():
    raw = {
        "type": "assistant",
        "timestamp": "2026-06-14T00:30:00.000Z",
        "message": {
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "READY"}],
        },
    }
    event = CLAUDE_DRIVER.normalize_transcript_line(raw)
    assert event["type"] == "response_item"
    assert event["timestamp"] == "2026-06-14T00:30:00.000Z"
    payload = event["payload"]
    assert payload["role"] == "assistant"
    assert payload["phase"] == "final_answer"
    assert payload["content"][0]["text"] == "READY"


def test_claude_normalizes_tool_use_into_function_call():
    raw = {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "stop_reason": "tool_use",
            "content": [
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}}
            ],
        },
    }
    payload = CLAUDE_DRIVER.normalize_transcript_line(raw)["payload"]
    assert payload["type"] == "function_call"
    assert payload["name"] == "Bash"
    assert '"command": "ls"' in payload["arguments"]


def test_claude_maps_todowrite_into_update_plan():
    raw = {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "name": "TodoWrite",
                    "input": {
                        "todos": [
                            {"content": "map code", "status": "in_progress"},
                            {"content": "write tests", "status": "pending"},
                        ]
                    },
                }
            ],
        },
    }
    payload = CLAUDE_DRIVER.normalize_transcript_line(raw)["payload"]
    assert payload["name"] == "update_plan"
    assert '"step": "map code"' in payload["arguments"]
    assert '"status": "in_progress"' in payload["arguments"]


def test_claude_normalizes_thinking_and_tool_result_as_presence():
    thinking = {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "thinking", "thinking": "deliberating"}],
        },
    }
    result = {
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "tool_result", "content": "done"}],
        },
    }
    assert (
        CLAUDE_DRIVER.normalize_transcript_line(thinking)["payload"]["type"]
        == "reasoning"
    )
    assert (
        CLAUDE_DRIVER.normalize_transcript_line(result)["payload"]["type"]
        == "function_call_output"
    )


def test_claude_normalizes_tool_result_image_into_output_item():
    raw = {
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "content": [
                        {"type": "text", "text": "shot"},
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": "QUJD",
                            },
                        },
                    ],
                }
            ],
        },
    }
    payload = CLAUDE_DRIVER.normalize_transcript_line(raw)["payload"]
    assert payload["type"] == "function_call_output"
    assert payload["output"][0]["image_url"]["url"] == "data:image/png;base64,QUJD"


def test_claude_json_stdout_scanner_captures_assistant_prose():
    from spice.agent.watchdog import JsonStdoutScanner

    captured: list[str] = []
    compactions: list[int] = []
    scanner = JsonStdoutScanner(
        captured.append,
        CLAUDE_DRIVER.normalize_transcript_line,
        on_compaction=lambda: compactions.append(1),
    )
    scanner.process_line(
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"text","text":"hello operator"}]}}'
    )
    scanner.process_line(
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"tool_use","name":"Bash","input":{}}]}}'
    )
    scanner.process_line('{"type":"system","subtype":"compact_boundary"}')
    scanner.close()
    assert captured == ["hello operator"]
    assert len(compactions) == 1


def test_claude_normalizes_compaction_and_skips_app_records():
    boundary = {"type": "system", "subtype": "compact_boundary", "timestamp": "t"}
    assert CLAUDE_DRIVER.normalize_transcript_line(boundary)["type"] == "compacted"
    assert CLAUDE_DRIVER.normalize_transcript_line({"type": "summary"})["type"] == (
        "compacted"
    )
    assert CLAUDE_DRIVER.normalize_transcript_line({"type": "queue-operation"}) is None


def test_claude_context_fields_sum_prompt_and_fit_window():
    fresh, cache_read, cache_create, output = 1000, 50000, 4000, 500
    raw = {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "usage": {
                "input_tokens": fresh,
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": cache_create,
                "output_tokens": output,
            },
        },
    }
    fields = CLAUDE_DRIVER.context_snapshot_fields(raw)
    assert fields["total_tokens"] == fresh + cache_read + cache_create + output
    assert fields["cached_input_tokens"] == cache_read + cache_create
    assert fields["model_context_window"] == CLAUDE_DRIVER.default_context_window


def test_claude_context_window_grows_to_million_when_overflowing():
    cache_read, output = 355000, 2000
    raw = {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "usage": {
                "input_tokens": 0,
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": 0,
                "output_tokens": output,
            },
        },
    }
    fields = CLAUDE_DRIVER.context_snapshot_fields(raw)
    assert fields["total_tokens"] == cache_read + output
    assert fields["model_context_window"] == CLAUDE_FALLBACK_CONTEXT_WINDOW
