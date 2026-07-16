"""Agent command-surface, side-channel, and inbox steering contracts."""

from __future__ import annotations

import io
import os
import subprocess
import sys
import time
from pathlib import Path
from threading import Event, Thread

import pytest

from spice.agent import driver as agent_driver
from spice.agent import sidechannel, sidechannelnotify, wrap
from spice.agent.maximmetrics import (
    MAXIM_EVENT_FIRE,
    MaximMetricEventWrite,
    record_maxim_metric_events,
)
from spice.agent.shadow import shadow_environment
from spice.mail.feedback import supervisor_feedback_line
from spice.mail.inbox import InboxResendAttempt, compose_inbox_text, write_inbox_item
from spice.mail import readout as inbox_readout
from spice.sessions.meter import ActiveContextSnapshot, ContextMeter

COMMAND_WORKING_STATE_ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
COMMAND_WORKING_STATE_NOW = 1_767_225_600.0
COMMAND_WORKING_STATE_MONOTONIC = 50_000.0
COMMAND_WORKING_STATE_ELAPSED_SECONDS = 90
COMMAND_WORKING_STATE_PROCESS_PID = 123
COMMAND_WORKING_STATE_EXIT_CODE = 0
COMMAND_WORKING_STATE_ONE_PENDING = 1
COMMAND_WORKING_STATE_TWO_PENDING = 2
COMMAND_WORKING_STATE_SENTENCE_PERIODS = 1
COMMAND_WORKING_STATE_INCEPTED = "00000001"
COMMAND_WORKING_STATE_HANDLE = f"METER-{COMMAND_WORKING_STATE_INCEPTED}"


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def _working_state_lines(text: str) -> list[str]:
    lines = [line for line in text.splitlines() if line.startswith("🌶️ Working state:")]
    for line in lines:
        assert "\n" not in line
        assert line.count(".") == COMMAND_WORKING_STATE_SENTENCE_PERIODS
    return lines


def _working_state_event_counts(*texts: str) -> list[int]:
    return [len(_working_state_lines(text)) for text in texts]


class _CommandWorkingStateProcess:
    pid = COMMAND_WORKING_STATE_PROCESS_PID

    def wait(self) -> int:
        return COMMAND_WORKING_STATE_EXIT_CODE


def _run_working_state_command(repo_root: Path) -> str:
    stderr = io.StringIO()
    exit_code = wrap.run_agent_command(
        repo_root,
        ["true"],
        popen_factory=lambda _command, env=None: _CommandWorkingStateProcess(),
        stderr=stderr,
    )
    assert exit_code == COMMAND_WORKING_STATE_EXIT_CODE
    return stderr.getvalue()


def _record_command_working_state_maxim(repo_root: Path, bag_name: str) -> None:
    record_maxim_metric_events(
        repo_root,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name=bag_name,
                driver_name=agent_driver.DRIVER.name,
                thread_id=COMMAND_WORKING_STATE_ACTOR,
                trigger_family=bag_name,
                statement=f"{bag_name} triggered",
            )
        ],
        now=COMMAND_WORKING_STATE_NOW,
    )


def _assert_command_working_state(
    text: str, *, pending: int, phase: str, maxim: str
) -> None:
    inbox_label = "pending inbox" if pending == 1 else "pending inboxes"
    assert _working_state_lines(text) == [
        (
            f"🌶️ Working state: {pending} {inbox_label}; claim "
            f"{COMMAND_WORKING_STATE_HANDLE} {phase} for "
            f"{COMMAND_WORKING_STATE_ELAPSED_SECONDS}s; "
            f"last maxim {maxim}."
        )
    ]


def _configure_command_working_state(tmp_path: Path, monkeypatch) -> list[str]:
    monkeypatch.setenv(agent_driver.DRIVER.thread_id_env, COMMAND_WORKING_STATE_ACTOR)
    monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args, **_kwargs: None)
    monkeypatch.setattr(wrap.time, "time", lambda: COMMAND_WORKING_STATE_NOW)
    monkeypatch.setattr(wrap.time, "monotonic", lambda: COMMAND_WORKING_STATE_MONOTONIC)
    monkeypatch.setattr(
        wrap, "start_agent_side_channel_watch", lambda *args, **kwargs: None
    )
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", lambda _thread: None)
    (tmp_path / ".git" / "info" / "exclude").write_text(".spice/\n", encoding="utf-8")
    claim_phase = ["todo"]
    claim_at = (
        wrap.datetime.datetime.fromtimestamp(
            COMMAND_WORKING_STATE_NOW - COMMAND_WORKING_STATE_ELAPSED_SECONDS,
            wrap.datetime.UTC,
        )
        .isoformat()
        .replace("+00:00", "Z")
    )

    def fake_export(args=None):
        assert args == ["+ACTIVE"]
        return [
            {
                "claim_at": claim_at,
                "claim_by": COMMAND_WORKING_STATE_ACTOR,
                "claim_worktree": str(tmp_path),
                "description": "Validate working-state stderr line end to end",
                "incepted": COMMAND_WORKING_STATE_INCEPTED,
                "phase": claim_phase[0],
                "project": "session.meter",
                "status": "pending",
            }
        ]

    monkeypatch.setattr("spice.tasks.tw.export", fake_export)
    return claim_phase


