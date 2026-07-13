"""The Claude Code driver: launch argv, transcript location, normalization.

Claude is the second shipped driver. These assert the seam Codex already
satisfies — command shape, file-based transcript resolution, the canonical
event vocabulary every transcript consumer reads, and the per-message token
usage the context meter folds into pressure.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import (
    CLAUDE_AUTO_COMPACT_WINDOW_ENV,
    CLAUDE_AUTO_COMPACT_WINDOW_TOKENS,
    CLAUDE_DENIED_TOOLS,
    CLAUDE_DRIVER,
    CLAUDE_NATIVE_TASK_TOOLS,
    CLAUDE_NO_SUBAGENT_TOOLS,
    CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE,
    CODEX_DRIVER,
    POST_TOOL_HOOK_EVENT,
    PLAYWRIGHT_MCP_COMMAND,
    PLAYWRIGHT_MCP_SERVER_NAME,
    SPICE_AGENT_DRIVER_ENV,
    claude_auto_compact_environment,
    driver_for,
    playwright_mcp_args,
    post_tool_hook_config_path,
    resolve_claude_model,
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
    from spice.config import set_worktree_section

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    set_worktree_section(tmp_path, "agent", {"driver": "claude"})
    assert select_driver().name == "claude"


def test_driver_for_reads_each_worktree_config(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    from spice.config import set_worktree_section

    codex_repo = tmp_path / "codex-repo"
    claude_repo = tmp_path / "claude-repo"
    codex_repo.mkdir()
    claude_repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=codex_repo, check=True)
    subprocess.run(["git", "init", "-q"], cwd=claude_repo, check=True)
    set_worktree_section(claude_repo, "agent", {"driver": "claude"})

    assert driver_for(codex_repo).name == "codex"
    assert driver_for(claude_repo).name == "claude"


def test_driver_for_rejects_unknown_configured_driver(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    from spice.config import set_worktree_section

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    set_worktree_section(tmp_path, "agent", {"driver": "cloude"})

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
    assert "--include-partial-messages" in command
    assert command[command.index("--model") + 1] == "haiku"
    assert command[command.index("--permission-mode") + 1] == "bypassPermissions"
    assert command[command.index("--effort") + 1] == "xhigh"
    assert command[-1] == f"{CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE}\n\nfollow the skill"


def test_claude_command_disables_commit_attribution(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
    )
    settings = json.loads(command[command.index("--settings") + 1])

    assert settings["attribution"]["commit"] == ""
    assert settings["attribution"]["sessionUrl"] is False


def test_claude_command_writes_post_tool_hook_settings(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
    )
    settings = json.loads(command[command.index("--settings") + 1])
    hook_config = json.loads(
        post_tool_hook_config_path(tmp_path, CLAUDE_DRIVER).read_text(encoding="utf-8")
    )
    group = settings["hooks"][POST_TOOL_HOOK_EVENT][0]
    hook = group["hooks"][0]

    assert hook_config["event"] == POST_TOOL_HOOK_EVENT
    assert hook_config["matcher"] == "*"
    assert "spice agent post-tool-hook" in hook_config["command"]
    assert str(tmp_path) in hook_config["command"]
    assert group["matcher"] == hook_config["matcher"]
    assert hook["command"] == hook_config["command"]
    assert hook["statusMessage"] == "Checking spice steering"


def test_claude_command_uses_shipped_claude_opus_xhigh_defaults(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
    )

    assert command[command.index("--model") + 1] == "claude-opus-4-8"
    assert command[command.index("--effort") + 1] == "xhigh"


def test_claude_command_passes_sonnet_family_alias(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
        model="sonnet",
    )

    assert resolve_claude_model("sonnet") == "sonnet"
    assert command[command.index("--model") + 1] == "sonnet"


def test_claude_command_preserves_explicit_full_model(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
        model="claude-sonnet-4-6",
    )

    assert resolve_claude_model("claude-sonnet-4-6") == "claude-sonnet-4-6"
    assert command[command.index("--model") + 1] == "claude-sonnet-4-6"


def test_claude_command_appends_skill_to_system_prompt(tmp_path):
    skill_link = "[$spice](.agents/skills/spice/SKILL.md)"
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt=skill_link,
        model="haiku",
    )
    expected = f"{CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE}\n\n{skill_link}"
    # The skill rides Claude's system prompt every launch, prefaced so it
    # reads as binding rather than optional, carrying the same preamble and
    # relpath link as the trailing prompt — not just the bootstrap turn.
    assert command[command.index("--append-system-prompt") + 1] == expected
    # The trailing prompt the agent acts on gets the identical preamble --
    # still generic, not operator-specific, so the prompt boundary holds.
    assert command[-1] == expected
    assert command.index("--append-system-prompt") < len(command) - 1


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
    assert command[-1] == f"{CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE}\n\nfollow the skill"


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
    assert command[-1] == f"{CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE}\n\ncontinue"


def test_claude_user_event_carries_prompt_id_as_turn_boundary():
    event = CLAUDE_DRIVER.normalize_transcript_line(
        {
            "type": "user",
            "timestamp": "2026-01-01T00:00:00Z",
            "promptId": "prompt-xyz",
            "message": {"content": "operator prompt"},
        }
    )
    assert event is not None
    assert event["payload"]["prompt_id"] == "prompt-xyz"

    # Tool-result `user` lines are not prompts, so they carry no turn id.
    tool_result = CLAUDE_DRIVER.normalize_transcript_line(
        {
            "type": "user",
            "timestamp": "2026-01-01T00:00:01Z",
            "promptId": "prompt-xyz",
            "message": {"content": [{"type": "tool_result", "content": "ok"}]},
        }
    )
    assert tool_result is None or "prompt_id" not in tool_result.get("payload", {})


def test_claude_driver_classifies_out_of_credits_output():
    assert (
        CLAUDE_DRIVER.process_failure_kind(
            exit_code=1,
            output="Error: Claude AI usage limit reached for this account.",
        )
        == "out-of-credits"
    )
    assert (
        CLAUDE_DRIVER.process_failure_kind(
            exit_code=1,
            output="Error: generic command failure",
        )
        == ""
    )


def test_claude_skill_prompt_matches_codex_link_form():
    skill = Path(".agents") / "skills" / "spice" / "SKILL.md"

    assert CLAUDE_DRIVER.skill_invocation_prompt(skill) == (
        CODEX_DRIVER.skill_invocation_prompt(skill)
    )
    assert CLAUDE_DRIVER.skill_invocation_prompt(skill) == (
        "[$spice](.agents/skills/spice/SKILL.md)"
    )


def test_driver_post_tool_hook_capabilities_name_codex_gap():
    codex_hook = CODEX_DRIVER.post_tool_hook
    claude_hook = CLAUDE_DRIVER.post_tool_hook

    assert codex_hook is not None
    assert "hooks.PostToolUse" in codex_hook.config_surface
    assert codex_hook.supported_tools == ("Bash", "apply_patch", "MCP")
    assert "WebSearch" in codex_hook.unsupported_tools
    assert "non-MCP native tools" in codex_hook.unsupported_tools
    assert codex_hook.native_non_shell_complete is False
    assert "WebSearch" in codex_hook.note

    assert claude_hook is not None
    assert "PostToolUse" in claude_hook.config_surface
    assert claude_hook.unsupported_tools == ()
    assert claude_hook.native_non_shell_complete is True
    assert claude_hook.context_output_field == "hookSpecificOutput.additionalContext"


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


def test_claude_normalizes_assistant_text_after_thinking_block():
    raw = {
        "type": "assistant",
        "timestamp": "2026-06-14T00:30:00.000Z",
        "message": {
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": "working"},
                {"type": "text", "text": "ACK 20260614T003000000000Z: done."},
            ],
        },
    }

    payload = CLAUDE_DRIVER.normalize_transcript_line(raw)["payload"]

    assert payload["type"] == "message"
    assert payload["role"] == "assistant"
    assert payload["phase"] == "final_answer"
    assert payload["content"][0]["text"] == "ACK 20260614T003000000000Z: done."


def test_claude_normalizes_tool_use_into_function_call():
    raw = {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "choosing command"},
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}},
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


def test_claude_json_stdout_scanner_flags_text_starvation_once_per_streak():
    from spice.agent.watchdog import TEXT_STARVATION_THRESHOLD, JsonStdoutScanner

    captured: list[str] = []
    starvations: list[int] = []
    scanner = JsonStdoutScanner(
        captured.append,
        CLAUDE_DRIVER.normalize_transcript_line,
        on_compaction=lambda: None,
        on_text_starvation=starvations.append,
    )
    # Observed failure shape: long stretches of thinking-only and tool_use
    # responses whose canonical assistant events carry no text block at all.
    thinking_line = (
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"thinking","thinking":"planning"}]}}'
    )
    tool_line = (
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"tool_use","name":"Bash","input":{}}]}}'
    )
    for _ in range(TEXT_STARVATION_THRESHOLD - 1):
        scanner.process_line(thinking_line)
        scanner.process_line(tool_line)
    assert starvations == []
    scanner.process_line(tool_line)
    assert starvations == [TEXT_STARVATION_THRESHOLD]
    # The streak keeps growing but the callback stays fired-once.
    scanner.process_line(tool_line)
    assert starvations == [TEXT_STARVATION_THRESHOLD]
    assert captured == []


def test_claude_json_stdout_scanner_text_resets_starvation_streak():
    from spice.agent.watchdog import TEXT_STARVATION_THRESHOLD, JsonStdoutScanner

    starvations: list[int] = []
    scanner = JsonStdoutScanner(
        lambda _text: None,
        CLAUDE_DRIVER.normalize_transcript_line,
        on_compaction=lambda: None,
        on_text_starvation=starvations.append,
    )
    tool_line = (
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"tool_use","name":"Bash","input":{}}]}}'
    )
    text_line = (
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"text","text":"ACK k: narrating"}]}}'
    )
    for _ in range(TEXT_STARVATION_THRESHOLD - 1):
        scanner.process_line(tool_line)
    scanner.process_line(text_line)
    for _ in range(TEXT_STARVATION_THRESHOLD - 1):
        scanner.process_line(tool_line)
    assert starvations == []
    # A fresh streak after the reset can fire again.
    scanner.process_line(tool_line)
    assert starvations == [TEXT_STARVATION_THRESHOLD]


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


def test_claude_context_window_stays_at_standard_tier_when_overflowing():
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
    # Overflow no longer promotes to the 1M tier; it stays pinned at 200K so
    # pressure reads past 100% and drives compaction.
    assert fields["model_context_window"] == CLAUDE_DRIVER.default_context_window


def test_claude_tool_inventory_keeps_the_no_subagent_boundary_distinct():
    assert CLAUDE_NO_SUBAGENT_TOOLS == ("Task", "Agent")
    assert CLAUDE_NATIVE_TASK_TOOLS == (
        "TaskCreate",
        "TaskGet",
        "TaskList",
        "TaskUpdate",
        "TaskOutput",
        "TaskStop",
    )
    assert CLAUDE_DENIED_TOOLS == (
        *CLAUDE_NO_SUBAGENT_TOOLS,
        *CLAUDE_NATIVE_TASK_TOOLS,
        "Monitor",
    )


def test_claude_supervised_command_denies_complete_lifecycle_inventory(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
    )
    settings = json.loads(command[command.index("--settings") + 1])

    assert command[1] == "--print"
    assert settings["permissions"]["deny"] == [
        "Task",
        "Agent",
        "TaskCreate",
        "TaskGet",
        "TaskList",
        "TaskUpdate",
        "TaskOutput",
        "TaskStop",
        "Monitor",
    ]


def test_claude_auto_compact_environment_sets_a_default_window(tmp_path, monkeypatch):
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    env = claude_auto_compact_environment(tmp_path, base_env={})
    assert env == {
        CLAUDE_AUTO_COMPACT_WINDOW_ENV: str(CLAUDE_AUTO_COMPACT_WINDOW_TOKENS)
    }


def test_claude_auto_compact_environment_is_a_noop_for_codex(tmp_path, monkeypatch):
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "codex")
    assert claude_auto_compact_environment(tmp_path, base_env={}) == {}


def test_claude_auto_compact_environment_never_overrides_an_explicit_value(
    tmp_path, monkeypatch
):
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    base_env = {CLAUDE_AUTO_COMPACT_WINDOW_ENV: "50000"}
    assert claude_auto_compact_environment(tmp_path, base_env=base_env) == {}


def test_claude_command_appends_the_steering_token_to_the_system_prompt(tmp_path):
    import subprocess

    from spice.mail.steeringkey import steering_token

    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    token = steering_token(tmp_path)

    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path, prompt="follow the skill"
    )
    system_prompt = command[command.index("--append-system-prompt") + 1]

    # The agent sees the same <token> in the system prompt as in live steering.
    assert f"<{token}>" in system_prompt
    assert f"steering key for this worktree is {token}" in system_prompt
    assert command[-1] == system_prompt  # trailing prompt mirrors it


def test_claude_command_system_prompt_unchanged_without_a_worktree_token(tmp_path):
    # A non-repo path yields no token, so the prompt carries no steering line.
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path / "not-a-repo", prompt="follow the skill"
    )
    system_prompt = command[command.index("--append-system-prompt") + 1]
    assert "steering key for this worktree" not in system_prompt
