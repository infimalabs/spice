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
    CLAUDE_SUPERVISED_TASK_TOOLS,
    CODEX_DRIVER,
    POST_TOOL_HOOK_EVENT,
    PLAYWRIGHT_MCP_COMMAND,
    PLAYWRIGHT_MCP_SERVER_NAME,
    RATE_LIMIT_HTTP_STATUS,
    SPICE_AGENT_DRIVER_ENV,
    claude_auto_compact_environment,
    driver_for,
    playwright_mcp_args,
    post_tool_hook_config_path,
    resolve_claude_model,
    select_driver,
)
from spice.agent.paths import agent_worktree_state_dir

SPEND_LIMIT_RESET_EPOCH = 1784280000


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


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
    from spice.config.edit import set_scope_section
    from spice.config.layers import WORKTREE_SOURCE

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    set_scope_section(tmp_path, WORKTREE_SOURCE, "agent", {"driver": "claude"})
    assert select_driver().name == "claude"


def test_driver_for_reads_each_worktree_config(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    from spice.config.edit import set_scope_section
    from spice.config.layers import WORKTREE_SOURCE

    codex_repo = tmp_path / "codex-repo"
    claude_repo = tmp_path / "claude-repo"
    codex_repo.mkdir()
    claude_repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=codex_repo, check=True)
    subprocess.run(["git", "init", "-q"], cwd=claude_repo, check=True)
    set_scope_section(claude_repo, WORKTREE_SOURCE, "agent", {"driver": "claude"})

    assert driver_for(codex_repo).name == "codex"
    assert driver_for(claude_repo).name == "claude"


def test_driver_for_rejects_unknown_configured_driver(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    from spice.config.edit import set_scope_section
    from spice.config.layers import WORKTREE_SOURCE

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    set_scope_section(tmp_path, WORKTREE_SOURCE, "agent", {"driver": "cloude"})

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
    assert command[-1] == command[command.index("--append-system-prompt") + 1]


def test_claude_command_leaves_commit_attribution_to_the_harness(tmp_path):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
    )
    settings = json.loads(command[command.index("--settings") + 1])

    # Spice injects no attribution override, so Claude's native attribution
    # governs: the settings payload carries only denials and hooks.
    assert sorted(settings) == ["hooks", "permissions"]


def test_claude_settings_disable_attribution_when_repo_blocks_trailer(tmp_path):
    (tmp_path / "spice.toml").write_text(
        '[policy.commit_message]\nblocked_trailers = ["Co-Authored-By"]\n',
        encoding="utf-8",
    )
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
    )
    settings = json.loads(command[command.index("--settings") + 1])

    # A repo that explicitly blocks the attribution trailer inverts the driver
    # default: spice disables Claude's native attribution so it never emits a
    # trailer the commit-msg gate would then reject.
    assert sorted(settings) == ["attribution", "hooks", "permissions"]
    assert settings["attribution"] == {"commit": "", "sessionUrl": False}


def test_claude_settings_disable_attribution_when_allow_set_omits_trailer(tmp_path):
    (tmp_path / "spice.toml").write_text(
        '[policy.commit_message]\nallowed_trailers = ["Task"]\n',
        encoding="utf-8",
    )
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
    )
    settings = json.loads(command[command.index("--settings") + 1])

    # An explicit allow-set that omits the attribution trailer rejects it just
    # as a block does, so the driver disables native attribution to match.
    assert sorted(settings) == ["attribution", "hooks", "permissions"]
    assert settings["attribution"] == {"commit": "", "sessionUrl": False}


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

    assert post_tool_hook_config_path(tmp_path, CLAUDE_DRIVER) == (
        agent_worktree_state_dir(tmp_path) / "claude-post-tool-hook.json"
    )
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
    expected_prefix = f"{CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE}\n\n{skill_link}"
    # The skill rides Claude's system prompt every launch, prefaced so it
    # reads as binding rather than optional, carrying the same preamble and
    # relpath link as the trailing prompt — not just the bootstrap turn.
    system_prompt = command[command.index("--append-system-prompt") + 1]
    assert system_prompt.startswith(expected_prefix)
    # The trailing prompt the agent acts on gets the identical preamble --
    # still generic, not operator-specific, so the prompt boundary holds.
    assert command[-1] == system_prompt
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
    assert command[-1] == command[command.index("--append-system-prompt") + 1]


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
    assert command[-1] == command[command.index("--append-system-prompt") + 1]


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


