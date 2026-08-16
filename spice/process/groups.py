"""Process-group spawn, liveness, and termination across POSIX and Windows."""

from __future__ import annotations

from collections.abc import Callable
import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
from typing import Any

from spice.errors import SpiceError

WINDOWS_CREATE_NEW_PROCESS_GROUP = 0x00000200
WINDOWS_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
WINDOWS_STILL_ACTIVE = 259
WINDOWS_ERROR_INVALID_PARAMETER = 87
PROCESS_POLL_INTERVAL_SECONDS = 0.1
# How often a streamed child is checked for exit and for newly written output.
# This is the granularity of "still alive" reporting, so it is short enough that
# a caller's heartbeat lands on time and long enough that a quiet multi-minute
# suite costs a negligible number of wakeups.
STREAMED_OUTPUT_TICK_SECONDS = 0.25
# Liveness and forced-termination helpers shell out to `ps`/`taskkill`. A wedged
# invocation must not stall the supervisor's cleanup or liveness decisions, so
# every probe carries this named budget and degrades deterministically on expiry
# (liveness assumes the process is still alive; termination falls through to the
# caller's forceful escalation).
PROCESS_PROBE_TIMEOUT_SECONDS = 5.0
PROCESS_GROUP_TERMINATION_TIMEOUT_SECONDS = 2.0
PROCESS_GROUP_TERMINATION_MAX_PROBES = 2
GIT_OPTIONAL_LOCKS_ENV = "GIT_OPTIONAL_LOCKS"  # env-policy: allow
GIT_EXECUTABLE_NAMES = frozenset({"git", "git.exe"})
PROCESS_GROUP_TERMINATION_BOUND_SECONDS = (
    PROCESS_GROUP_TERMINATION_TIMEOUT_SECONDS
    + PROCESS_GROUP_TERMINATION_MAX_PROBES * PROCESS_PROBE_TIMEOUT_SECONDS
    + PROCESS_POLL_INTERVAL_SECONDS
)


def internal_process_environment(
    command: list[str], env: dict[str, str] | None
) -> dict[str, str] | None:
    """Return the child environment for one Python-managed process.

    Git's optional index refresh can replace an otherwise unchanged worktree
    index while a concurrent hook is validating its generation. Every internal
    Git child therefore opts out at the shared spawn boundary. Required Git
    mutations still take their mandatory locks. Non-Git and user-shell children
    never pass through this policy and retain their existing environment.
    """
    if not command or Path(command[0]).name.casefold() not in GIT_EXECUTABLE_NAMES:
        return env
    child_env = dict(os.environ if env is None else env)  # env-policy: allow
    child_env[GIT_OPTIONAL_LOCKS_ENV] = "0"
    return child_env


def popen_new_process_group_kwargs() -> dict[str, Any]:
    if _is_windows():
        return {
            "creationflags": getattr(
                subprocess,
                "CREATE_NEW_PROCESS_GROUP",
                WINDOWS_CREATE_NEW_PROCESS_GROUP,
            )
        }
    return {"start_new_session": True}


def terminate_process_group(
    process: subprocess.Popen[Any],
    *,
    signum: int | None = None,
    timeout_seconds: float = PROCESS_GROUP_TERMINATION_TIMEOUT_SECONDS,
) -> None:
    # The leader may exit while descendants keep inherited pipes open, so
    # cleanup always targets the complete group/tree regardless of leader state.
    if _is_windows():
        _terminate_windows_process_tree(process, timeout_seconds=timeout_seconds)
        return
    _terminate_posix_process_group(
        process,
        signum=signal.SIGTERM if signum is None else signum,
        timeout_seconds=timeout_seconds,
    )


