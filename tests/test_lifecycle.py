"""Agent lifecycle, wrapper routing, and supervisor contracts."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
from types import SimpleNamespace

import pytest

from spice.config import edit, layers, values
from spice.agent import driver as agent_driver
from spice.agent import (
    lifecycle,
    renewal,
    sidechannel,
    sidechannelnotify,
    wrap,
)
from spice.agent.driver import (
    CLAUDE_DRIVER,
    CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE,
    CODEX_DRIVER,
    DRIVER,
    FAST_MODE_LAUNCH_KNOB,
    PERSONALITY_LAUNCH_KNOB,
    POST_TOOL_HOOK_EVENT,
    PLAYWRIGHT_MCP_ARGS,
    PLAYWRIGHT_MCP_COMMAND,
    PLAYWRIGHT_MCP_SERVER_NAME,
    operator_color_scheme,
    playwright_mcp_args,
    post_tool_hook_config_path,
    write_playwright_mcp_config,
)
from spice.agent.cli import render_ensure_result
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.paths import git_dir
from spice.process import tool as processtool
from spice.tasks import claimstate
from tests.test_lifecyclehelpers import (
    FakeProcess,
    status,
)

DIRECT_AGENT_PID = 2222
SUPERVISOR_PID = 3333
SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
SHELL_HOOK_FAILURE_EXIT_CODE = 127
WORKING_STATE_ELAPSED_SECONDS = 90
LAUNCH_CLAIM_UUID = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"
LAUNCH_CLAIM_ACTOR = "dddddddddddddddddddddddddddddddd"
# A real effort both drivers accept, differing from the shipped default so
# asking for it is visible in the command either one builds.
PROBE_REASONING_EFFORT = "low"
# What `spice agent ensure` reports about every launch, before the lines it
# adds only when there is something to add.
LAUNCH_REPORT_FIELDS = (
    "action",
    "status",
    "pid",
    "pgid",
    "thread",
    "service_tier",
    "prompt",
)


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def test_shipped_agent_defaults_are_current_high_effort():
    assert CODEX_DRIVER.default_model == "gpt-5.5"
    assert CODEX_DRIVER.default_reasoning_effort == "xhigh"
    assert CODEX_DRIVER.default_service_tier == ""
    assert CLAUDE_DRIVER.default_model == "claude-opus-4-8"
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
    monkeypatch.setattr(
        claimstate, "ambient_thread", lambda: ("thread-third", third_driver)
    )
    site = claimstate.ClaimSite(tmp_path, "main", "head-third")

    claim = claimstate.claim_meta(
        "actor-third", site=site, context_thread=None, lease_seconds=None
    )

    assert "claim_thread:thread-third" in claim
    assert "claim_context_turn:turn-third" in claim
    assert f"claim_worktree:{tmp_path}" in claim
    assert "claim_branch:main" in claim
    assert "claim_head:head-third" in claim

    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return "rtk third inner" if args == ("third inner",) else None

    monkeypatch.setattr(wrap, "driver_for", lambda _repo_root: third_driver)
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(
        ["zsh", "-c", "third:third inner"], repo_root=tmp_path, rewrite_rtk=True
    )

    assert command == ["zsh", "-c", "third:rtk third inner"]
    assert calls == [("third:third inner",), ("third inner",)]


def test_claim_meta_uses_actor_as_thread_without_ambient(tmp_path, monkeypatch):
    monkeypatch.setattr(claimstate, "ambient_thread", lambda: None)
    site = claimstate.ClaimSite(tmp_path, "main", "head-third")

    claim = claimstate.claim_meta(
        "actor-third", site=site, context_thread=None, lease_seconds=None
    )

    assert "claim_by:actorthird" in claim
    assert "claim_thread:actorthird" in claim
    assert "claim_context_turn:actorthird" in claim


def test_codex_driver_command_honors_explicit_fast_service_tier_and_playwright_mcp(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "dark")
    agent_root = git_dir(tmp_path) / ".spice" / "agents"
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
        f'"{agent_root / "playwright-mcp.json"}"]'
    ) in configs
    hook_config_path = post_tool_hook_config_path(tmp_path, DRIVER)
    hook_config = json.loads(hook_config_path.read_text(encoding="utf-8"))
    hook_overrides = [
        config for config in configs if config.startswith("hooks.PostToolUse=")
    ]
    assert len(hook_overrides) == 1
    assert hook_config_path == agent_root / "codex-post-tool-hook.json"
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
        (agent_root / "playwright-mcp.json").read_text(encoding="utf-8")
    ) == {"browser": {"contextOptions": {"colorScheme": "dark"}}}
    assert 'personality="pragmatic"' in configs
    assert 'service_tier="fast"' in configs
    assert command[command.index("--enable") + 1] == "fast_mode"
    assert command[-3:] == ["resume", "thread-1", prompt]


def test_every_driver_honors_exactly_the_launch_knobs_it_declares(
    tmp_path, monkeypatch
):
    # The declaration is checked against the command each driver actually
    # builds, so a driver cannot claim a knob it drops or drop one it claims.
    # A driver growing a launch seam without declaring it fails here too.
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "dark")
    probes: dict[str, object] = {
        "model": "probe-model",
        "reasoning_effort": PROBE_REASONING_EFFORT,
        "personality": "friendly",
        "fast_mode": True,
    }
    assert set(probes) == agent_driver.LAUNCH_KNOBS

    for driver in agent_driver.all_drivers():
        baseline = driver.build_exec_command(
            repo_root=tmp_path, prompt="P", binary="probe-bin"
        )
        for knob, probe in probes.items():
            asked = driver.build_exec_command(
                repo_root=tmp_path, prompt="P", binary="probe-bin", **{knob: probe}
            )
            reaches_command = asked != baseline
            assert reaches_command == (knob in driver.honored_launch_knobs), (
                f"{driver.name} declares honored={knob in driver.honored_launch_knobs} "
                f"for {knob} but reaches_command={reaches_command}"
            )


def test_launch_sends_no_knob_its_driver_cannot_carry_and_says_which(
    tmp_path, monkeypatch
):
    # Claude has no launch-time seam for either knob, so the launch withholds
    # both in the open and names them instead of handing them to a driver body
    # that ignores them. Assert on what the driver is handed: a driver that
    # ignores an argument builds the same command either way, so the command
    # alone cannot tell a withheld knob from a dropped one.
    monkeypatch.setattr(lifecycle, "agent_status", lambda *_args, **_kwargs: status())
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    handed = _record_launch_arguments(monkeypatch, CLAUDE_DRIVER)
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {values.AGENT_PERSONALITY_KEY: "friendly"},
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True, fast_mode=True)

    assert result.unhonored_launch_knobs == ("fast_mode", "personality")
    assert handed == [{"personality": "", "fast_mode": False}]
    assert LAUNCH_REPORT_FIELDS + ("unhonored", "command") == _report_fields(result)
    assert (
        "unhonored=fast_mode,personality (no launch-time seam on this driver; not sent)"
    ) in render_ensure_result(result).splitlines()


def test_launch_stays_quiet_about_knobs_nobody_asked_for(tmp_path, monkeypatch):
    # The shipped personality default is the contract's own answer, not a
    # request, so an ordinary Claude launch reports nothing and prints nothing.
    monkeypatch.setattr(lifecycle, "agent_status", lambda *_args, **_kwargs: status())
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.unhonored_launch_knobs == ()
    assert LAUNCH_REPORT_FIELDS + ("command",) == _report_fields(result)


def test_codex_carries_both_knobs_the_launch_asks_it_for(tmp_path, monkeypatch):
    # The same request on the driver that does have the seams: nothing is
    # dropped, nothing is reported, and both land in the built command.
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "dark")
    monkeypatch.setattr(lifecycle, "agent_status", lambda *_args, **_kwargs: status())
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CODEX_DRIVER)
    handed = _record_launch_arguments(monkeypatch, CODEX_DRIVER)
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {values.AGENT_PERSONALITY_KEY: "friendly"},
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True, fast_mode=True)

    assert result.unhonored_launch_knobs == ()
    assert handed == [{"personality": "friendly", "fast_mode": True}]
    assert 'personality="friendly"' in _config_values(result.command)
    assert result.command[result.command.index("--enable") + 1] == "fast_mode"


def test_ensure_agent_uses_shipped_codex_defaults_without_config(tmp_path, monkeypatch):
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: status(),
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "gpt-5.5"
    configs = _config_values(result.command)
    assert 'model_reasoning_effort="xhigh"' in configs
    assert not any(config.startswith("service_tier=") for config in configs)


def test_playwright_mcp_args_write_light_scheme_config(tmp_path, monkeypatch):
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "light")

    config_path = write_playwright_mcp_config(tmp_path)

    assert config_path == (
        git_dir(tmp_path) / ".spice" / "agents" / "playwright-mcp.json"
    )
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

    monkeypatch.setattr(processtool, "run_bounded_process_group", raise_os_error)

    assert operator_color_scheme() == "light"


def test_ensure_agent_dry_run_covers_start_resume_and_renew(tmp_path, monkeypatch):
    status_thread = [""]
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: status(thread_id=status_thread[0]),
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
        lambda *_args, **_kwargs: status(),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.prompt == "[$spice](.agents/skills/spice/SKILL.md)"
    assert str(tmp_path) not in result.prompt
    # Claude's own command construction prefaces the trailing prompt so the
    # skill reads as binding rather than optional (still generic, not
    # operator-specific -- the neutral result.prompt above is what the
    # prompt boundary actually locks), then appends the worktree steering token
    # (exact line asserted in test_claudedriver).
    from spice.mail.steeringkey import steering_token

    assert result.command[-1].startswith(
        f"{CLAUDE_SKILL_SYSTEM_PROMPT_PREAMBLE}\n\n{result.prompt}"
    )
    assert f"<{steering_token(tmp_path)}>" in result.command[-1]


def test_ensure_agent_uses_configured_claude_sonnet_family(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: status(),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {values.AGENT_MODEL_KEY: "sonnet"},
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "sonnet"


def test_ensure_agent_applies_phase_model_for_claimed_task(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: status(thread_id="claimed-thread"),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    monkeypatch.setattr(
        claimstate,
        "active_claim_phase",
        lambda actor: "plan" if actor == "claimed-thread" else "",
    )
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.tasks.phase_models.claude.plan]\n"
        'model = "claude-sonnet-5"\n'
        'effort = "high"\n',
        encoding="utf-8",
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    # Mapped model is intentionally NOT the opus default, so this asserts the
    # phase mapping is honored rather than coinciding with the fallback.
    assert result.command[result.command.index("--model") + 1] == "claude-sonnet-5"
    assert result.command[result.command.index("--effort") + 1] == "high"


def test_ensure_agent_falls_back_when_claimed_phase_is_unmapped(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: status(thread_id="claimed-thread"),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)
    monkeypatch.setattr(claimstate, "active_claim_phase", lambda actor: "todo")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.tasks.phase_models.claude.plan]\nmodel = "claude-sonnet-5"\n',
        encoding="utf-8",
    )

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    # The claimed phase is unmapped, so it must fall back to the opus default --
    # not silently pick up the (distinct) plan-phase mapping.
    assert result.command[result.command.index("--model") + 1] == "claude-opus-4-8"


def test_ensure_agent_skips_phase_lookup_without_a_thread_id(tmp_path, monkeypatch):
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda *_args, **_kwargs: status(),
    )
    monkeypatch.setattr(lifecycle, "driver_for", lambda _repo_root: CLAUDE_DRIVER)

    def _unexpected_call(actor):
        raise AssertionError("active_claim_phase should not run without a thread id")

    monkeypatch.setattr(claimstate, "active_claim_phase", _unexpected_call)

    result = lifecycle.ensure_agent(tmp_path, dry_run=True)

    assert result.command[result.command.index("--model") + 1] == "claude-opus-4-8"


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
    common_agent_root = (repo / ".git" / ".spice" / "agents").resolve()
    primary_worktree_dir = (repo / ".git" / ".spice" / "agents").resolve()
    linked_git_dir = repo / ".git" / "worktrees" / linked.name / ".spice" / "agents"
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
    assert post_tool_hook_config_path(repo, DRIVER) == (
        primary_worktree_dir / "codex-post-tool-hook.json"
    )
    assert post_tool_hook_config_path(linked, DRIVER) == (
        linked_worktree_dir / "codex-post-tool-hook.json"
    )
    monkeypatch.setattr(agent_driver, "operator_color_scheme", lambda: "light")
    assert write_playwright_mcp_config(repo) == (
        primary_worktree_dir / "playwright-mcp.json"
    )
    assert write_playwright_mcp_config(linked) == (
        linked_worktree_dir / "playwright-mcp.json"
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
    process = FakeProcess(pid=DIRECT_AGENT_PID, returncode=None)
    spawned: list[tuple[list[str], object, object]] = []
    reaped: list[int] = []
    events: list[str] = []
    thread_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    monkeypatch.setattr(lifecycle, "next_agent_log_path", lambda _repo: log_path)
    monkeypatch.setattr(
        lifecycle.boundaries,
        "fast_forward_if_safe",
        lambda repo_root: events.append(f"sync:{repo_root}"),
    )

    def spawn_direct(command, *, cwd, log_path):
        events.append("spawn-direct")
        spawned.append((command, cwd, log_path))
        return process

    monkeypatch.setattr(
        lifecycle,
        "spawn_agent",
        spawn_direct,
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
        lifecycle,
        "reap_process_when_done",
        lambda proc, **_kwargs: reaped.append(proc.pid),
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
        launch_claim=None,
    )
    state = lifecycle.read_agent_state(tmp_path)
    final_log_path = (
        tmp_path / ".git" / ".spice" / "agents" / thread_id / "logs" / log_path.name
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
    assert events == [f"sync:{tmp_path}", "spawn-direct"]


@pytest.mark.parametrize("action", ["start", "resume", "renew"])
def test_start_agent_supervised_path_uses_supervisor_and_reaper(
    tmp_path, monkeypatch, action
):
    log_path = tmp_path / "supervised.log"
    process = FakeProcess(pid=SUPERVISOR_PID, returncode=None)
    spawned: list[dict[str, object]] = []
    required: list[tuple[int, object, object]] = []
    reaped: list[int] = []
    events: list[str] = []
    monkeypatch.setattr(lifecycle, "next_agent_log_path", lambda _repo: log_path)
    monkeypatch.setattr(
        lifecycle.boundaries,
        "fast_forward_if_safe",
        lambda repo_root: events.append(f"sync:{repo_root}"),
    )

    def spawn_supervisor(repo_root, **kwargs):
        events.append("spawn-supervisor")
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
        lifecycle,
        "reap_process_when_done",
        lambda proc, **_kwargs: reaped.append(proc.pid),
    )

    returned = lifecycle.start_agent(
        tmp_path,
        action=action,
        command=["codex", "exec", "resume", "thread", "prompt"],
        model="gpt-test",
        reasoning_effort="high",
        service_tier="fast",
        resume_thread_id="thread",
        prompt_skill_path=tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH,
        fast_mode=True,
        supervise_stdout=True,
        launch_claim=None,
    )

    assert returned == log_path
    assert spawned[0]["repo_root"] == tmp_path
    assert spawned[0]["action"] == action
    assert spawned[0]["service_tier"] == "fast"
    assert spawned[0]["fast_mode"] is True
    assert "prompt_skill_path" not in spawned[0]
    assert required == [(SUPERVISOR_PID, tmp_path, log_path)]
    assert reaped == [SUPERVISOR_PID]
    assert events == [f"sync:{tmp_path}", "spawn-supervisor"]


def test_spawn_agent_supervisor_omits_prompt_skill_path_arg(tmp_path, monkeypatch):
    log_path = tmp_path / "supervised.log"
    spawned: list[dict[str, object]] = []

    class FakePopen(FakeProcess):
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
        launch_claim=None,
    )

    command = spawned[0]["command"]
    assert isinstance(command, list)
    assert returned.pid == SUPERVISOR_PID
    assert command[command.index("--repo-root") + 1] == str(tmp_path)
    assert "--prompt-skill-path" not in command
    assert command[command.index("--command-json") + 1] == '["codex","exec","prompt"]'


def test_supervisor_command_carries_the_launch_claim_to_the_supervisor(
    tmp_path, monkeypatch
):
    """The reservation survives the only hop that leaves this process."""
    log_path = tmp_path / "supervised.log"
    spawned: list[list[str]] = []
    claim = lifecycle.LaunchClaim(uuid=LAUNCH_CLAIM_UUID, actor=LAUNCH_CLAIM_ACTOR)
    # Built before Popen is faked: assembling the parser shells out to git.
    parser = build_parser()

    class FakePopen(FakeProcess):
        def __init__(self, command, **_kwargs) -> None:
            super().__init__(pid=SUPERVISOR_PID, returncode=None)
            spawned.append(command)

        @classmethod
        def __class_getitem__(cls, _item):
            return cls

    monkeypatch.setattr(lifecycle.subprocess, "Popen", FakePopen)
    monkeypatch.setattr(
        lifecycle, "agent_supervisor_environment", lambda repo_root: {"ENV": "1"}
    )

    for launch_claim in (claim, None):
        lifecycle.spawn_agent_supervisor(
            tmp_path,
            action="start",
            command=["codex", "exec", "prompt"],
            model="gpt-test",
            reasoning_effort="medium",
            service_tier="",
            resume_thread_id="",
            log_path=log_path,
            fast_mode=False,
            launch_claim=launch_claim,
        )

    # `spice agent supervise ...` is the whole hop: parse the spawned argv back
    # with the real CLI parser rather than reading the flags off the list.
    parsed = [parser.parse_args(command[3:]) for command in spawned]
    assert [lifecycle.launch_claim_from_args(args) for args in parsed] == [claim, None]
    assert [command[:5] for command in spawned] == [
        [sys.executable, "-m", "spice", "agent", "supervise"]
    ] * 2


@pytest.mark.parametrize(
    "uuid,actor",
    [(LAUNCH_CLAIM_UUID, ""), ("", LAUNCH_CLAIM_ACTOR)],
)
def test_launch_claim_refuses_to_name_half_a_reservation(uuid, actor):
    args = argparse.Namespace(launch_claim_uuid=uuid, launch_claim_actor=actor)

    with pytest.raises(SpiceError, match="names both the task and its owner"):
        lifecycle.launch_claim_from_args(args)


def test_ensure_agent_refuses_a_launch_claim_no_supervisor_can_release(tmp_path):
    with pytest.raises(SpiceError, match="a launch claim rides the supervisor"):
        lifecycle.ensure_agent(
            tmp_path,
            supervise_stdout=False,
            launch_claim=lifecycle.LaunchClaim(
                uuid=LAUNCH_CLAIM_UUID, actor=LAUNCH_CLAIM_ACTOR
            ),
        )


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


def _record_launch_arguments(
    monkeypatch, driver: agent_driver.AgentDriver
) -> list[dict[str, object]]:
    """The knobs each launch hands this driver, recorded as the launch sends them.

    Reads the seam between the launch path and the driver, which is where a
    withheld knob and a knob the driver body silently ignores stop looking the
    same. Delegates to the real builder so the command is still the shipped one.
    """
    handed: list[dict[str, object]] = []
    build_exec_command = type(driver).build_exec_command
    watched = {PERSONALITY_LAUNCH_KNOB, FAST_MODE_LAUNCH_KNOB}

    def record(self, **kwargs):
        handed.append({key: kwargs[key] for key in sorted(watched & set(kwargs))})
        return build_exec_command(self, **kwargs)

    monkeypatch.setattr(type(driver), "build_exec_command", record)
    return handed


def _report_fields(result: lifecycle.AgentEnsureResult) -> tuple[str, ...]:
    """The field names `spice agent ensure` prints for a result, in order."""
    return tuple(
        line.split("=", 1)[0] for line in render_ensure_result(result).splitlines()
    )