def test_wrapper_plain_exec_starts_side_channel_watch(tmp_path, monkeypatch):
    monkeypatch.delenv(agent_driver.DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args, **_kwargs: None)
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    class FakeProcess:
        pid = 123

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 7

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        events.append(
            (
                "popen",
                command,
                None if env is None else (env.get("ZDOTDIR"), env.get("BASH_ENV")),
            )
        )
        return FakeProcess()

    def fake_watch(
        repo_root,
        *,
        parent_pid,
        stderr,
        initial_inbox_signature=None,
    ):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_inbox_signature),
            )
        )
        return watch_thread

    def fake_join(thread):
        events.append(("join", thread, None))

    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", fake_watch)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", fake_join)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["find", ".", "-maxdepth", "0", "-print"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 7
    assert events == [
        ("popen", ["find", ".", "-maxdepth", "0", "-print"], None),
        ("watch", tmp_path, (123, stderr, ())),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_run_agent_command_initial_stderr_includes_working_state(tmp_path, monkeypatch):
    monkeypatch.delenv(agent_driver.DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args, **_kwargs: None)
    monkeypatch.setattr(
        wrap,
        "collect_working_state_snapshot",
        lambda _repo: wrap.WorkingStateSnapshot(
            claim_handle="METER-00000001",
            claim_phase="todo",
            claim_elapsed_seconds=90,
        ),
    )
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    class FakeProcess:
        pid = 123

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        events.append(("popen", command, env))
        return FakeProcess()

    def fake_watch(
        repo_root,
        *,
        parent_pid,
        stderr,
        initial_inbox_signature=None,
    ):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_inbox_signature),
            )
        )
        return watch_thread

    def fake_join(thread):
        events.append(("join", thread, None))

    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", fake_watch)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", fake_join)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["true"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 0
    assert stderr.getvalue().splitlines() == [
        "🌶️ Working state: claim METER-00000001 todo for 90s."
    ]
    assert events == [
        ("popen", ["true"], None),
        ("watch", tmp_path, (123, stderr, ())),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_run_agent_command_stderr_reflects_live_working_state_fields(
    tmp_path, monkeypatch
):
    claim_phase = _configure_command_working_state(tmp_path, monkeypatch)
    write_inbox_item(
        tmp_path,
        "20260101T000000000010Z.txt",
        compose_inbox_text(body="pending command work", priority=None, stop=False),
    )
    _record_command_working_state_maxim(tmp_path, "fallbacks")

    first = _run_working_state_command(tmp_path)
    assert "Inbox Steering" in first
    assert "pending command work" in first
    _assert_command_working_state(
        first,
        pending=COMMAND_WORKING_STATE_ONE_PENDING,
        phase="todo",
        maxim="fallbacks",
    )

    # An unchanged state stays silent: the banner is a change notification, not a
    # per-command meter.
    repeat = _run_working_state_command(tmp_path)

    write_inbox_item(
        tmp_path,
        "20260101T000000000011Z.txt",
        compose_inbox_text(
            body="second pending command work", priority=None, stop=False
        ),
    )
    second_pending = _run_working_state_command(tmp_path)
    _assert_command_working_state(
        second_pending,
        pending=COMMAND_WORKING_STATE_TWO_PENDING,
        phase="todo",
        maxim="fallbacks",
    )

    claim_phase[0] = "verify"
    phase_changed = _run_working_state_command(tmp_path)
    _assert_command_working_state(
        phase_changed,
        pending=COMMAND_WORKING_STATE_TWO_PENDING,
        phase="verify",
        maxim="fallbacks",
    )

    _record_command_working_state_maxim(tmp_path, "aliases")
    maxim_changed = _run_working_state_command(tmp_path)
    _assert_command_working_state(
        maxim_changed,
        pending=COMMAND_WORKING_STATE_TWO_PENDING,
        phase="verify",
        maxim="aliases",
    )
    assert _working_state_event_counts(
        first,
        repeat,
        second_pending,
        phase_changed,
        maxim_changed,
    ) == [1, 0, 1, 1, 1]


def test_run_agent_command_rewrites_stage_one_shell_before_popen(tmp_path, monkeypatch):
    monkeypatch.delenv(agent_driver.DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)
    calls: list[tuple[str, ...]] = []
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return "rtk git status --short"

    class FakeProcess:
        pid = 321

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        env_snapshot = (
            None if env is None else (env.get("ZDOTDIR"), env.get("BASH_ENV"))
        )
        events.append(("popen", command, env_snapshot))
        return FakeProcess()

    def fake_watch(
        repo_root,
        *,
        parent_pid,
        stderr,
        initial_inbox_signature=None,
    ):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_inbox_signature),
            )
        )
        return watch_thread

    def fake_join(thread):
        events.append(("join", thread, None))

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)
    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", fake_watch)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", fake_join)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["zsh", "-c", "git status --short"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 0
    assert calls == [("git status --short",)]
    static_hook_dir = wrap.packaged_shell_steering_static_hook_dir()
    assert events == [
        (
            "popen",
            ["zsh", "-c", "rtk git status --short"],
            (str(static_hook_dir), str(static_hook_dir / wrap.BASH_HOOK_NAME)),
        ),
        ("watch", tmp_path, (321, stderr, ())),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_run_agent_command_reports_missing_command_without_traceback(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args, **_kwargs: None)
    stderr = io.StringIO()

    def fake_popen(command, env=None):
        raise FileNotFoundError(2, "No such file or directory", command[0])

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["nonexistent-cmd-xyz"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == wrap.COMMAND_NOT_FOUND_EXIT_CODE
    assert "command not found: nonexistent-cmd-xyz" in stderr.getvalue()


def test_wrapper_leaves_plain_commands_native_without_rtk_rewrite():
    assert wrap.build_agent_run_command(["find", ".", "-maxdepth", "0", "-print"]) == [
        "find",
        ".",
        "-maxdepth",
        "0",
        "-print",
    ]
    assert wrap.build_agent_run_command(
        ["find", ".", "(", "-name", "*.py", "-o", "-name", "*.md", ")", "-print"]
    ) == [
        "find",
        ".",
        "(",
        "-name",
        "*.py",
        "-o",
        "-name",
        "*.md",
        ")",
        "-print",
    ]
    assert wrap.build_agent_run_command(["find", ".", "-name", "*.py"]) == [
        "find",
        ".",
        "-name",
        "*.py",
    ]
    assert wrap.build_agent_run_command(
        ["proxy", "find", ".", "-maxdepth", "0", "-print"]
    ) == ["proxy", "find", ".", "-maxdepth", "0", "-print"]
    assert wrap.build_agent_run_command(["rg", "needle"]) == ["rg", "needle"]


CLAUDE_EVAL_ENVELOPE = (
    "source /tmp/snapshot-zsh-1.sh 2>/dev/null || true "
    "&& setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true "
    "&& eval 'git show HEAD' < /dev/null && pwd -P >| /tmp/claude-cwd"
)


def test_wrapper_rewrites_claude_eval_envelope_inner_command(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return "rtk git show HEAD" if args == ("git show HEAD",) else None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)
    monkeypatch.setattr(
        wrap, "driver_for", lambda _repo_root: agent_driver.CLAUDE_DRIVER
    )

    rewritten = wrap.build_agent_run_command(
        ["zsh", "-c", CLAUDE_EVAL_ENVELOPE], rewrite_rtk=True
    )

    assert calls == [(CLAUDE_EVAL_ENVELOPE,), ("git show HEAD",)]
    assert rewritten == [
        "zsh",
        "-c",
        (
            "source /tmp/snapshot-zsh-1.sh 2>/dev/null "
            "|| true && setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null "
            "|| true && eval 'rtk git show HEAD' < /dev/null && pwd -P >| /tmp/claude-cwd"
        ),
    ]


def test_wrapper_eval_envelope_preserves_embedded_single_quotes(monkeypatch):
    envelope = "x=1 && eval 'echo '\\''hi there'\\''' < /dev/null && pwd"
    seen: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        seen.append(args)
        return None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    assert (
        agent_driver.CLAUDE_DRIVER.rewrite_tool_command(envelope, fake_rewrite) is None
    )
    assert seen == [("echo 'hi there'",)]


def test_wrapper_leaves_non_eval_commands_native(monkeypatch):
    def rewrite(*args: str) -> str | None:
        return None

    assert (
        agent_driver.CLAUDE_DRIVER.rewrite_tool_command("git status --short", rewrite)
        is None
    )
    assert (
        agent_driver.CLAUDE_DRIVER.rewrite_tool_command(
            "exec bash -lc 'git show'", rewrite
        )
        is None
    )


def test_shell_word_end_tracks_quotes_and_escapes():
    text = "eval 'a b'\\''c' rest"
    start = len("eval ")
    end = agent_driver.shell_word_end(text, start)
    assert text[start:end] == "'a b'\\''c'"
    double = 'eval "a \\" b" rest'
    dstart = len("eval ")
    assert double[dstart : agent_driver.shell_word_end(double, dstart)] == '"a \\" b"'


def test_wrapper_runs_plain_find_natively(tmp_path, monkeypatch):
    monkeypatch.delenv(agent_driver.DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args, **_kwargs: None)
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    class FakeProcess:
        pid = 321

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        events.append(
            (
                "popen",
                command,
                None if env is None else (env.get("ZDOTDIR"), env.get("BASH_ENV")),
            )
        )
        return FakeProcess()

    def fake_watch(
        repo_root,
        *,
        parent_pid,
        stderr,
        initial_inbox_signature=None,
    ):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_inbox_signature),
            )
        )
        return watch_thread

    def fake_join(thread):
        events.append(("join", thread, None))

    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", fake_watch)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", fake_join)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["find", ".", "-name", "*.py"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 0
    assert events == [
        ("popen", ["find", ".", "-name", "*.py"], None),
        ("watch", tmp_path, (321, stderr, ())),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_agent_run_direct_git_inherits_ambient_shadow_environment(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(agent_driver.DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(agent_driver.CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setenv("GIT_CONFIG_SYSTEM", "shadow-system")
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args, **_kwargs: None)
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    class FakeProcess:
        pid = 321

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        source = "ambient" if env is None else "explicit"
        shadow = (
            os.environ["GIT_CONFIG_SYSTEM"]  # env-policy: allow
            if env is None
            else env.get("GIT_CONFIG_SYSTEM")
        )
        events.append(("popen", command, (source, shadow)))
        return FakeProcess()

    def fake_watch(
        repo_root,
        *,
        parent_pid,
        stderr,
        initial_inbox_signature=None,
    ):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_inbox_signature),
            )
        )
        return watch_thread

    def fake_join(thread):
        events.append(("join", thread, None))

    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", fake_watch)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", fake_join)

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["git", "status"],
        popen_factory=fake_popen,
        stderr=stderr,
    )

    assert exit_code == 0
    assert events == [
        ("popen", ["git", "status"], ("ambient", "shadow-system")),
        ("watch", tmp_path, (321, stderr, ())),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_shadow_environment_masks_upstream_to_self(tmp_path):
    repo = tmp_path / "lane"
    subprocess.run(["git", "init", "-q", "-b", "main-d", str(repo)], check=True)
    for key, value in (
        ("user.email", "t@t.t"),
        ("user.name", "t"),
        # Native tracking the operator (no env) sees: a real upstream.
        ("branch.main-d.remote", "origin"),
        ("branch.main-d.merge", "refs/heads/main"),
    ):
        subprocess.run(["git", "-C", str(repo), "config", key, value], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "--allow-empty", "-m", "c0"],
        check=True,
    )

    env = shadow_environment(
        repo, base_env={"PATH": os.environ["PATH"]}
    )  # env-policy: allow

    # System config (read first) carries the self merge; remote=. is appended last.
    assert "GIT_CONFIG_SYSTEM" in env
    assert env[f"GIT_CONFIG_KEY_{int(env['GIT_CONFIG_COUNT']) - 1}"] == (
        "branch.main-d.remote"
    )
    self_config = Path(env["GIT_CONFIG_SYSTEM"]).read_text(encoding="utf-8")
    assert "merge = refs/heads/main-d" in self_config

    # The agent (with env) resolves upstream to itself...
    agent = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--abbrev-ref", "main-d@{upstream}"],
        capture_output=True,
        text=True,
        env={**os.environ, **env},  # env-policy: allow
    )
    assert agent.stdout.strip() == "main-d"
    # ...while the operator (no env) still sees the native branch.merge as truth.
    truth = subprocess.run(
        ["git", "-C", str(repo), "config", "--get", "branch.main-d.merge"],
        capture_output=True,
        text=True,
        env={**os.environ, **env},  # env-policy: allow
    )
    assert truth.stdout.strip() == "refs/heads/main"