def process_group_is_running(process_group_id: int | None) -> bool:
    if process_group_id is None:
        return False
    if _is_windows():
        return _windows_pid_is_running(process_group_id)
    try:
        os.kill(-process_group_id, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return _posix_process_group_has_live_member(process_group_id)
    return _posix_process_group_has_live_member(process_group_id)


def process_id_is_running(pid: int | None) -> bool:
    if pid is None:
        return False
    if _is_windows():
        return _windows_pid_is_running(pid)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return _posix_pid_has_live_state(pid)
    return _posix_pid_has_live_state(pid)


def _terminate_posix_process_group(
    process: subprocess.Popen[Any], *, signum: int, timeout_seconds: float
) -> None:
    process_group_id = process.pid
    try:
        os.killpg(process_group_id, signum)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + timeout_seconds
    while process_group_is_running(process_group_id) and time.monotonic() < deadline:
        time.sleep(PROCESS_POLL_INTERVAL_SECONDS)
    if process_group_is_running(process_group_id):
        try:
            os.killpg(process_group_id, signal.SIGKILL)
        except ProcessLookupError:
            return


def _terminate_windows_process_tree(
    process: subprocess.Popen[Any], *, timeout_seconds: float
) -> None:
    _force_windows_process_tree(process.pid)
    try:
        process.wait(timeout=timeout_seconds)
    except (OSError, subprocess.TimeoutExpired):
        if process.poll() is None:
            process.kill()


def _force_windows_process_tree(pid: int) -> None:
    try:
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/T", "/F"],
            check=False,
            capture_output=True,
            text=True,
            timeout=PROCESS_PROBE_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired):
        pass


def _posix_process_group_has_live_member(process_group_id: int) -> bool:
    try:
        completed = subprocess.run(
            ["ps", "-o", "stat=", "-g", str(process_group_id)],
            check=False,
            capture_output=True,
            text=True,
            timeout=PROCESS_PROBE_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired):
        return True
    if completed.returncode != 0:
        return True
    states = [line.strip() for line in completed.stdout.splitlines() if line.strip()]
    if not states:
        return False
    return any(not state.startswith("Z") for state in states)


def _posix_pid_has_live_state(pid: int) -> bool:
    try:
        completed = subprocess.run(
            ["ps", "-o", "stat=", "-p", str(pid)],
            check=False,
            capture_output=True,
            text=True,
            timeout=PROCESS_PROBE_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired):
        return True
    if completed.returncode != 0:
        return True
    states = [line.strip() for line in completed.stdout.splitlines() if line.strip()]
    if not states:
        return False
    return any(not state.startswith("Z") for state in states)


def _windows_pid_is_running(pid: int) -> bool:
    kernel32 = getattr(ctypes, "windll").kernel32
    kernel32.OpenProcess.argtypes = [
        wintypes.DWORD,
        wintypes.BOOL,
        wintypes.DWORD,
    ]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.GetExitCodeProcess.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(wintypes.DWORD),
    ]
    kernel32.GetExitCodeProcess.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    handle = kernel32.OpenProcess(
        WINDOWS_PROCESS_QUERY_LIMITED_INFORMATION,
        False,
        pid,
    )
    if not handle:
        return kernel32.GetLastError() != WINDOWS_ERROR_INVALID_PARAMETER
    exit_code = wintypes.DWORD()
    try:
        if not kernel32.GetExitCodeProcess(handle, ctypes.byref(exit_code)):
            return True
        return exit_code.value == WINDOWS_STILL_ACTIVE
    finally:
        kernel32.CloseHandle(handle)


PROCESS_GROUP_TERMINATION_GRACE_SECONDS = 0.25


class ProcessDeadlineExceeded(SpiceError):
    """A named external provider exhausted its process-group deadline."""

    def __init__(
        self,
        *,
        phase: str,
        input_label: str,
        timeout_seconds: float,
        command: list[str],
    ) -> None:
        self.phase = phase
        self.input_label = input_label
        self.timeout_seconds = timeout_seconds
        self.command = tuple(command)
        super().__init__(
            f"process deadline exceeded phase={phase} input={input_label} "
            f"budget={timeout_seconds:g}s command={' '.join(command)}"
        )


def run_bounded_process_group(
    command: list[str],
    *,
    timeout_seconds: float,
    phase: str,
    input_label: str,
    cwd: Any = None,
    text: bool = False,
    env: dict[str, str] | None = None,
    input_data: Any = None,
    capture_output: bool = True,
    check: bool = False,
) -> subprocess.CompletedProcess[Any]:
    """Run a child under a deadline and terminate its whole group on expiry."""
    if timeout_seconds <= 0:
        raise ValueError("timeout_seconds must be positive")
    process = subprocess.Popen(
        command,
        cwd=cwd,
        stdout=subprocess.PIPE if capture_output else None,
        stderr=subprocess.PIPE if capture_output else None,
        text=text,
        env=internal_process_environment(command, env),
        stdin=subprocess.PIPE if input_data is not None else None,
        **popen_new_process_group_kwargs(),
    )
    try:
        stdout, stderr = process.communicate(input=input_data, timeout=timeout_seconds)
    except subprocess.TimeoutExpired as exc:
        _reap_expired_process_group(process)
        raise ProcessDeadlineExceeded(
            phase=phase,
            input_label=input_label,
            timeout_seconds=timeout_seconds,
            command=command,
        ) from exc
    completed = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    if check and completed.returncode != 0:
        raise subprocess.CalledProcessError(
            completed.returncode,
            command,
            output=stdout,
            stderr=stderr,
        )
    return completed


