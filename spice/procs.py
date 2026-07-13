"""Process-group spawn, liveness, and termination across POSIX and Windows."""

from __future__ import annotations

import ctypes
from ctypes import wintypes
import os
import signal
import subprocess
import time
from typing import Any

from spice.errors import SpiceError

WINDOWS_CREATE_NEW_PROCESS_GROUP = 0x00000200
WINDOWS_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
WINDOWS_STILL_ACTIVE = 259
WINDOWS_ERROR_INVALID_PARAMETER = 87
PROCESS_POLL_INTERVAL_SECONDS = 0.1
# Liveness and forced-termination helpers shell out to `ps`/`taskkill`. A wedged
# invocation must not stall the supervisor's cleanup or liveness decisions, so
# every probe carries this named budget and degrades deterministically on expiry
# (liveness assumes the process is still alive; termination falls through to the
# caller's forceful escalation).
PROCESS_PROBE_TIMEOUT_SECONDS = 5.0


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
    timeout_seconds: float = 2.0,
) -> None:
    if process.poll() is not None:
        return
    if _is_windows():
        _terminate_windows_process_tree(process, timeout_seconds=timeout_seconds)
        return
    _terminate_posix_process_group(
        process,
        signum=signal.SIGTERM if signum is None else signum,
        timeout_seconds=timeout_seconds,
    )


def terminate_process_group_id(
    process_group_id: int,
    *,
    signum: int | None = None,
) -> None:
    if _is_windows():
        _force_windows_process_tree(process_group_id)
        return
    try:
        os.killpg(process_group_id, signal.SIGTERM if signum is None else signum)
    except ProcessLookupError:
        return


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
    try:
        os.killpg(process.pid, signum)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + timeout_seconds
    while process.poll() is None and time.monotonic() < deadline:
        time.sleep(PROCESS_POLL_INTERVAL_SECONDS)
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            return


def _terminate_windows_process_tree(
    process: subprocess.Popen[Any], *, timeout_seconds: float
) -> None:
    try:
        process.terminate()
        process.wait(timeout=timeout_seconds)
    except (OSError, subprocess.TimeoutExpired):
        pass
    if process.poll() is not None:
        return
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
    input: Any = None,
    check: bool = False,
) -> subprocess.CompletedProcess[Any]:
    """Capture a child under a deadline and terminate its whole group on expiry."""
    if timeout_seconds <= 0:
        raise ValueError("timeout_seconds must be positive")
    process = subprocess.Popen(
        command,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=text,
        env=env,
        stdin=subprocess.PIPE if input is not None else None,
        **popen_new_process_group_kwargs(),
    )
    try:
        stdout, stderr = process.communicate(input=input, timeout=timeout_seconds)
    except subprocess.TimeoutExpired as exc:
        terminate_process_group(
            process,
            timeout_seconds=PROCESS_GROUP_TERMINATION_GRACE_SECONDS,
        )
        try:
            process.communicate(timeout=PROCESS_GROUP_TERMINATION_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
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


def _is_windows() -> bool:
    return os.name == "nt"
