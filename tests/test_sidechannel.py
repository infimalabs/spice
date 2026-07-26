"""Agent side-channel payload, streaming, and watch-lifecycle contracts."""

from __future__ import annotations

import io
import os
import subprocess
import sys
import time
from threading import Event, Thread

import pytest

from spice.agent import driver as agent_driver
from spice.agent import runwatch, sidechannel, sidechannelnotify, wrap
from spice.mail.feedback import supervisor_feedback_line
from spice.mail.inbox import compose_inbox_text, write_inbox_item
from spice.mail import readout as inbox_readout
from tests.test_command import _working_state_event_counts

# Watcher shutdown crosses two thread handoffs (server handler close, client
# EOF); join returns the moment the thread exits, so this ceiling only bounds
# genuinely stuck threads while tolerating a saturated parallel-suite host.
SIDE_CHANNEL_SHUTDOWN_DEADLINE_S = 10.0


@pytest.fixture(autouse=True)
def _git_worktree_tmp_path(request, tmp_path):
    if "tmp_path" in request.fixturenames:
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)


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


def test_side_channel_payload_keeps_inbox_context_and_working_state_single_line(
    tmp_path, monkeypatch
):
    write_inbox_item(
        tmp_path,
        "1jN54zJR.txt",
        compose_inbox_text(body="payload steering", priority=None, stop=False),
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
        "1jN54zJS.txt",
        compose_inbox_text(body="post-tool steering", priority=None, stop=False),
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


def test_side_channel_repeats_critical_claim_warning_on_lease_timer(
    tmp_path, monkeypatch
):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(wrap, "CLAIM_LEASE_CRITICAL_REPEAT_SECONDS", 0.05)
    monkeypatch.setattr(
        wrap,
        "collect_working_state_snapshot",
        lambda _repo: wrap.WorkingStateSnapshot(
            claim_handle="CLAIMS-00000002",
            claim_phase="todo",
            claim_elapsed_seconds=90,
            claim_remaining_seconds=59,
        ),
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

        def two_warnings():
            output = stderr.getvalue()
            return output if output.count("CLAIM LEASE CRITICAL") >= 2 else ""

        output = _eventually(two_warnings, contains="CLAIM LEASE CRITICAL")

    thread.join(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
    assert output.count("CLAIM LEASE CRITICAL") >= 2
    assert "CLAIMS-00000002 has 59s remaining" in output
    assert "run spice task reclaim CLAIMS-00000002" in output
    assert not thread.is_alive()


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
            "1jN54zJM.txt",
            compose_inbox_text(body="late steering", priority=None, stop=False),
        )
        output = _eventually(lambda: stderr.getvalue(), contains="late steering")

    thread.join(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
    assert "Inbox Steering" in output
    # The late item's full readout streams exactly once; any later suppressed
    # inject surfaces only the one-line pending count, not a second full readout.
    assert output.count("late steering") == 1
    assert not thread.is_alive()


def test_side_channel_stream_hello_uses_inbox_signature_shape(tmp_path):
    signature = (("1jN54zJK.txt", 123, 45),)

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
        "initialInboxSignature": [["1jN54zJK.txt", 123, 45]],
    }


def test_side_channel_notice_queue_consumes_once(tmp_path):
    notice = supervisor_feedback_line("task.created", handles=["ACKS-1jN54zJK"])
    sidechannelnotify.publish_side_channel_feedback(
        tmp_path, "task.created", handles=["ACKS-1jN54zJK"]
    )

    first = sidechannelnotify.consume_side_channel_notices(tmp_path)
    second = sidechannelnotify.consume_side_channel_notices(tmp_path)

    assert first == [notice]
    assert second == []


def test_side_channel_claim_event_wakes_supervisor_callback(tmp_path):
    claim_changed = Event()
    observed: list[str] = []

    def on_claim() -> None:
        observed.append(sidechannelnotify.SIDE_CHANNEL_CLAIM_EVENT)
        claim_changed.set()

    with sidechannel.AgentSideChannelServer(tmp_path, on_claim=on_claim):
        sidechannelnotify.notify_agent_side_channel(
            tmp_path, event=sidechannelnotify.SIDE_CHANNEL_CLAIM_EVENT
        )
        assert claim_changed.wait(2.0)

    assert observed == [sidechannelnotify.SIDE_CHANNEL_CLAIM_EVENT]


def test_side_channel_watch_streams_queued_notice_after_initial_payload(
    tmp_path, monkeypatch
):
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    notice = supervisor_feedback_line("task.created", handles=["ACKS-1jN54zJL"])
    sidechannelnotify.publish_side_channel_feedback(
        tmp_path, "task.created", handles=["ACKS-1jN54zJL"]
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
        output = _eventually(lambda: stderr.getvalue(), contains="1jN54zJL")

    thread.join(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
    assert "Supervisor Feedback" in output
    assert notice in output
    assert output.count("1jN54zJL") == 1
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

    thread.join(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
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

    thread.join(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
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
        "1jN54zJN.txt",
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
        thread.join(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
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
            "1jN54zJN.txt",
            compose_inbox_text(body="runner steering", priority=None, stop=False),
        )
        allow_registration.set()
        output = _eventually(lambda: stderr.getvalue(), contains="runner steering")
        thread.join(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)

    assert results == [0]
    assert "Inbox Steering" in output
    assert output.count("Inbox Steering") == 1


def test_run_agent_command_does_not_duplicate_initial_side_channel_with_watch(
    tmp_path, monkeypatch
):
    monkeypatch.chdir(tmp_path)
    write_inbox_item(
        tmp_path,
        "1jN54zJP.txt",
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
        "1jN54zJP.txt",
        compose_inbox_text(body="initial snapshot steering", priority=None, stop=False),
    )
    initial_stat = initial.stat()
    late_name = "1jN54zJQ.txt"
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
        "1jN54zJQ.txt",
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


def test_side_channel_watch_exits_when_parent_already_exited_before_registration(
    tmp_path, monkeypatch
):
    watcher = wrap._parent_exit_watcher(os.getpid())
    if watcher is None:
        pytest.skip("platform does not expose process-exit watch handles")
    watcher.close()
    waitid_names = ("waitid", "P_PID", "WEXITED", "WNOWAIT")
    if not all(hasattr(os, name) for name in waitid_names):
        pytest.skip("platform cannot observe an exited child without reaping it")
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    parent = subprocess.Popen([sys.executable, "-c", ""])
    completed = Event()

    def watch_to_completion():
        wrap.watch_agent_side_channel(
            tmp_path,
            parent_pid=parent.pid,
            stderr=stderr,
        )
        completed.set()

    try:
        os.waitid(os.P_PID, parent.pid, os.WEXITED | os.WNOWAIT)
        with sidechannel.AgentSideChannelServer(tmp_path):
            thread = Thread(target=watch_to_completion)
            thread.start()
            assert completed.wait(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
            thread.join()
    finally:
        parent.wait(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)


def test_side_channel_watch_completes_when_parent_is_already_waitable(
    tmp_path, monkeypatch
):
    waitid_names = ("waitid", "P_PID", "WEXITED", "WNOWAIT")
    if not all(hasattr(os, name) for name in waitid_names):
        pytest.skip("platform does not expose non-reaping child status")
    watcher = wrap._parent_exit_watcher(os.getpid())
    if watcher is None:
        pytest.skip("platform does not expose process-exit watch handles")
    watcher.close()
    stderr = io.StringIO()
    monkeypatch.chdir(tmp_path)
    parent = subprocess.Popen([sys.executable, "-c", ""])
    completed = Event()

    try:
        status = os.waitid(os.P_PID, parent.pid, os.WEXITED | os.WNOWAIT)

        def watch_to_completion():
            wrap.watch_agent_side_channel(
                tmp_path,
                parent_pid=parent.pid,
                stderr=stderr,
            )
            completed.set()

        with sidechannel.AgentSideChannelServer(tmp_path):
            thread = Thread(target=watch_to_completion)
            thread.start()
            assert completed.wait(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)
            thread.join()

        assert status.si_pid == parent.pid
    finally:
        parent.wait(timeout=SIDE_CHANNEL_SHUTDOWN_DEADLINE_S)


def test_parent_exit_check_uses_one_nonreaping_observation_before_portable_probe(
    monkeypatch,
):
    waitid_names = ("waitid", "P_PID", "WEXITED", "WNOHANG", "WNOWAIT")
    if not all(hasattr(os, name) for name in waitid_names):
        pytest.skip("platform does not expose non-reaping child status")
    parent_pid = 424242
    observations: list[tuple[object, ...]] = []

    def observe_child(id_type, pid, options):
        observations.append(("waitid", id_type, pid, options))
        return None

    def probe_existence(pid, signal):
        observations.append(("kill", pid, signal))

    monkeypatch.setattr(os, "waitid", observe_child)
    monkeypatch.setattr(os, "kill", probe_existence)

    runwatch._process_has_exited(parent_pid)

    assert observations == [
        (
            "waitid",
            os.P_PID,
            parent_pid,
            os.WEXITED | os.WNOHANG | os.WNOWAIT,
        ),
        ("kill", parent_pid, 0),
    ]


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