def test_claude_driver_classifies_live_spend_limit_wording():
    # The exact rejection the 2026-07-17 credit storm streamed: the CLI exits
    # zero after this message, so text classification must recognize it
    # wherever it appears.
    assert (
        CLAUDE_DRIVER.process_failure_kind(
            exit_code=0,
            output=(
                "You've hit your monthly spend limit · "
                "raise it at claude.ai/settings/usage"
            ),
        )
        == "out-of-credits"
    )


def test_claude_stream_failure_fields_read_rejected_rate_limit_event():
    fields = CLAUDE_DRIVER.stream_failure_fields(
        {
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "rejected",
                "resetsAt": SPEND_LIMIT_RESET_EPOCH,
                "rateLimitType": "five_hour",
            },
        }
    )
    allowed = CLAUDE_DRIVER.stream_failure_fields(
        {
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "allowed",
                "resetsAt": SPEND_LIMIT_RESET_EPOCH,
            },
        }
    )

    assert fields == {
        "kind": "out-of-credits",
        "reset_epoch": SPEND_LIMIT_RESET_EPOCH,
    }
    assert allowed is None


def test_claude_stream_failure_fields_read_429_result_line():
    fields = CLAUDE_DRIVER.stream_failure_fields(
        {
            "type": "result",
            "subtype": "success",
            "is_error": True,
            "api_error_status": RATE_LIMIT_HTTP_STATUS,
            "result": (
                "You've hit your monthly spend limit · "
                "raise it at claude.ai/settings/usage"
            ),
        }
    )
    text_only = CLAUDE_DRIVER.stream_failure_fields(
        {
            "type": "result",
            "is_error": True,
            "result": "Claude AI usage limit reached",
        }
    )
    clean = CLAUDE_DRIVER.stream_failure_fields(
        {"type": "result", "subtype": "success", "is_error": False, "result": "done"}
    )

    assert fields == {"kind": "out-of-credits"}
    assert text_only == {"kind": "out-of-credits"}
    assert clean is None


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


def _write_claude_transcript(
    config_dir: Path, dashed: str, *, slug: str, cwd: str | None
) -> Path:
    """Plant a Claude transcript whose first record stamps `cwd` (or none)."""
    project = config_dir / "projects" / slug
    project.mkdir(parents=True, exist_ok=True)
    transcript = project / f"{dashed}.jsonl"
    lines = [json.dumps({"type": "queue-operation"})]
    if cwd is not None:
        lines.append(json.dumps({"type": "user", "cwd": cwd, "message": {}}))
    transcript.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return transcript


def test_claude_resumability_follows_the_recorded_cwd(tmp_path, monkeypatch):
    config_dir = tmp_path / "claude"
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(config_dir))
    home_root = tmp_path / "wt-home"
    away_root = tmp_path / "wt-away"
    home_root.mkdir()
    away_root.mkdir()
    canonical = "768bcba1a66f4d229ce7bcf65b5d16aa"
    dashed = "768bcba1-a66f-4d22-9ce7-bcf65b5d16aa"
    # `--resume` is scoped to the invoking cwd's project-slug dir, so the one
    # transcript is reachable only from the worktree that recorded it.
    _write_claude_transcript(config_dir, dashed, slug="-wt-home", cwd=str(home_root))

    resumable_home = CLAUDE_DRIVER.thread_resumable_here(home_root, canonical)
    resumable_away = CLAUDE_DRIVER.thread_resumable_here(away_root, canonical)
    foreign_home = CLAUDE_DRIVER.thread_known_foreign(home_root, canonical)
    foreign_away = CLAUDE_DRIVER.thread_known_foreign(away_root, canonical)

    # Resumable-here and foreign-here are mirror verdicts of the same transcript:
    # the worktree that recorded it can resume it; every other worktree finds it
    # foreign.
    assert resumable_home != resumable_away
    assert foreign_home != foreign_away
    assert resumable_home is True
    assert foreign_away is True