def test_shadow_environment_reinjection_is_idempotent(tmp_path):
    repo = tmp_path / "lane"
    subprocess.run(["git", "init", "-q", "-b", "main-d", str(repo)], check=True)
    for key, value in (("user.email", "t@t.t"), ("user.name", "t")):
        subprocess.run(["git", "-C", str(repo), "config", key, value], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "--allow-empty", "-m", "c0"],
        check=True,
    )

    first = shadow_environment(
        repo, base_env={"PATH": os.environ["PATH"]}
    )  # env-policy: allow
    # Re-applying on an env that already carries the shadow (lifecycle env, then
    # the wrap per-command re-apply) must not append a duplicate remote pair.
    second = shadow_environment(repo, base_env=first)

    assert first["GIT_CONFIG_COUNT"] == "1"
    assert second["GIT_CONFIG_COUNT"] == "1"
    remote_keys = [v for k, v in second.items() if k.startswith("GIT_CONFIG_KEY_")]
    assert remote_keys.count("branch.main-d.remote") == 1


def test_inbox_injector_repeats_pending_steering_after_interval(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000001Z.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    now = [0.0]
    stderr = io.StringIO()
    injector = wrap.AgentInboxInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: now[0],
    )

    injector.inject(force=False)
    now[0] = 10.0
    injector.inject(force=False)
    now[0] = 16.0
    injector.inject(force=False)

    output = stderr.getvalue()
    # Full readout at t=0 and again after the 15s repeat interval (t=16); the
    # suppressed inject at t=10 surfaces only a one-line pending count so the
    # command never looks empty while steering waits.
    assert output.count("operator steering") == 2
    assert output.count("recently shown") == 1
    assert "resend #" not in output
    assert "Task offload: capture in the moment" in output
    assert "standalone TASK line" in output
    assert "TASK title=... | project=<stem.child> [| acceptance=...]" in output
    assert "omitted acceptance with no flow starts in plan" in output
    assert "repeat acceptance=... for multiple criteria" in output
    assert "ACK prose first and then the TASK line on its own line" in output
    assert "same task-add batch format" in output


