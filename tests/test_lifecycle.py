"""Agent lifecycle, wrapper routing, and supervisor contracts."""

import argparse
from datetime import UTC, datetime
import io
import json
import os
from pathlib import Path
import re
import subprocess
from types import SimpleNamespace

import pytest

from spice import config
from spice.agent import driver as agent_driver
from spice.agent import cli as agent_cli
from spice.agent import (
    lifecycle,
    renewal,
    sidechannel,
    sidechannelnotify,
    watchdog,
    wrap,
)
from spice.agent.driver import (
    CLAUDE_DRIVER,
    CODEX_DRIVER,
    DRIVER,
    POST_TOOL_HOOK_EVENT,
    PLAYWRIGHT_MCP_ARGS,
    PLAYWRIGHT_MCP_COMMAND,
    PLAYWRIGHT_MCP_SERVER_NAME,
    operator_color_scheme,
    playwright_mcp_args,
    post_tool_hook_config_path,
    write_playwright_mcp_config,
)
from spice.agent.maximmetrics import (
    MAXIM_EVENT_FIRE,
    MaximMetricEventWrite,
    record_maxim_metric_events,
)
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.mail.ackstate import ACK_DISPOSITION_ACKED, ack_state_records
from spice.mail.inbox import collect_inbox_items, compose_inbox_text, write_inbox_item
from spice.tasks import ops

DIRECT_AGENT_PID = 2222
SUPERVISOR_PID = 3333
SUPERVISED_AGENT_PID = 4444
SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
SHELL_HOOK_FAILURE_EXIT_CODE = 127
WORKING_STATE_ELAPSED_SECONDS = 90


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def test_agent_help_lists_show_and_status_commands():
    parser = build_parser()
    subparsers = next(
        action
        for action in parser._actions
        if isinstance(action, argparse._SubParsersAction)
    )
    help_text = subparsers.choices["agent"].format_help()

    assert "show" in help_text
    assert "Show the bound agent's state." in help_text
    assert "status" in help_text
    assert "Compatibility alias for agent show." in help_text


def test_agent_show_and_status_parse_to_agent_handler():
    show = build_parser().parse_args(["agent", "show"])
    status = build_parser().parse_args(["agent", "status"])

    assert show.agent_action == "show"
    assert show.func == agent_cli.handle_agent
    assert status.agent_action == "status"
    assert status.func == agent_cli.handle_agent


def test_agent_show_and_status_render_same_bound_agent_state(
    tmp_path, monkeypatch, capsys
):
    status = SimpleNamespace(
        repo_root=tmp_path,
        process_status="running",
        pid=123,
        process_group_id=456,
        thread_id="thread-agent",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="fast",
        started_at="2026-07-03T00:00:00Z",
        prompt_skill_path=tmp_path / "skill.md",
        log_path=tmp_path / "agent.log",
    )
    calls: list[Path] = []

    def fake_agent_status(repo_root: Path):
        calls.append(repo_root)
        return status

    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(lifecycle, "agent_status", fake_agent_status)

    show_args = build_parser().parse_args(["agent", "show"])
    assert agent_cli.handle_agent(show_args) == 0
    show_output = capsys.readouterr().out

    status_args = build_parser().parse_args(["agent", "status"])
    assert agent_cli.handle_agent(status_args) == 0
    status_output = capsys.readouterr().out

    expected = "\n".join(
        [
            f"worktree={tmp_path}",
            "status=running",
            "pid=123",
            "pgid=456",
            "thread=thread-agent",
            "model=gpt-test effort=high service_tier=fast",
            "started_at=2026-07-03T00:00:00Z",
            f"skill={tmp_path / 'skill.md'}",
            f"log={tmp_path / 'agent.log'}",
            "",
        ]
    )
    assert show_output == expected
    assert status_output == expected
    assert calls == [tmp_path, tmp_path]


def test_shipped_agent_defaults_are_current_high_effort():
    assert CODEX_DRIVER.default_model == "gpt-5.5"
    assert CODEX_DRIVER.default_reasoning_effort == "xhigh"
    assert CODEX_DRIVER.default_service_tier == ""
    assert CLAUDE_DRIVER.default_model == "claude-sonnet-5"
    assert CLAUDE_DRIVER.default_reasoning_effort == "xhigh"