def test_claude_thread_without_a_local_transcript_is_unresumable_but_not_foreign(
    tmp_path, monkeypatch
):
    config_dir = tmp_path / "claude"
    (config_dir / "projects").mkdir(parents=True)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(config_dir))
    repo_root = tmp_path / "wt"
    repo_root.mkdir()
    # The spice-e incident id: a pointer named it, but no transcript exists
    # anywhere, so `--resume` had nothing to attach to and exited within a
    # second.
    canonical = "019f880685c07312b89f6bfc6cdd0bb5"

    resumable = CLAUDE_DRIVER.thread_resumable_here(repo_root, canonical)
    foreign = CLAUDE_DRIVER.thread_known_foreign(repo_root, canonical)

    # Unresumable, so `ensure` starts fresh; yet an absent transcript is not
    # proof the thread belongs elsewhere, so a mid-startup bind stays allowed.
    assert resumable is False
    assert foreign is False


def test_claude_thread_with_unknown_cwd_is_unresumable_but_not_foreign(
    tmp_path, monkeypatch
):
    config_dir = tmp_path / "claude"
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(config_dir))
    repo_root = tmp_path / "wt"
    repo_root.mkdir()
    canonical = "3c1d7e045a2b4f6c8d9e0a1b2c3d4e5f"
    dashed = "3c1d7e04-5a2b-4f6c-8d9e-0a1b2c3d4e5f"
    _write_claude_transcript(config_dir, dashed, slug="-unknown", cwd=None)

    verdict = (
        CLAUDE_DRIVER.thread_resumable_here(repo_root, canonical),
        CLAUDE_DRIVER.thread_known_foreign(repo_root, canonical),
    )

    # A partial/legacy transcript is not affirmative evidence that Claude can
    # reach it from this cwd, but its missing metadata also cannot prove that an
    # ambient, just-started session belongs to another worktree.
    assert verdict == (False, False)