def test_inbox_injector_repeats_already_shown_item_after_new_key(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000001Z.txt",
        compose_inbox_text(
            body="first steering",
            priority="critical",
            stop=False,
            resend_attempts=(
                InboxResendAttempt(
                    attempt=1,
                    at="2026-01-01T00:00:00Z",
                    messages_elapsed=3,
                ),
                InboxResendAttempt(
                    attempt=2,
                    at="2026-01-01T00:01:00Z",
                    messages_elapsed=4,
                ),
            ),
        ),
    )
    now = [0.0]
    stderr = io.StringIO()
    injector = wrap.AgentInboxInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: now[0],
    )

    injector.inject(force=False)
    # A new key arrives while the first is still inside its suppression window.
    now[0] = 5.0
    write_inbox_item(
        tmp_path,
        "20260101T000000000002Z.txt",
        compose_inbox_text(body="second steering", priority=None, stop=False),
    )
    injector.inject(force=False)
    # The first key has now aged past the 15s repeat cadence, even though a new
    # key arrived in the meantime; it must render full again instead of staying
    # compact forever.
    now[0] = 16.0
    injector.inject(force=False)

    output = stderr.getvalue()
    # The new key renders full (real-time delivery preserved); the already-shown
    # key first collapses to one compact summary line, then renders full again
    # after the repeat interval.
    assert output.count("first steering") == 2
    assert output.count("second steering") == 1
    assert output.count("shown earlier; ACK to clear") == 2
    assert "priority=critical resend #2 (shown earlier; ACK to clear)" in output
    assert output.count("resend #2") == 3