def test_new_driver_value_supplies_turn_id_and_tool_rewrite_to_consumers(
    tmp_path, monkeypatch
):
    class ThirdDriver(agent_driver.AgentDriver):
        def home(self) -> Path:
            return tmp_path / "third-home"

        def thread_transcript_path(
            self, thread_id: str, *, must_exist: bool = True
        ) -> Path:
            del must_exist
            return tmp_path / f"{thread_id}.jsonl"

        def current_turn_id(self, env):
            return env.get("FAKEENV_THIRD_TURN_ID")

        def rewrite_tool_command(self, command_text, rewrite_command):
            if not command_text.startswith("third:"):
                return None
            rewritten = rewrite_command(command_text.removeprefix("third:"))
            return f"third:{rewritten}" if rewritten else None

    third_driver = ThirdDriver(
        name="third",
        default_bin="third",
        bin_env="FAKEENV_THIRD_BIN",
        thread_id_env="FAKEENV_THIRD_THREAD_ID",
        default_model="third-model",
        default_reasoning_effort="",
        default_service_tier="",
        stdout_assistant_marker="third",
        stdout_section_markers=frozenset(),
        stdout_compaction_marker="",
        session_id_pattern=re.compile(r"^third-session$"),
    )
    monkeypatch.setenv("FAKEENV_THIRD_TURN_ID", "turn-third")
    monkeypatch.setattr(ops, "ambient_thread", lambda: ("thread-third", third_driver))
    monkeypatch.setattr(ops.config, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(ops.tw, "current_branch", lambda: "main")
    monkeypatch.setattr(ops.tw, "claim_head", lambda: "head-third")

    claim = ops.claim_meta("actor-third")

    assert "claim_thread:thread-third" in claim
    assert "claim_context_turn:turn-third" in claim

    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str) -> str | None:
        calls.append(args)
        return "rtk third inner" if args == ("third inner",) else None

    monkeypatch.setattr(wrap, "driver_for", lambda _repo_root: third_driver)
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(
        ["zsh", "-c", "third:third inner"], repo_root=tmp_path, rewrite_rtk=True
    )

    assert command == ["zsh", "-c", "third:rtk third inner"]
    assert calls == [("third:third inner",), ("third inner",)]


def test_codex_driver_command_honors_explicit_fast_service_tier_and_playwright_mcp(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "dark")
    prompt = "[$spice](/tmp/skill.md)"
    command = DRIVER.build_exec_command(
        repo_root=tmp_path,
        prompt=prompt,
        thread_id="thread-1",
        model="gpt-test",
        reasoning_effort="xhigh",
        personality="pragmatic",
        service_tier="fast",
        binary="codex-test",
        fast_mode=True,
    )
    configs = _config_values(command)

    assert command[:5] == ["codex-test", "exec", "--cd", str(tmp_path), "--model"]
    assert command[5] == "gpt-test"
    assert 'model_reasoning_effort="xhigh"' in configs
    assert (
        f'mcp_servers.{PLAYWRIGHT_MCP_SERVER_NAME}.command="{PLAYWRIGHT_MCP_COMMAND}"'
    ) in configs
    assert (
        f"mcp_servers.{PLAYWRIGHT_MCP_SERVER_NAME}.args="
        f'["--yes","@playwright/mcp@latest","--headless","--config",'
        f'"{tmp_path / ".spice" / "agent" / "playwright-mcp.json"}"]'
    ) in configs
    hook_config_path = post_tool_hook_config_path(tmp_path, DRIVER)
    hook_config = json.loads(hook_config_path.read_text(encoding="utf-8"))
    hook_overrides = [
        config for config in configs if config.startswith("hooks.PostToolUse=")
    ]
    assert len(hook_overrides) == 1
    assert hook_config["event"] == POST_TOOL_HOOK_EVENT
    assert hook_config["matcher"] == "^(Bash|apply_patch|Edit|Write|mcp__.*)$"
    assert "spice agent post-tool-hook" in hook_config["command"]
    assert str(tmp_path) in hook_config["command"]
    assert hook_config["command"] in hook_overrides[0]
    assert list(PLAYWRIGHT_MCP_ARGS) == [
        "--yes",
        "@playwright/mcp@latest",
        "--headless",
    ]
    assert json.loads(
        (tmp_path / ".spice" / "agent" / "playwright-mcp.json").read_text(
            encoding="utf-8"
        )
    ) == {"browser": {"contextOptions": {"colorScheme": "dark"}}}
    assert 'personality="pragmatic"' in configs
    assert 'service_tier="fast"' in configs
    assert command[command.index("--enable") + 1] == "fast_mode"
    assert command[-3:] == ["resume", "thread-1", prompt]


def test_post_tool_hook_config_requires_driver_capability(tmp_path):
    driver = agent_driver.AgentDriver(
        name="unsupported",
        default_bin="unsupported",
        bin_env="FAKEENV_THIRD_BIN",
        thread_id_env="FAKEENV_THIRD_THREAD_ID",
        default_model="model",
        default_reasoning_effort="",
        default_service_tier="",
        stdout_assistant_marker="",
        stdout_section_markers=frozenset(),
        stdout_compaction_marker="",
        session_id_pattern=re.compile("unsupported"),
    )

    with pytest.raises(SpiceError, match="does not declare supported PostToolUse"):
        agent_driver.write_post_tool_hook_config(tmp_path, driver)


def test_post_tool_hook_response_renders_pending_steering(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000005Z.txt",
        compose_inbox_text(body="hook-delivered steering", priority=None, stop=False),
    )

    response = json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    payload = response["hookSpecificOutput"]

    assert payload["hookEventName"] == POST_TOOL_HOOK_EVENT
    assert "Inbox Steering" in payload["additionalContext"]
    assert "hook-delivered steering" in payload["additionalContext"]