def test_claude_json_stdout_scanner_captures_assistant_prose():
    from spice.agent.watchdog import JsonStdoutScanner

    captured: list[str] = []
    compactions: list[int] = []
    activities: list[str] = []
    scanner = JsonStdoutScanner(
        captured.append,
        CLAUDE_DRIVER,
        on_compaction=lambda: compactions.append(1),
        on_activity=lambda: activities.append("activity"),
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
    assert activities == ["activity", "activity"]


def test_claude_json_stdout_scanner_counts_an_image_turn_as_activity():
    """A turn that produces only an image is still the lane producing output.

    The startup deadline waits on the agent's first fact; a screenshot is one
    of the agent's, where a reasoning summary or returning tool output is the
    harness speaking about it.
    """
    from spice.agent.watchdog import JsonStdoutScanner

    observed: list[str] = []
    scanner = JsonStdoutScanner(
        lambda text: observed.append(f"message:{text}"),
        CLAUDE_DRIVER,
        on_activity=lambda: observed.append("activity"),
    )
    scanner.process_line(
        '{"type":"assistant","message":{"role":"assistant","content":'
        '[{"type":"image","source":{"type":"base64","media_type":"image/png",'
        '"data":"QUJD"}}]}}'
    )
    scanner.process_line(
        '{"type":"user","message":{"role":"user","content":'
        '[{"type":"tool_result","tool_use_id":"t","content":"back"}]}}'
    )
    scanner.close()
    assert observed == ["activity"]


def test_claude_json_stdout_scanner_flags_text_starvation_once_per_streak():
    from spice.agent.watchdog import TEXT_STARVATION_THRESHOLD, JsonStdoutScanner

    captured: list[str] = []
    starvations: list[int] = []
    scanner = JsonStdoutScanner(
        captured.append,
        CLAUDE_DRIVER,
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
        CLAUDE_DRIVER,
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


def test_claude_json_stdout_scanner_reports_compaction_apart_from_activity():
    from spice.agent.watchdog import JsonStdoutScanner

    observed: list[str] = []
    scanner = JsonStdoutScanner(
        lambda text: observed.append(f"message:{text}"),
        CLAUDE_DRIVER,
        on_compaction=lambda: observed.append("compacted"),
        on_activity=lambda: observed.append("activity"),
        on_compaction_active=lambda active: observed.append(f"compacting:{active}"),
    )
    # The exact stdout sequence of the stalled launch, then the assistant turn
    # a surviving compaction goes on to produce.
    scanner.process_line('{"type":"system","subtype":"status","status":"requesting"}')
    scanner.process_line('{"type":"system","subtype":"status","status":"compacting"}')
    scanner.process_line(
        '{"type":"system","subtype":"status","status":null,"compact_result":"success"}'
    )
    scanner.process_line(
        '{"type":"assistant","message":{"role":"assistant",'
        '"content":[{"type":"text","text":"back from compaction"}]}}'
    )
    scanner.close()
    assert observed == [
        "compacting:True",
        "compacting:False",
        "activity",
        "message:back from compaction",
    ]


def test_claude_json_stdout_scanner_settles_compaction_on_a_boundary():
    from spice.agent.watchdog import JsonStdoutScanner

    observed: list[str] = []
    scanner = JsonStdoutScanner(
        lambda _text: None,
        CLAUDE_DRIVER,
        on_compaction=lambda: observed.append("compacted"),
        on_compaction_active=lambda active: observed.append(f"compacting:{active}"),
    )
    scanner.process_line('{"type":"system","subtype":"compact_boundary"}')
    scanner.close()
    assert observed == ["compacted", "compacting:False"]


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
    assert fields is not None
    assert fields.last.total_tokens == fresh + cache_read + cache_create + output
    assert fields.last.cached_input_tokens == cache_read + cache_create
    assert fields.model_context_window == CLAUDE_DRIVER.default_context_window


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
    assert fields is not None
    assert fields.last.total_tokens == cache_read + output
    # Overflow no longer promotes to the 1M tier; it stays pinned at 200K so
    # pressure reads past 100% and drives compaction.
    assert fields.model_context_window == CLAUDE_DRIVER.default_context_window


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
    assert CLAUDE_SUPERVISED_TASK_TOOLS == (
        "Task",
        "Agent",
        "TaskCreate",
        "TaskGet",
        "TaskList",
        "TaskUpdate",
        "TaskOutput",
        "TaskStop",
    )
    assert CLAUDE_DENIED_TOOLS == (
        *CLAUDE_SUPERVISED_TASK_TOOLS,
        "Monitor",
    )


@pytest.mark.parametrize(
    ("thread_id", "resume_tail"),
    [
        ("", []),
        (
            "768bcba1a66f4d229ce7bcf65b5d16aa",
            ["--resume", "768bcba1-a66f-4d22-9ce7-bcf65b5d16aa"],
        ),
    ],
    ids=("initial", "resumed"),
)
def test_claude_commands_apply_task_denials_and_hooks(tmp_path, thread_id, resume_tail):
    command = CLAUDE_DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt="follow the skill",
        thread_id=thread_id,
    )
    settings = json.loads(command[command.index("--settings") + 1])

    assert command[1] == "--print"
    assert command[command.index("--permission-mode") + 1] == "bypassPermissions"
    expected_prompt = command[command.index("--append-system-prompt") + 1]
    assert command[-(len(resume_tail) + 1) :] == [*resume_tail, expected_prompt]
    assert settings["permissions"]["deny"] == [*CLAUDE_SUPERVISED_TASK_TOOLS, "Monitor"]
    assert sorted(settings) == ["hooks", "permissions"]
    hook_group = settings["hooks"][POST_TOOL_HOOK_EVENT][0]
    assert hook_group["matcher"] == "*"
    assert hook_group["hooks"][0]["statusMessage"] == "Checking spice steering"


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