def test_inbox_injector_suppresses_task_offload_for_maxim_guidance(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000002Z.txt",
        compose_inbox_text(
            body="No separate task is needed for the maxim itself.",
            priority="maxim",
            stop=False,
        ),
    )
    stderr = io.StringIO()
    injector = wrap.AgentInboxInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: 0.0,
    )

    injector.inject(force=False)

    output = stderr.getvalue()
    assert "priority=maxim" in output
    assert "No separate task is needed for the maxim itself." in output
    assert "Task offload: capture in the moment" not in output


def test_side_channel_payload_keeps_inbox_context_and_working_state_single_line(
    tmp_path, monkeypatch
):
    write_inbox_item(
        tmp_path,
        "20260101T000000000007Z.txt",
        compose_inbox_text(body="payload steering", priority=None, stop=False),
    )
    monkeypatch.setattr(
        sidechannel,
        "agent_context_meter",
        lambda _repo: _pressure_context_meter(),
        raising=False,
    )
    monkeypatch.setattr(
        wrap,
        "collect_working_state_snapshot",
        lambda _repo: wrap.WorkingStateSnapshot(
            last_maxim_bag="fallbacks",
        ),
    )

    payload, _signature = sidechannel.render_side_channel_payload(tmp_path)

    assert payload.splitlines()[0].startswith("Inbox Steering")
    assert "payload steering" in payload
    working_lines = [line for line in payload.splitlines() if line.startswith("🌶️ ")]
    assert working_lines == ["🌶️ Working state: last maxim fallbacks."]
    assert "\n" not in working_lines[0]
    assert working_lines[0].count(".") == 1
    assert payload.splitlines()[-2].startswith("  </")
    assert payload.splitlines()[-1] == "🌶️ Working state: last maxim fallbacks."


