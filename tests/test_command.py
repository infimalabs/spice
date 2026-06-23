"""Agent command-surface, side-channel, and inbox steering contracts."""

from __future__ import annotations

import io
import os
import subprocess
import sys
import time
from pathlib import Path
from threading import Thread

import pytest

from spice.agent import sidechannel, sidechannelnotify, wrap
from spice.agent.shadow import shadow_environment
from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.inbox import compose_inbox_text, write_inbox_item
from spice.sessions.meter import (
    ActiveContextSnapshot,
    ContextMeter,
    context_meter_instruction,
)


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


def test_wrapper_plain_exec_starts_side_channel_watch(tmp_path, monkeypatch):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args: None)
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

    def fake_watch(repo_root, *, parent_pid, stderr, initial_payload_already_rendered):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_payload_already_rendered),
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
        ("watch", tmp_path, (123, stderr, True)),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_run_agent_command_rewrites_stage_one_shell_before_popen(tmp_path, monkeypatch):
    calls: list[tuple[str, ...]] = []
    events: list[tuple[str, object, object | None]] = []
    stderr = io.StringIO()
    watch_thread = object()

    def fake_rewrite(*args: str) -> str | None:
        calls.append(args)
        return "rtk git status --short"

    class FakeProcess:
        pid = 321

        def wait(self) -> int:
            events.append(("wait", None, None))
            return 0

    def fake_popen(command: list[str], env=None) -> FakeProcess:
        events.append(("popen", command, env))
        return FakeProcess()

    def fake_watch(repo_root, *, parent_pid, stderr, initial_payload_already_rendered):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_payload_already_rendered),
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
    assert events == [
        ("popen", ["zsh", "-c", "rtk git status --short"], None),
        ("watch", tmp_path, (321, stderr, True)),
        ("wait", None, None),
        ("join", watch_thread, None),
    ]


def test_run_agent_command_reports_missing_command_without_traceback(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args: None)
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

    def fake_rewrite(*args: str) -> str | None:
        calls.append(args)
        return "rtk git show HEAD" if args == ("git show HEAD",) else None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

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

    def fake_rewrite(*args: str) -> str | None:
        seen.append(args)
        return None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    assert wrap.rtk_rewrite_eval_envelope_command(envelope) is None
    assert seen == [("echo 'hi there'",)]


def test_wrapper_leaves_non_eval_commands_native(monkeypatch):
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args: None)
    assert wrap.rtk_rewrite_eval_envelope_command("git status --short") is None
    assert wrap.rtk_rewrite_eval_envelope_command("exec bash -lc 'git show'") is None


def test_shell_word_end_tracks_quotes_and_escapes():
    text = "eval 'a b'\\''c' rest"
    start = len("eval ")
    end = wrap.shell_word_end(text, start)
    assert text[start:end] == "'a b'\\''c'"
    double = 'eval "a \\" b" rest'
    dstart = len("eval ")
    assert double[dstart : wrap.shell_word_end(double, dstart)] == '"a \\" b"'


def test_wrapper_runs_plain_find_natively(tmp_path, monkeypatch):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", lambda *args: None)
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

    def fake_watch(repo_root, *, parent_pid, stderr, initial_payload_already_rendered):
        events.append(
            (
                "watch",
                repo_root,
                (parent_pid, stderr, initial_payload_already_rendered),
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
        ("watch", tmp_path, (321, stderr, True)),
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

    env = shadow_environment(repo, base_env={"PATH": os.environ["PATH"]})

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
        env={**os.environ, **env},
    )
    assert agent.stdout.strip() == "main-d"
    # ...while the operator (no env) still sees the native branch.merge as truth.
    truth = subprocess.run(
        ["git", "-C", str(repo), "config", "--get", "branch.main-d.merge"],
        capture_output=True,
        text=True,
        env={**os.environ, **env},
    )
    assert truth.stdout.strip() == "refs/heads/main"


def test_shadow_environment_reinjection_is_idempotent(tmp_path):
    repo = tmp_path / "lane"
    subprocess.run(["git", "init", "-q", "-b", "main-d", str(repo)], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "--allow-empty", "-m", "c0"],
        check=True,
    )

    first = shadow_environment(repo, base_env={"PATH": os.environ["PATH"]})
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
    assert "Task offload: capture in the moment" in output
    assert "standalone TASK line" in output
    assert "TASK title=... | project=<stem.child> | acceptance=..." in output
    assert "ACK prose first and then the TASK line on its own line" in output
    assert "same task-add batch format" in output