def test_post_tool_hook_response_suppresses_recently_rendered_pending_steering(
    tmp_path,
):
    write_inbox_item(
        tmp_path,
        "20260101T000000000005Z.txt",
        compose_inbox_text(body="hook-suppressed steering", priority=None, stop=False),
    )

    first = json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    second = json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    first_context = first["hookSpecificOutput"]["additionalContext"]
    second_context = second["hookSpecificOutput"]["additionalContext"]

    assert "hook-suppressed steering" in first_context
    assert second_context.splitlines() == [
        "Inbox Steering",
        "  pending=1 (recently shown; full readout on repeat or run "
        "`spice session briefing`)",
    ]


def test_post_tool_hook_response_renders_new_pending_key_after_suppressed_key(
    tmp_path,
):
    write_inbox_item(
        tmp_path,
        "20260101T000000000005Z.txt",
        compose_inbox_text(body="first hook steering", priority=None, stop=False),
    )
    json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    write_inbox_item(
        tmp_path,
        "20260101T000000000006Z.txt",
        compose_inbox_text(body="second hook steering", priority=None, stop=False),
    )

    response = json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    context = response["hookSpecificOutput"]["additionalContext"]

    assert "key=20260101T000000000005Z: age=" in context
    assert "(shown earlier; ACK to clear)" in context
    assert "second hook steering" in context


def test_hook_delivered_steering_retires_from_assistant_ack(tmp_path, monkeypatch):
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    monkeypatch.setattr(
        watchdog,
        "publish_maxim_hits_as_inbox",
        lambda _repo, _text, **_kwargs: [],
    )
    key = "20260101T000000000007Z"
    inbox_name = f"{key}.txt"
    inbox_text = compose_inbox_text(
        body="hook-delivered ack target", priority=None, stop=False
    )
    write_inbox_item(tmp_path, inbox_name, inbox_text)

    delivered = json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    assert (
        "hook-delivered ack target"
        in delivered["hookSpecificOutput"]["additionalContext"]
    )

    ack_text = f"ACK {key}: processed hook steering"
    watchdog.process_supervised_assistant_message(
        tmp_path,
        ack_text,
        io.StringIO(),
        watchdog.MaximReminderGate(),
    )

    assert collect_inbox_items(tmp_path) == []
    records = ack_state_records(tmp_path)
    assert [
        (
            record.key,
            record.inbox_name,
            record.text,
            record.ack_text,
            record.ack_content,
            record.disposition,
        )
        for record in records
    ] == [
        (
            key,
            inbox_name,
            inbox_text,
            ack_text,
            "processed hook steering",
            ACK_DISPOSITION_ACKED,
        )
    ]
    assert agent_cli.render_post_tool_hook_response(tmp_path) == ""
    command_stderr = io.StringIO()
    wrap.AgentInboxInjector(tmp_path, stderr=command_stderr).inject(force=True)
    assert key not in command_stderr.getvalue()
    assert "hook-delivered ack target" not in command_stderr.getvalue()


def test_post_tool_hook_steering_end_to_end_without_shell_readout(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    monkeypatch.setattr(
        watchdog,
        "publish_maxim_hits_as_inbox",
        lambda _repo, _text, **_kwargs: [],
    )
    key = "20260101T000000000008Z"
    inbox_name = f"{key}.txt"
    inbox_text = compose_inbox_text(
        body="non-shell hook steering", priority=None, stop=False
    )
    write_inbox_item(tmp_path, inbox_name, inbox_text)

    first = json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    first_context = first["hookSpecificOutput"]["additionalContext"]
    second = json.loads(agent_cli.render_post_tool_hook_response(tmp_path))
    second_context = second["hookSpecificOutput"]["additionalContext"]

    assert "key=20260101T000000000008Z: age=" in first_context
    assert "non-shell hook steering" in first_context
    assert "key=20260101T000000000008Z" not in second_context
    assert "non-shell hook steering" not in second_context
    assert second_context.splitlines() == [
        "Inbox Steering",
        "  pending=1 (recently shown; full readout on repeat or run "
        "`spice session briefing`)",
    ]

    ack_text = f"ACK {key}: handled before another shell command"
    watchdog.process_supervised_assistant_message(
        tmp_path,
        ack_text,
        io.StringIO(),
        watchdog.MaximReminderGate(),
    )

    assert collect_inbox_items(tmp_path) == []
    records = ack_state_records(tmp_path)
    assert [
        (
            record.key,
            record.inbox_name,
            record.text,
            record.ack_text,
            record.ack_content,
            record.disposition,
        )
        for record in records
    ] == [
        (
            key,
            inbox_name,
            inbox_text,
            ack_text,
            "handled before another shell command",
            ACK_DISPOSITION_ACKED,
        )
    ]
    assert agent_cli.render_post_tool_hook_response(tmp_path) == ""
    command_stderr = io.StringIO()
    wrap.AgentInboxInjector(tmp_path, stderr=command_stderr).inject(force=True)
    assert command_stderr.getvalue() == ""


def test_working_state_snapshot_is_empty_when_no_live_state(tmp_path):
    snapshot = wrap.collect_working_state_snapshot(tmp_path)

    assert snapshot == wrap.WorkingStateSnapshot()
    assert not snapshot.has_fields()


def test_working_state_snapshot_collects_live_fields(tmp_path, monkeypatch):
    actor = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    claim_at = datetime(2026, 1, 1, 0, 0, tzinfo=UTC)
    now = (
        datetime(2026, 1, 1, 0, 0, tzinfo=UTC).timestamp()
        + WORKING_STATE_ELAPSED_SECONDS
    )
    monkeypatch.setenv(DRIVER.thread_id_env, actor)

    def fake_export(args=None):
        assert args == ["status:pending", "+ACTIVE"]
        return [
            {
                "claim_at": claim_at.isoformat().replace("+00:00", "Z"),
                "claim_by": actor,
                "claim_worktree": str(tmp_path),
                "description": "Collect working-state snapshot",
                "incepted": "00000001",
                "phase": "todo",
                "project": "session.meter",
                "status": "pending",
            }
        ]

    monkeypatch.setattr("spice.tasks.tw.export", fake_export)
    write_inbox_item(
        tmp_path,
        "20260101T000000000009Z.txt",
        compose_inbox_text(body="pending work", priority=None, stop=False),
    )
    (tmp_path / "dirty.txt").write_text("dirty\n", encoding="utf-8")
    record_maxim_metric_events(
        tmp_path,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="fallbacks",
                driver_name="codex",
                thread_id=actor,
                trigger_family="fallbacks",
                statement="fallback triggered",
            )
        ],
        now=now,
    )

    snapshot = wrap.collect_working_state_snapshot(tmp_path, now=now)

    assert snapshot.pending_inbox_count == 1
    assert snapshot.claim_handle == "METER-00000001"
    assert snapshot.claim_phase == "todo"
    assert snapshot.claim_elapsed_seconds == WORKING_STATE_ELAPSED_SECONDS
    assert snapshot.dirty_file_count >= 1
    assert snapshot.last_maxim_bag == "fallbacks"
    assert snapshot.has_fields()