def test_post_tool_hook_payload_keeps_inbox_without_context_pressure(
    tmp_path, monkeypatch
):
    write_inbox_item(
        tmp_path,
        "20260101T000000000008Z.txt",
        compose_inbox_text(body="post-tool steering", priority=None, stop=False),
    )
    monkeypatch.setattr(
        sidechannel,
        "agent_context_meter",
        lambda _repo: _pressure_context_meter(),
        raising=False,
    )

    payload = sidechannel.render_post_tool_hook_payload(tmp_path)

    lines = payload.splitlines()
    assert lines[0].startswith("Inbox Steering")
    assert "post-tool steering" in payload
    assert lines[-1].startswith("  </")


def test_side_channel_working_state_suppresses_repeats_and_post_tool_omits(
    tmp_path, monkeypatch
):
    snapshot = wrap.WorkingStateSnapshot(
        claim_handle="METER-00000002",
        claim_phase="todo",
        claim_elapsed_seconds=5,
    )
    monkeypatch.setattr(
        wrap,
        "collect_working_state_snapshot",
        lambda _repo: snapshot,
    )

    first, _first_signature = sidechannel.render_side_channel_payload(tmp_path)
    second, _second_signature = sidechannel.render_side_channel_payload(tmp_path)
    post_tool = sidechannel.render_post_tool_hook_payload(tmp_path)

    assert first.splitlines() == ["🌶️ Working state: claim METER-00000002 todo for 5s."]
    assert _working_state_event_counts(first, second, post_tool) == [1, 0, 0]

    monkeypatch.setattr(
        wrap,
        "collect_working_state_snapshot",
        lambda _repo: wrap.WorkingStateSnapshot(),
    )
    empty_payload, empty_signature = sidechannel.render_side_channel_payload(tmp_path)
    assert (empty_payload, empty_signature) == ("", ())


def test_side_channel_watch_streams_later_inbox_to_stderr(tmp_path, monkeypatch):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": os.getpid(),
                "stderr": stderr,
            },
        )
        thread.start()
        write_inbox_item(
            tmp_path,
            "20260101T000000000003Z.txt",
            compose_inbox_text(body="late steering", priority=None, stop=False),
        )
        output = _eventually(lambda: stderr.getvalue(), contains="late steering")

    thread.join(timeout=1.0)
    assert "Inbox Steering" in output
    # The late item's full readout streams exactly once; any later suppressed
    # inject surfaces only the one-line pending count, not a second full readout.
    assert output.count("late steering") == 1
    assert not thread.is_alive()


def test_side_channel_stream_hello_uses_inbox_signature_shape(tmp_path):
    signature = (("20260101T000000000001Z.txt", 123, 45),)

    hello = wrap.agent_side_channel_hello(
        tmp_path,
        runner="agent.run.watch",
        stream_until_parent_exit=321,
        initial_inbox_signature=signature,
    )

    assert hello == {
        "type": "hello",
        "pid": os.getpid(),
        "ppid": os.getppid(),
        "runner": "agent.run.watch",
        "cwd": os.getcwd(),
        "repoRoot": str(tmp_path),
        "streamUntilParentExit": 321,
        "initialInboxSignature": [["20260101T000000000001Z.txt", 123, 45]],
    }


def test_side_channel_notice_queue_consumes_once(tmp_path):
    notice = supervisor_feedback_line(
        "task.created", handles=["ACKS-20260101T000000000001Z"]
    )
    sidechannelnotify.publish_side_channel_feedback(
        tmp_path, "task.created", handles=["ACKS-20260101T000000000001Z"]
    )

    first = sidechannelnotify.consume_side_channel_notices(tmp_path)
    second = sidechannelnotify.consume_side_channel_notices(tmp_path)

    assert first == [notice]
    assert second == []


def test_side_channel_watch_streams_queued_notice_after_initial_payload(
    tmp_path, monkeypatch
):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    notice = supervisor_feedback_line(
        "task.created", handles=["ACKS-20260101T000000000002Z"]
    )
    sidechannelnotify.publish_side_channel_feedback(
        tmp_path, "task.created", handles=["ACKS-20260101T000000000002Z"]
    )

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": os.getpid(),
                "stderr": stderr,
                "initial_inbox_signature": (),
            },
        )
        thread.start()
        output = _eventually(lambda: stderr.getvalue(), contains="000000000002Z")

    thread.join(timeout=1.0)
    assert "Supervisor Feedback" in output
    assert notice in output
    assert output.count("000000000002Z") == 1
    assert not thread.is_alive()