def test_inbox_injector_surfaces_clear_after_ack_removes_last_pending(tmp_path):
    key = "20260101T000000000001Z"
    write_inbox_item(
        tmp_path,
        f"{key}.txt",
        compose_inbox_text(body="operator steering", priority=None, stop=False),
    )
    stderr = io.StringIO()
    injector = wrap.AgentInboxInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: 0.0,
    )

    injector.inject(force=False)
    archived = archive_ackd_inbox_items(tmp_path, [key])
    injector.inject(force=False)

    assert archived == [key]
    assert stderr.getvalue().endswith("Inbox Steering\n  pending=none\n")


def test_inbox_injector_repeats_already_shown_item_after_new_key(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000001Z.txt",
        compose_inbox_text(body="first steering", priority=None, stop=False),
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


def test_context_meter_injector_repeats_warning_after_interval(tmp_path):
    now = [100.0]
    stderr = io.StringIO()
    meter = _context_meter(total_tokens=80_000, window=100_000)
    injector = wrap.AgentContextMeterInjector(
        tmp_path,
        stderr=stderr,
        repeat_interval_seconds=15.0,
        time_factory=lambda: now[0],
        meter_factory=lambda _repo: meter,
    )

    injector.inject(force=False)
    now[0] = 110.0
    injector.inject(force=False)
    now[0] = 116.0
    injector.inject(force=False)

    output = stderr.getvalue()
    guidance = context_meter_instruction("yellow")
    assert output.strip().splitlines() == [guidance, guidance]


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


def test_side_channel_notice_queue_consumes_once(tmp_path):
    sidechannelnotify.publish_side_channel_notice(
        tmp_path, "inline_task_created=ACKS-20260101T000000000001Z"
    )

    first = sidechannelnotify.consume_side_channel_notices(tmp_path)
    second = sidechannelnotify.consume_side_channel_notices(tmp_path)

    assert first == ["inline_task_created=ACKS-20260101T000000000001Z"]
    assert second == []


def test_side_channel_watch_streams_queued_notice_after_initial_payload(
    tmp_path, monkeypatch
):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    sidechannelnotify.publish_side_channel_notice(
        tmp_path, "inline_task_created=ACKS-20260101T000000000002Z"
    )

    with sidechannel.AgentSideChannelServer(tmp_path):
        thread = Thread(
            target=wrap.watch_agent_side_channel,
            kwargs={
                "repo_root": tmp_path,
                "parent_pid": os.getpid(),
                "stderr": stderr,
                "initial_payload_already_rendered": True,
            },
        )
        thread.start()
        output = _eventually(lambda: stderr.getvalue(), contains="000000000002Z")

    thread.join(timeout=1.0)
    assert "Supervisor Feedback" in output
    assert "inline_task_created=ACKS-20260101T000000000002Z" in output
    assert output.count("000000000002Z") == 1
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
        sidechannelnotify.publish_side_channel_notice(
            tmp_path, "inline_task_error=batch add rejected"
        )
        output = _eventually(lambda: stderr.getvalue(), contains="batch add rejected")

    thread.join(timeout=1.0)
    assert "Supervisor Feedback" in output
    assert "inline_task_error=batch add rejected" in output
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
    script = (
        "from pathlib import Path; "
        "import sys, time; "
        "Path(sys.argv[1]).write_text('ready', encoding='utf-8'); "
        "time.sleep(0.4)"
    )

    with sidechannel.AgentSideChannelServer(tmp_path):
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
        write_inbox_item(
            tmp_path,
            "20260101T000000000004Z.txt",
            compose_inbox_text(body="runner steering", priority=None, stop=False),
        )
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


def _context_meter(*, total_tokens: int, window: int) -> ContextMeter:
    snapshot = ActiveContextSnapshot(
        source_file="rollout.jsonl",
        ts="2026-01-01T00:00:00.000Z",
        input_tokens=total_tokens,
        cached_input_tokens=0,
        output_tokens=0,
        reasoning_output_tokens=0,
        total_tokens=total_tokens,
        model_context_window=window,
        cumulative_total_tokens=total_tokens,
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