def test_working_state_injector_omits_empty_snapshot(tmp_path):
    stderr = io.StringIO()
    injector = wrap.AgentWorkingStateInjector(
        tmp_path,
        stderr=stderr,
        snapshot_factory=lambda _repo: wrap.WorkingStateSnapshot(),
    )

    injector.inject(force=True)

    assert stderr.getvalue() == ""


def test_working_state_injector_renders_and_suppresses_one_line_sentence(tmp_path):
    now = [0.0]
    snapshot = [
        wrap.WorkingStateSnapshot(
            pending_inbox_count=1,
            claim_handle="METER-00000001",
            claim_phase="todo",
            claim_elapsed_seconds=90,
            dirty_file_count=2,
            last_maxim_bag="fallbacks",
        )
    ]
    stderr = io.StringIO()

    first = wrap.AgentWorkingStateInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: now[0],
        snapshot_factory=lambda _repo: snapshot[0],
    )
    first.inject(force=True)
    now[0] = 5.0
    second = wrap.AgentWorkingStateInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: now[0],
        snapshot_factory=lambda _repo: snapshot[0],
    )
    second.inject(force=True)
    now[0] = 6.0
    snapshot[0] = wrap.WorkingStateSnapshot(
        pending_inbox_count=1,
        claim_handle="METER-00000001",
        claim_phase="todo",
        claim_elapsed_seconds=96,
        dirty_file_count=3,
        last_maxim_bag="fallbacks",
    )
    second.inject(force=True)

    lines = stderr.getvalue().splitlines()
    assert lines == [
        (
            "🌶️ Working state: 1 pending inbox; claim METER-00000001 todo "
            "for 90s; 2 dirty files; last maxim fallbacks."
        ),
        (
            "🌶️ Working state: 1 pending inbox; claim METER-00000001 todo "
            "for 96s; 3 dirty files; last maxim fallbacks."
        ),
    ]
    for line in lines:
        assert line.startswith("🌶️ ")
        assert "\n" not in line
        assert line.count(".") == 1


def test_ensure_agent_uses_shipped_codex_defaults_without_config(tmp_path, monkeypatch):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(),
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "gpt-5.5"
    configs = _config_values(result.command)
    assert 'model_reasoning_effort="xhigh"' in configs
    assert not any(config.startswith("service_tier=") for config in configs)


def test_playwright_mcp_args_write_light_scheme_config(tmp_path, monkeypatch):
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "light")

    config_path = write_playwright_mcp_config(tmp_path)

    assert config_path == tmp_path / ".spice" / "agent" / "playwright-mcp.json"
    assert json.loads(config_path.read_text(encoding="utf-8")) == {
        "browser": {"contextOptions": {"colorScheme": "light"}}
    }
    assert playwright_mcp_args(tmp_path) == [
        "--yes",
        "@playwright/mcp@latest",
        "--headless",
        "--config",
        str(config_path),
    ]


