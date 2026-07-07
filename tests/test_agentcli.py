"""Agent CLI surface, steering readout, import, and reply contracts."""

import argparse
from datetime import UTC, datetime
import io
import json
from pathlib import Path
import re
import subprocess
from types import SimpleNamespace

import pytest

from spice.agent import driver as agent_driver
from spice.agent import cli as agent_cli
from spice.agent import (
    lifecycle,
    watchdog,
    wrap,
)
from spice.agent.driver import (
    DRIVER,
    POST_TOOL_HOOK_EVENT,
)
from spice.agent.maximmetrics import (
    MAXIM_EVENT_FIRE,
    MaximMetricEventWrite,
    record_maxim_metric_events,
)
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    ack_state_records,
)
from spice.mail.inbox import collect_inbox_items, compose_inbox_text, write_inbox_item

from spice.mail.steeringkey import steering_token


WORKING_STATE_ELAPSED_SECONDS = 90


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def _status(*, thread_id: str = "", running: bool = False):
    return SimpleNamespace(
        running=running,
        thread_id=thread_id,
        log_path=None,
        process_status="running" if running else "idle",
    )


AGENT_COMMAND_MENTION_RE = re.compile(r"\bspice\s+agent\s+([a-z][a-z0-9-]*)\b")


AGENT_COMMAND_AUDIT_ROOTS = (
    "README.md",
    "docs",
    "spice",
    "tests",
)


AGENT_COMMAND_AUDIT_OPTIONAL_ROOTS = (".agents/skills/spice/SKILL.md",)


AGENT_COMMAND_AUDIT_TEXT_SUFFIXES = {
    "",
    ".css",
    ".js",
    ".md",
    ".py",
    ".sh",
    ".toml",
    ".txt",
}


AGENT_COMMAND_NON_COMMAND_WORDS = frozenset({"bootstrap", "from"})


def _agent_parser_verbs() -> set[str]:
    parser = build_parser()
    top_level = next(
        action
        for action in parser._actions
        if isinstance(action, argparse._SubParsersAction)
    )
    agent_parser = top_level.choices["agent"]
    agent_actions = next(
        action
        for action in agent_parser._actions
        if isinstance(action, argparse._SubParsersAction)
    )
    return set(agent_actions.choices)


def _agent_command_audit_paths(repo_root: Path) -> list[Path]:
    paths: list[Path] = []
    for relative in AGENT_COMMAND_AUDIT_ROOTS:
        root = repo_root / relative
        if root.is_file():
            if _agent_command_audit_text_path(root):
                paths.append(root)
        elif root.is_dir():
            paths.extend(
                path
                for path in root.rglob("*")
                if path.is_file() and _agent_command_audit_text_path(path)
            )
    for relative in AGENT_COMMAND_AUDIT_OPTIONAL_ROOTS:
        path = repo_root / relative
        if path.is_file():
            paths.append(path)
    return sorted(set(paths))


def _agent_command_audit_text_path(path: Path) -> bool:
    return path.suffix in AGENT_COMMAND_AUDIT_TEXT_SUFFIXES


def _agent_command_mentions(repo_root: Path):
    for path in _agent_command_audit_paths(repo_root):
        text = path.read_text(encoding="utf-8", errors="replace")
        relative = path.relative_to(repo_root).as_posix()
        for line_number, line in enumerate(text.splitlines(), start=1):
            for match in AGENT_COMMAND_MENTION_RE.finditer(line):
                verb = match.group(1)
                if verb not in AGENT_COMMAND_NON_COMMAND_WORDS:
                    yield relative, line_number, verb, line.strip()


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


def test_agent_command_mentions_match_parser_surface():
    repo_root = Path(__file__).resolve().parents[1]
    parser_verbs = _agent_parser_verbs()
    assert "show" in parser_verbs
    assert "run" in parser_verbs
    assert "post-tool-hook" in parser_verbs

    unsupported = [
        f"{path}:{line_number}: unsupported spice agent {verb!r}: {line}"
        for path, line_number, verb, line in _agent_command_mentions(repo_root)
        if verb not in parser_verbs
    ]

    assert unsupported == []


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

    token = steering_token(tmp_path)
    assert second_context.splitlines() == [
        f"Inbox Steering  <{token}>",
        "  pending=1 (recently shown; full readout on repeat or run "
        "`spice session briefing`)",
        f"  </{token}>",
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

    token = steering_token(tmp_path)
    assert second_context.splitlines() == [
        f"Inbox Steering  <{token}>",
        "  pending=1 (recently shown; full readout on repeat or run "
        "`spice session briefing`)",
        f"  </{token}>",
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
            last_maxim_bag="fallbacks",
        )
    ]
    stderr = io.StringIO()

    def new_injector() -> wrap.AgentWorkingStateInjector:
        return wrap.AgentWorkingStateInjector(
            tmp_path,
            stderr=stderr,
            time_factory=lambda: now[0],
            snapshot_factory=lambda _repo: snapshot[0],
        )

    new_injector().inject(force=True)
    # A fresh command far beyond any repeat interval with the identical state must
    # stay silent: the banner is a change notification, not a periodic meter. Only
    # the elapsed seconds (not part of the change key) advanced.
    now[0] = 100_000.0
    snapshot[0] = wrap.WorkingStateSnapshot(
        pending_inbox_count=1,
        claim_handle="METER-00000001",
        claim_phase="todo",
        claim_elapsed_seconds=93,
        last_maxim_bag="fallbacks",
    )
    new_injector().inject(force=True)
    # A real state change (a second pending inbox) re-emits.
    now[0] = 100_006.0
    snapshot[0] = wrap.WorkingStateSnapshot(
        pending_inbox_count=2,
        claim_handle="METER-00000001",
        claim_phase="todo",
        claim_elapsed_seconds=96,
        last_maxim_bag="fallbacks",
    )
    new_injector().inject(force=True)

    lines = stderr.getvalue().splitlines()
    assert lines == [
        (
            "🌶️ Working state: 1 pending inbox; claim METER-00000001 todo "
            "for 90s; last maxim fallbacks."
        ),
        (
            "🌶️ Working state: 2 pending inboxes; claim METER-00000001 todo "
            "for 96s; last maxim fallbacks."
        ),
    ]
    for line in lines:
        assert line.startswith("🌶️ ")
        assert "\n" not in line
        assert line.count(".") == 1