def test_side_channel_watch_keeps_supervisor_feedback_and_working_state(
    tmp_path, monkeypatch
):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(
        wrap,
        "collect_working_state_snapshot",
        lambda _repo: wrap.WorkingStateSnapshot(last_maxim_bag="fallbacks"),
    )
    notice = supervisor_feedback_line("task.error", error="batch add rejected")
    sidechannelnotify.publish_side_channel_feedback(
        tmp_path, "task.error", error="batch add rejected"
    )

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": os.getpid(),
                "stderr": stderr,
            },
        )
        thread.start()
        output = _eventually(lambda: stderr.getvalue(), contains="Working state")

    thread.join(timeout=1.0)
    assert "Supervisor Feedback" in output
    assert notice in output
    assert output.count("batch add rejected") == 1
    assert output.count("🌶️ Working state: last maxim fallbacks.") == 1
    assert not thread.is_alive()


def test_side_channel_watch_streams_later_notice_to_stderr(tmp_path, monkeypatch):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": os.getpid(),
                "stderr": stderr,
            },
        )
        thread.start()
        notice = supervisor_feedback_line("task.error", error="batch add rejected")
        sidechannelnotify.publish_side_channel_feedback(
            tmp_path, "task.error", error="batch add rejected"
        )
        output = _eventually(lambda: stderr.getvalue(), contains="batch add rejected")

    thread.join(timeout=1.0)
    assert "Supervisor Feedback" in output
    assert notice in output
    assert output.count("batch add rejected") == 1
    assert not thread.is_alive()


def test_side_channel_streams_to_each_connection_without_cross_suppression(
    tmp_path, monkeypatch
):
    # Two overlapping connections share the same 15s window; with per-connection
    # injectors each still gets the full readout (a shared injector would
    # suppress the second). Short commands must never be cross-suppressed.
    monkeypatch.chdir(tmp_path)
    write_inbox_item(
        tmp_path,
        "20260101T000000000004Z.txt",
        compose_inbox_text(body="multi connection steering", priority=None, stop=False),
    )
    stderr_a = io.StringIO()
    stderr_b = io.StringIO()

    with sidechannel.AgentSideChannelServer(tmp_path):
        threads = [
            Thread(
                target=wrap.watch_agent_side_channel,
                kwargs={
                    "repo_root": tmp_path,
                    "parent_pid": os.getpid(),
                    "stderr": buf,
                },
            )
            for buf in (stderr_a, stderr_b)
        ]
        for thread in threads:
            thread.start()
        out_a = _eventually(
            lambda: stderr_a.getvalue(), contains="multi connection steering"
        )
        out_b = _eventually(
            lambda: stderr_b.getvalue(), contains="multi connection steering"
        )

    for thread in threads:
        thread.join(timeout=1.0)
    assert "multi connection steering" in out_a
    assert "multi connection steering" in out_b