def _reap_expired_process_group(process: subprocess.Popen[Any]) -> None:
    """Terminate the whole group and cap the final reap before diagnosing."""
    terminate_process_group(
        process,
        timeout_seconds=PROCESS_GROUP_TERMINATION_GRACE_SECONDS,
    )
    try:
        process.communicate(timeout=PROCESS_GROUP_TERMINATION_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.communicate()


def run_streamed_process_group(
    command: list[str],
    *,
    timeout_seconds: float,
    phase: str,
    input_label: str,
    on_progress: Callable[[str, float], None],
    cwd: Any = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a child under a deadline, reporting output while it still runs.

    The child writes into a temporary file rather than a pipe, and the parent
    tails that file through its own handle. Two handles onto one path is what
    makes this work: they are separate file descriptions, so the parent's reads
    never move the offset the child appends at, and no reader thread is needed
    to keep a pipe from filling. `on_progress` is called on a fixed tick with
    whatever arrived since the last one -- the empty string when nothing did --
    so a caller can both forward output and notice silence.

    Streams merge: the child's stderr is redirected into the same sink so the
    order the caller sees is the order the child wrote, which a two-pipe capture
    cannot reconstruct after the fact.
    """
    if timeout_seconds <= 0:
        raise ValueError("timeout_seconds must be positive")
    handle, raw_sink = tempfile.mkstemp(prefix="spice-streamed-")
    os.close(handle)
    sink_path = Path(raw_sink)
    try:
        return _stream_until_exit(
            command,
            sink_path,
            timeout_seconds=timeout_seconds,
            phase=phase,
            input_label=input_label,
            on_progress=on_progress,
            cwd=cwd,
            env=env,
        )
    finally:
        sink_path.unlink(missing_ok=True)


def _stream_until_exit(
    command: list[str],
    sink_path: Path,
    *,
    timeout_seconds: float,
    phase: str,
    input_label: str,
    on_progress: Callable[[str, float], None],
    cwd: Any,
    env: dict[str, str] | None,
) -> subprocess.CompletedProcess[str]:
    started = time.monotonic()
    with (
        open(sink_path, "w", encoding="utf-8") as sink,
        open(sink_path, encoding="utf-8", errors="replace") as tail,
    ):
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdout=sink,
            stderr=subprocess.STDOUT,
            text=True,
            env=internal_process_environment(command, env),
            **popen_new_process_group_kwargs(),
        )
        try:
            output = _tail_process_output(
                process,
                tail,
                on_progress,
                started=started,
                timeout_seconds=timeout_seconds,
            )
        except BaseException:
            _reap_expired_process_group(process)
            raise
        if output is None:
            _reap_expired_process_group(process)
            raise ProcessDeadlineExceeded(
                phase=phase,
                input_label=input_label,
                timeout_seconds=timeout_seconds,
                command=command,
            )
    return subprocess.CompletedProcess(command, process.returncode, output, "")


def _tail_process_output(
    process: subprocess.Popen[Any],
    tail: Any,
    on_progress: Callable[[str, float], None],
    *,
    started: float,
    timeout_seconds: float,
) -> str | None:
    """Return the child's complete output, or None once its deadline expires."""
    collected: list[str] = []
    while True:
        running = _child_outlives_one_tick(process)
        chunk = tail.read()
        collected.append(chunk)
        elapsed = time.monotonic() - started
        on_progress(chunk, elapsed)
        if not running:
            return "".join(collected)
        if elapsed >= timeout_seconds:
            return None


def _child_outlives_one_tick(process: subprocess.Popen[Any]) -> bool:
    """Return whether the child is still running after one output tick."""
    try:
        process.wait(timeout=STREAMED_OUTPUT_TICK_SECONDS)
    except subprocess.TimeoutExpired:
        return True
    return False


def _is_windows() -> bool:
    return os.name == "nt"