def _init_git_repo(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    subprocess.run(
        ["git", "config", "user.email", "spice@example.test"], cwd=path, check=True
    )
    subprocess.run(["git", "config", "user.name", "Spice Tests"], cwd=path, check=True)


def test_agent_import_parses_uuid_and_lists_in_help():
    parser = build_parser()
    subparsers = next(
        action
        for action in parser._actions
        if isinstance(action, argparse._SubParsersAction)
    )
    assert "import" in subparsers.choices["agent"].format_help()

    args = build_parser().parse_args(
        ["agent", "import", "f2249a9f-b996-41e2-9e18-54cb381cc634"]
    )
    assert args.agent_action == "import"
    assert args.func == agent_cli.handle_agent
    assert args.uuid == "f2249a9f-b996-41e2-9e18-54cb381cc634"


def test_agent_import_binds_external_thread_from_either_uuid_form(tmp_path):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    dashless = "f2249a9fb99641e29e1854cb381cc634"
    dashed = "f2249a9f-b996-41e2-9e18-54cb381cc634"

    status = lifecycle.import_agent(repo, dashed)
    # The dashed argument normalizes to the dashless canonical thread, and the
    # binding owns no process, so spice tracks it as idle without supervising.
    assert status.thread_id == dashless
    assert status.process_status == "idle"
    assert status.pid is None
    assert lifecycle.agent_status(repo).thread_id == dashless

    # The dashless form of the same UUID is the same binding.
    assert lifecycle.import_agent(repo, dashless).thread_id == dashless


def test_agent_import_rejects_non_uuid(tmp_path):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    with pytest.raises(SpiceError, match="not a thread UUID"):
        lifecycle.import_agent(repo, "not-a-uuid")


def test_agent_import_refuses_over_a_running_agent(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(
        lifecycle,
        "agent_status",
        lambda _root: SimpleNamespace(
            process_status="running", thread_id="live", pid=4321
        ),
    )
    with pytest.raises(SpiceError, match="already running"):
        lifecycle.import_agent(repo, "f2249a9f-b996-41e2-9e18-54cb381cc634")


def test_agent_reply_retires_acked_key_from_stdin(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_inbox_item(repo, "20260104T000000000004Z.txt", "please do the thing")
    assert collect_inbox_items(str(repo))  # pending before reply
    monkeypatch.setattr(
        agent_cli.sys,
        "stdin",
        io.StringIO("ACK 20260104T000000000004Z: did the thing\n"),
    )

    args = build_parser().parse_args(["agent", "reply"])
    assert agent_cli.handle_agent(args) == 0

    assert not collect_inbox_items(str(repo))  # the key was retired
    assert any(
        r.key == "20260104T000000000004Z"
        and r.disposition == ACK_DISPOSITION_ACKED
        and "did the thing" in (r.ack_content or "")
        for r in ack_state_records(repo)
    )


def test_agent_reply_accepts_text_as_positional(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_inbox_item(repo, "20260104T000000000004Z.txt", "please do the thing")

    args = build_parser().parse_args(
        ["agent", "reply", "ACK", "20260104T000000000004Z:", "handled it"]
    )
    assert agent_cli.handle_agent(args) == 0
    assert not collect_inbox_items(str(repo))


def test_agent_reply_without_a_header_is_an_error(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    args = build_parser().parse_args(["agent", "reply", "just some prose, no header"])
    with pytest.raises(SpiceError, match="no ACK or NACK header"):
        agent_cli.handle_agent(args)


def test_agent_reply_handles_ack_and_nack_together(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_inbox_item(repo, "20260104T000000000004Z.txt", "do X")
    write_inbox_item(repo, "20260104T000000000005Z.txt", "do Y")
    monkeypatch.setattr(
        agent_cli.sys,
        "stdin",
        io.StringIO(
            "ACK 20260104T000000000004Z: shipped X\n"
            "NACK 20260104T000000000005Z: Y is out of scope\n"
        ),
    )

    args = build_parser().parse_args(["agent", "reply"])
    assert agent_cli.handle_agent(args) == 0

    assert not collect_inbox_items(str(repo))  # both retired
    records = {r.key: r.disposition for r in ack_state_records(repo)}
    assert records["20260104T000000000004Z"] == ACK_DISPOSITION_ACKED
    assert records["20260104T000000000005Z"] == ACK_DISPOSITION_REFUSED