def test_operator_color_scheme_defaults_to_explicit_light_off_macos(monkeypatch):
    monkeypatch.setattr(agent_driver.sys, "platform", "linux")

    assert operator_color_scheme() == "light"


def test_operator_color_scheme_defaults_to_explicit_light_when_unreadable(monkeypatch):
    monkeypatch.setattr(agent_driver.sys, "platform", "darwin")

    def raise_os_error(*_args, **_kwargs):
        raise OSError("defaults unavailable")

    monkeypatch.setattr(agent_driver.subprocess, "run", raise_os_error)

    assert operator_color_scheme() == "light"


def test_ensure_agent_dry_run_covers_start_resume_and_renew(tmp_path, monkeypatch):
    status_thread = [""]
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(thread_id=status_thread[0]),
    )

    started = lifecycle.ensure_agent(
        tmp_path,
        dry_run=True,
        model="gpt-direct",
        reasoning_effort="high",
        personality="friendly",
        agent_bin="codex-test",
        fast_mode=True,
    )
    status_thread[0] = "resume-thread"
    resumed = lifecycle.ensure_agent(
        tmp_path,
        dry_run=True,
    )
    renewed = lifecycle.ensure_agent(
        tmp_path,
        dry_run=True,
        force_new=True,
    )

    assert started.action == "would-start"
    assert started.prompt == "[$spice](.agents/skills/spice/SKILL.md)"
    assert str(tmp_path) not in started.prompt
    assert started.command[0] == "codex-test"
    assert 'model_reasoning_effort="high"' in _config_values(started.command)
    assert 'personality="friendly"' in _config_values(started.command)
    assert not any(
        config.startswith("service_tier=") for config in _config_values(started.command)
    )
    assert resumed.action == "would-resume"
    assert resumed.command[-3:] == ["resume", "resume-thread", resumed.prompt]
    assert renewed.action == "would-renew"
    assert renewed.command[-1] == renewed.prompt