def test_run_agent_command_streams_later_side_channel_while_child_runs(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    stderr = io.StringIO()
    ready = tmp_path / "ready"
    results: list[int] = []
    registration_started = Event()
    allow_registration = Event()
    script = (
        "from pathlib import Path; "
        "import sys, time; "
        "Path(sys.argv[1]).write_text('ready', encoding='utf-8'); "
        "time.sleep(0.4)"
    )

    with sidechannel.AgentSideChannelServer(tmp_path) as server:
        register_stream_wakeup = server._register_stream_wakeup

        def register_after_notification(wake_writer):
            registration_started.set()
            assert allow_registration.wait(timeout=2.0)
            register_stream_wakeup(wake_writer)

        monkeypatch.setattr(
            server, "_register_stream_wakeup", register_after_notification
        )
        thread = Thread(
            target=lambda: results.append(
                wrap.run_agent_command(
                    tmp_path,
                    [sys.executable, "-c", script, str(ready)],
                    stderr=stderr,
                )
            )
        )
        thread.start()
        _eventually(lambda: "ready" if ready.exists() else "", contains="ready")
        assert registration_started.wait(timeout=1.0)
        write_inbox_item(
            tmp_path,
            "20260101T000000000004Z.txt",
            compose_inbox_text(body="runner steering", priority=None, stop=False),
        )
        allow_registration.set()
        output = _eventually(lambda: stderr.getvalue(), contains="runner steering")
        thread.join(timeout=2.0)

    assert results == [0]
    assert "Inbox Steering" in output
    assert output.count("Inbox Steering") == 1


def test_run_agent_command_does_not_duplicate_initial_side_channel_with_watch(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    write_inbox_item(
        tmp_path,
        "20260101T000000000005Z.txt",
        compose_inbox_text(body="initial steering", priority=None, stop=False),
    )
    stderr = io.StringIO()

    with sidechannel.AgentSideChannelServer(tmp_path):
        exit_code = wrap.run_agent_command(
            tmp_path,
            [sys.executable, "-c", "import time; time.sleep(0.1)"],
            stderr=stderr,
        )

    output = stderr.getvalue()
    assert exit_code == 0
    assert "initial steering" in output
    assert output.count("Inbox Steering") == 1


def test_run_agent_command_delivers_interleaved_initial_inbox_row_once(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    initial = write_inbox_item(
        tmp_path,
        "20260101T000000000005Z.txt",
        compose_inbox_text(body="initial snapshot steering", priority=None, stop=False),
    )
    initial_stat = initial.stat()
    late_name = "20260101T000000000006Z.txt"
    original_print = inbox_readout.print_inbox_readout
    original_start = wrap.start_agent_side_channel_watch
    injected = False
    stream_signatures = []

    def print_with_interleaved_row(*args, **kwargs):
        nonlocal injected
        if kwargs.get("items") is not None and not injected:
            injected = True
            write_inbox_item(
                tmp_path,
                late_name,
                compose_inbox_text(
                    body="interleaved snapshot steering", priority=None, stop=False
                ),
            )
        return original_print(*args, **kwargs)

    def start_with_signature(*args, initial_inbox_signature=None, **kwargs):
        stream_signatures.append(initial_inbox_signature)
        return original_start(
            *args,
            initial_inbox_signature=initial_inbox_signature,
            **kwargs,
        )

    monkeypatch.setattr(
        inbox_readout, "print_inbox_readout", print_with_interleaved_row
    )
    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", start_with_signature)
    stderr = io.StringIO()

    with sidechannel.AgentSideChannelServer(tmp_path):
        exit_code = wrap.run_agent_command(
            tmp_path,
            [sys.executable, "-c", "import time; time.sleep(0.4)"],
            stderr=stderr,
        )

    output = stderr.getvalue()
    assert exit_code == 0
    assert stream_signatures == [
        ((initial.name, initial_stat.st_mtime_ns, initial_stat.st_size),)
    ]
    assert output.count("interleaved snapshot steering") == 1


def test_run_agent_command_dumps_initial_inbox_without_side_channel_server(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    write_inbox_item(
        tmp_path,
        "20260101T000000000006Z.txt",
        compose_inbox_text(body="synchronous steering", priority=None, stop=False),
    )
    stderr = io.StringIO()

    exit_code = wrap.run_agent_command(
        tmp_path,
        [sys.executable, "-c", ""],
        stderr=stderr,
    )

    output = stderr.getvalue()
    assert exit_code == 0
    assert "synchronous steering" in output
    assert output.count("Inbox Steering") == 1


def test_side_channel_watch_exits_when_parent_pid_exits_without_shell_trap(
    tmp_path, monkeypatch
):
    watcher = wrap._parent_exit_watcher(os.getpid())
    if watcher is None:
        pytest.skip("platform does not expose process-exit watch handles")
    watcher.close()
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    parent = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(0.1)"])

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": parent.pid,
                "stderr": stderr,
            },
        )
        thread.start()
        parent.wait(timeout=2.0)
        thread.join(timeout=2.0)
        alive = thread.is_alive()

    assert not alive


def _eventually(factory, *, contains: str):
    deadline = time.monotonic() + 2.0
    latest = factory()
    while time.monotonic() < deadline:
        if _contains(latest, contains):
            return latest
        time.sleep(0.05)
        latest = factory()
    return latest


def _contains(value, needle: str) -> bool:
    if isinstance(value, str):
        return needle in value
    return any(needle in item for item in value)


def _pressure_context_meter() -> ContextMeter:
    snapshot = ActiveContextSnapshot(
        source_file="rollout.jsonl",
        ts="2026-01-01T00:00:00.000Z",
        input_tokens=80_000,
        cached_input_tokens=0,
        output_tokens=0,
        reasoning_output_tokens=0,
        total_tokens=80_000,
        model_context_window=100_000,
        cumulative_total_tokens=80_000,
    )
    return ContextMeter(
        source_files=("rollout.jsonl",),
        latest_snapshot=snapshot,
        snapshot_count=1,
        compaction_count=0,
        latest_compaction_ts=None,
        snapshots_since_compaction=1,
        pre_compaction_min_tokens=None,
        pre_compaction_median_tokens=None,
        pre_compaction_max_tokens=None,
        pre_compaction_min_percent=None,
        pre_compaction_median_percent=None,
        pre_compaction_max_percent=None,
    )