def test_ensure_agent_dry_run_uses_relative_skill_prompt_for_claude(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.prompt == "[$spice](.agents/skills/spice/SKILL.md)"
    assert str(tmp_path) not in result.prompt
    assert result.command[-1] == result.prompt


def test_ensure_agent_uses_configured_claude_sonnet_family(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    config.update_section(
        tmp_path, config.AGENT_KEY, {config.AGENT_MODEL_KEY: "sonnet"}
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "sonnet"


def test_ensure_agent_applies_phase_model_for_claimed_task(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(thread_id="claimed-thread"),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    monkeypatch.setattr(
        ops,
        "active_claim_phase",
        lambda actor: "plan" if actor == "claimed-thread" else "",
    )
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.tasks.phase_models.claude.plan]\n"
        'model = "claude-opus-4-8"\n'
        'effort = "high"\n',
        encoding="utf-8",
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "claude-opus-4-8"
    assert result.command[result.command.index("--effort") + 1] == "high"


def test_ensure_agent_falls_back_when_claimed_phase_is_unmapped(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(thread_id="claimed-thread"),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    monkeypatch.setattr(ops, "active_claim_phase", lambda actor: "todo")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.tasks.phase_models.claude.plan]\nmodel = "claude-opus-4-8"\n',
        encoding="utf-8",
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "claude-sonnet-5"


def test_ensure_agent_skips_phase_lookup_without_a_thread_id(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: _status(),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)

    def _unexpected_call(actor):
        raise AssertionError("active_claim_phase should not run without a thread id")

    monkeypatch.setattr(ops, "active_claim_phase", _unexpected_call)

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "claude-sonnet-5"


def test_agent_state_uses_gitdirs_and_actual_thread_ids_for_linked_worktrees(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    linked = tmp_path / "linked"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    subprocess.run(
        ["git", "config", "user.email", "spice@example.test"],
        cwd=repo,
        check=True,
    )
    subprocess.run(["git", "config", "user.name", "Spice Tests"], cwd=repo, check=True)
    (repo / "README.md").write_text("initial\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "initial"], cwd=repo, check=True)
    subprocess.run(
        ["git", "worktree", "add", "-q", "--detach", str(linked), "HEAD"],
        cwd=repo,
        check=True,
    )
    common_agent_root = (repo / ".git" / "spice" / "agents").resolve()
    primary_worktree_dir = (repo / ".git" / "spice" / "agents").resolve()
    linked_git_dir = repo / ".git" / "worktrees" / linked.name / "spice" / "agents"
    linked_worktree_dir = (linked_git_dir).resolve()
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    linked_thread_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    primary_thread_dir = common_agent_root / thread_id
    linked_thread_dir = linked_worktree_dir / linked_thread_id

    assert lifecycle.agent_state_path(repo).parent == primary_worktree_dir
    assert sidechannelnotify.side_channel_marker_path(repo) == (
        primary_worktree_dir / "stderr.sock"
    )
    assert sidechannelnotify.side_channel_marker_path(linked) == (
        linked_worktree_dir / "stderr.sock"
    )
    with lifecycle.agent_ensure_lock(repo):
        assert (primary_worktree_dir / "ensure.lock").exists()

    log_path = primary_worktree_dir / "startup.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("starting\n", encoding="utf-8")
    final_log = lifecycle.settle_agent_log_path(repo, log_path, thread_id)
    lifecycle.write_agent_state(
        repo,
        {
            "mode": "start",
            "started_at": "2026-01-02T03:04:05Z",
            "prompt_skill_path": str(repo / lifecycle.WORKTREE_SKILL_RELATIVE_PATH),
            "thread_id": thread_id,
            "log_path": str(final_log),
        },
    )

    assert not log_path.exists()
    assert final_log == primary_thread_dir / "logs" / "startup.log"
    assert final_log.read_text(encoding="utf-8") == "starting\n"
    assert lifecycle.agent_state_path(repo) == primary_thread_dir / "state.json"
    assert (primary_worktree_dir / "thread-id").read_text(encoding="utf-8") == (
        f"{thread_id}\n"
    )
    assert wrap.context_meter_cache_path(repo) == (
        primary_thread_dir / "context-meter.json"
    )
    assert wrap.context_warning_state_path(repo) == (
        primary_thread_dir / "context-warning.json"
    )
    assert renewal.renewal_request_path(repo) == primary_thread_dir / "renew.json"
    assert repo / ".spice" not in primary_thread_dir.parents
    assert linked / ".spice" not in linked_worktree_dir.parents

    monkeypatch.setattr(lifecycle, "utc_now", lambda: "2026-01-02T03:04:05Z")
    linked_log = lifecycle.next_agent_log_path(linked)
    assert linked_log == linked_worktree_dir / "20260102T030405Z.log"
    linked_log.parent.mkdir(parents=True, exist_ok=True)
    linked_log.write_text("linked\n", encoding="utf-8")
    final_linked_log = lifecycle.settle_agent_log_path(
        linked, linked_log, linked_thread_id
    )
    lifecycle.write_agent_state(
        linked,
        {
            "mode": "start",
            "started_at": "2026-01-02T03:04:05Z",
            "prompt_skill_path": str(linked / lifecycle.WORKTREE_SKILL_RELATIVE_PATH),
            "thread_id": linked_thread_id,
            "log_path": str(final_linked_log),
        },
    )
    assert final_linked_log == linked_thread_dir / "logs" / "20260102T030405Z.log"
    assert lifecycle.agent_state_path(linked) == linked_thread_dir / "state.json"
    assert (linked_worktree_dir / "thread-id").read_text(encoding="utf-8") == (
        f"{linked_thread_id}\n"
    )
    assert not (common_agent_root / linked_thread_id).exists()


def test_start_agent_direct_path_writes_started_state_under_fakes(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "agent.log"
    process = _FakeProcess(pid=DIRECT_AGENT_PID, returncode=None)
    spawned: list[tuple[list[str], object, object]] = []
    reaped: list[int] = []
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    monkeypatch.setattr(lifecycle, "next_agent_log_path", lambda _repo: log_path)
    monkeypatch.setattr(
        lifecycle,
        "spawn_agent",
        lambda command, *, cwd, log_path: (
            spawned.append((command, cwd, log_path)) or process
        ),
    )
    monkeypatch.setattr(
        lifecycle,
        "require_started_process",
        lambda _process, _log_path, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log_path, *, repo_root, fallback_thread_id: thread_id,
    )
    monkeypatch.setattr(
        lifecycle, "reap_process_when_done", lambda proc: reaped.append(proc.pid)
    )

    returned = lifecycle.start_agent(
        tmp_path,
        action="start",
        command=["codex", "exec", "prompt"],
        model="gpt-test",
        reasoning_effort="medium",
        service_tier="",
        resume_thread_id="",
        prompt_skill_path=tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH,
        fast_mode=False,
        supervise_stdout=False,
    )
    state = lifecycle.read_agent_state(tmp_path)
    final_log_path = (
        tmp_path / ".git" / "spice" / "agents" / thread_id / "logs" / log_path.name
    ).resolve()

    assert returned == final_log_path
    assert spawned == [(["codex", "exec", "prompt"], tmp_path, log_path)]
    assert state["pid"] == DIRECT_AGENT_PID
    assert state["mode"] == "start"
    assert state["model"] == "gpt-test"
    assert state["reasoning_effort"] == "medium"
    assert state["thread_id"] == thread_id
    assert state["log_path"] == str(final_log_path)
    assert reaped == [DIRECT_AGENT_PID]


def test_start_agent_supervised_path_uses_supervisor_and_reaper(tmp_path, monkeypatch):
    log_path = tmp_path / "supervised.log"
    process = _FakeProcess(pid=SUPERVISOR_PID, returncode=None)
    spawned: list[dict[str, object]] = []
    required: list[tuple[int, object, object]] = []
    reaped: list[int] = []
    monkeypatch.setattr(lifecycle, "next_agent_log_path", lambda _repo: log_path)

    def spawn_supervisor(repo_root, **kwargs):
        spawned.append({"repo_root": repo_root, **kwargs})
        return process

    monkeypatch.setattr(lifecycle, "spawn_agent_supervisor", spawn_supervisor)
    monkeypatch.setattr(
        lifecycle,
        "require_supervisor_started",
        lambda proc, *, repo_root, log_path: required.append(
            (proc.pid, repo_root, log_path)
        ),
    )
    monkeypatch.setattr(
        lifecycle, "reap_process_when_done", lambda proc: reaped.append(proc.pid)
    )

    returned = lifecycle.start_agent(
        tmp_path,
        action="resume",
        command=["codex", "exec", "resume", "thread", "prompt"],
        model="gpt-test",
        reasoning_effort="high",
        service_tier="fast",
        resume_thread_id="thread",
        prompt_skill_path=tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH,
        fast_mode=True,
        supervise_stdout=True,
    )

    assert returned == log_path
    assert spawned[0]["repo_root"] == tmp_path
    assert spawned[0]["action"] == "resume"
    assert spawned[0]["service_tier"] == "fast"
    assert spawned[0]["fast_mode"] is True
    assert "prompt_skill_path" not in spawned[0]
    assert required == [(SUPERVISOR_PID, tmp_path, log_path)]
    assert reaped == [SUPERVISOR_PID]


def test_spawn_agent_supervisor_omits_prompt_skill_path_arg(tmp_path, monkeypatch):
    log_path = tmp_path / "supervised.log"
    spawned: list[dict[str, object]] = []

    class FakePopen(_FakeProcess):
        def __init__(self, command, **kwargs) -> None:
            super().__init__(pid=SUPERVISOR_PID, returncode=None)
            spawned.append({"command": command, **kwargs})

        @classmethod
        def __class_getitem__(cls, _item):
            return cls

    monkeypatch.setattr(lifecycle.subprocess, "Popen", FakePopen)
    monkeypatch.setattr(
        lifecycle, "agent_supervisor_environment", lambda repo_root: {"ENV": "1"}
    )

    returned = lifecycle.spawn_agent_supervisor(
        tmp_path,
        action="start",
        command=["codex", "exec", "prompt"],
        model="gpt-test",
        reasoning_effort="medium",
        service_tier="",
        resume_thread_id="",
        log_path=log_path,
        fast_mode=False,
    )

    command = spawned[0]["command"]
    assert isinstance(command, list)
    assert returned.pid == SUPERVISOR_PID
    assert command[command.index("--repo-root") + 1] == str(tmp_path)
    assert "--prompt-skill-path" not in command
    assert command[command.index("--command-json") + 1] == '["codex","exec","prompt"]'


def test_run_agent_supervisor_writes_state_under_fakes(tmp_path, monkeypatch):
    log_path = tmp_path / "supervisor.log"
    skill_path = (tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH).resolve()
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=5)
    thread = _FakeThread()
    side_events: list[tuple[str, object]] = []
    spawned: list[dict[str, object]] = []
    thread_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    monkeypatch.setattr(lifecycle, "agent_environment", lambda repo_root: {"ENV": "1"})
    monkeypatch.setattr(
        lifecycle,
        "spawn_supervised_agent",
        lambda command, *, cwd, log_path, env: (
            spawned.append(
                {"command": command, "cwd": cwd, "log_path": log_path, "env": env}
            )
            or (process, thread)
        ),
    )
    monkeypatch.setattr(
        lifecycle,
        "require_started_process",
        lambda _process, _log_path, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda _log_path, *, repo_root, fallback_thread_id: thread_id,
    )
    monkeypatch.setattr(
        sidechannel,
        "AgentSideChannelServer",
        lambda repo_root: _FakeSideChannel(repo_root, side_events),
    )
    args = argparse.Namespace(
        repo_root=str(tmp_path),
        action="resume",
        model="gpt-test",
        reasoning_effort="high",
        service_tier="fast",
        resume_thread_id="resume-thread",
        log_path=str(log_path),
        fast_mode=True,
        command_json='["codex","exec","prompt"]',
    )

    exit_code = lifecycle.run_agent_supervisor(args)
    state = lifecycle.read_agent_state(tmp_path)
    final_log_path = (
        tmp_path / ".git" / "spice" / "agents" / thread_id / "logs" / log_path.name
    ).resolve()

    assert exit_code == 5
    assert side_events == [("enter", tmp_path), ("exit", tmp_path)]
    assert spawned == [
        {
            "command": ["codex", "exec", "prompt"],
            "cwd": tmp_path,
            "log_path": log_path,
            "env": {"ENV": "1"},
        }
    ]
    assert state["pid"] == SUPERVISED_AGENT_PID
    assert state["supervisor_pid"] == os.getpid()
    assert state["thread_id"] == thread_id
    assert state["log_path"] == str(final_log_path)
    assert state["prompt_skill_path"] == str(skill_path)
    assert state["fast_mode"] is True
    assert thread.joined_timeouts == [1.0]


def test_require_supervisor_started_accepts_thread_settled_log_path(
    tmp_path, monkeypatch
):
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    log_path = lifecycle.next_agent_log_path(tmp_path)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("starting\n", encoding="utf-8")
    final_log_path = lifecycle.settle_agent_log_path(tmp_path, log_path, thread_id)
    lifecycle.write_agent_state(
        tmp_path,
        {
            "pid": SUPERVISED_AGENT_PID,
            "thread_id": thread_id,
            "log_path": str(final_log_path),
            "mode": "start",
            "started_at": "2026-01-02T03:04:05Z",
            "prompt_skill_path": str(tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH),
        },
    )
    monkeypatch.setattr(lifecycle, "SUPERVISOR_STARTUP_TIMEOUT_SECONDS", 0)
    monkeypatch.setattr(
        lifecycle,
        "process_id_is_running",
        lambda pid: pid == SUPERVISED_AGENT_PID,
    )
    process = _FakeProcess(pid=SUPERVISOR_PID, returncode=None)

    lifecycle.require_supervisor_started(process, repo_root=tmp_path, log_path=log_path)
    assert process.wait_calls == 0


def test_require_started_process_distinguishes_codex_credit_failure(
    tmp_path, monkeypatch
):
    log_path = tmp_path / "agent.log"
    log_path.write_text(
        "ERROR: You've hit your usage limit. Visit "
        "https://chatgpt.com/codex/settings/usage to purchase more credits "
        "or try again at 4:36 PM.\n",
        encoding="utf-8",
    )
    process = _FakeProcess(pid=SUPERVISED_AGENT_PID, returncode=1)

    monkeypatch.setattr(lifecycle, "STARTUP_GRACE_SECONDS", 0)
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CODEX_DRIVER)

    with pytest.raises(lifecycle.AgentOutOfCreditsError, match="hit your usage limit"):
        lifecycle.require_started_process(process, log_path, repo_root=tmp_path)


def test_agent_environment_refuses_ambient_thread(tmp_path, monkeypatch):
    monkeypatch.setenv(DRIVER.thread_id_env, "ambient-thread")

    with pytest.raises(SpiceError, match="refusing to spawn an agent"):
        lifecycle.agent_environment(tmp_path)


def test_agent_binding_error_reports_stale_launch_cwd_and_ignores_prompt_skill(
    tmp_path,
):
    lane = tmp_path / "lane"
    other = tmp_path / "other"
    lane.mkdir()
    other.mkdir()
    lane_skill = lane / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    other_skill = other / lifecycle.WORKTREE_SKILL_RELATIVE_PATH

    cwd_error = lifecycle.agent_binding_error(
        lane,
        SimpleNamespace(
            repo_root=lane,
            command=["codex", "exec", "--cd", str(other)],
            prompt_skill_path=lane_skill,
        ),
    )
    skill_error = lifecycle.agent_binding_error(
        lane,
        SimpleNamespace(
            repo_root=lane,
            command=["codex", "exec", "--cd", str(lane)],
            prompt_skill_path=other_skill,
        ),
    )

    assert f"launch cwd {other.resolve()} != lane root {lane.resolve()}" in cwd_error
    assert skill_error == ""


def test_agent_binding_allows_launch_cwd_inside_worktree_for_side_channel(tmp_path):
    child = tmp_path / "subdir"
    child.mkdir()

    diagnostic = sidechannel.side_channel_binding_diagnostic(
        tmp_path,
        {
            "type": "hello",
            "repoRoot": str(tmp_path),
            "cwd": str(child),
        },
    )

    assert diagnostic == ""


def test_side_channel_binding_diagnostic_refuses_wrong_repo_root(tmp_path):
    other = tmp_path / "other"
    other.mkdir()

    diagnostic = sidechannel.side_channel_binding_diagnostic(
        tmp_path,
        {
            "type": "hello",
            "repoRoot": str(other),
            "cwd": str(other),
        },
    )

    assert "Agent Binding Mismatch" in diagnostic
    assert f"lane_repo_root={tmp_path.resolve()}" in diagnostic
    assert f"wrapper_repo_root={other.resolve()}" in diagnostic
    assert "steering_delivery=refused" in diagnostic


def _config_values(command: list[str]) -> list[str]:
    return [
        command[index + 1] for index, part in enumerate(command) if part == "--config"
    ]


def _status(*, thread_id: str = "", running: bool = False):
    return SimpleNamespace(
        running=running,
        thread_id=thread_id,
        log_path=None,
        process_status="running" if running else "idle",
    )


class _FakeProcess:
    def __init__(self, *, pid: int, returncode: int | None) -> None:
        self.pid = pid
        self.returncode = returncode
        self.wait_calls = 0

    def poll(self):
        return self.returncode

    def wait(self):
        self.wait_calls += 1
        return self.returncode


class _FakeThread:
    def __init__(self) -> None:
        self.joined_timeouts: list[float | None] = []

    def join(self, timeout: float | None = None) -> None:
        self.joined_timeouts.append(timeout)


class _FakeSideChannel:
    def __init__(self, repo_root, events) -> None:
        self.repo_root = repo_root
        self.events = events

    def __enter__(self):
        self.events.append(("enter", self.repo_root))
        return self

    def __exit__(self, *_exc):
        self.events.append(("exit", self.repo_root))
