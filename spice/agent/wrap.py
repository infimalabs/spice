"""`spice agent run` — the command surface every agent shell command enters.

Shell startup hooks reexec zsh/bash commands through
`spice agent run -- <cmd…>`. The command surface:

* runs routed commands in the worktree env, which inherits the per-process git
  shadow the supervisor exports once (so the agent's git upstream reads as its
  own branch); the control plane reads the real integration branch via
  `git config --get`;
* connects to the supervisor side-channel socket (when one is live) and
  relays its payload to stderr;
* injects pending inbox steering into stderr, re-displaying every 15s until ACK;
* injects keep-working guidance derived from the agent's own transcript,
  repeated every 15 minutes and persisted across wrapper processes.

The agent's terminal is therefore a duplex steering surface: every command it
runs is an opportunity for the operator to be heard.
"""

from __future__ import annotations

import datetime
import json
import math
import os
import shlex
import socket as socket
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TextIO

from spice.config import values
from spice.agent.driver import driver_for
from spice.agent.runinbox import (
    AGENT_RUN_INBOX_REPEAT_SECONDS as AGENT_RUN_INBOX_REPEAT_SECONDS,
    AgentInboxInjector as AgentInboxInjector,
    AgentSideChannelNoticeInjector as AgentSideChannelNoticeInjector,
    InboxSignature as InboxSignature,
    inbox_pending_signature,
    inbox_signature_from_payload as inbox_signature_from_payload,
    post_tool_hook_inbox_state_path as post_tool_hook_inbox_state_path,
)
from spice.agent.runwatch import (
    AGENT_RUN_SIDE_CHANNEL_CONNECT_TIMEOUT_S as AGENT_RUN_SIDE_CHANNEL_CONNECT_TIMEOUT_S,
    AGENT_RUN_SIDE_CHANNEL_READ_BYTES as AGENT_RUN_SIDE_CHANNEL_READ_BYTES,
    _parent_exit_watcher as _parent_exit_watcher,
    agent_side_channel_hello as agent_side_channel_hello,
    join_agent_side_channel_watch,
    start_agent_side_channel_watch,
    watch_agent_side_channel as watch_agent_side_channel,
    write_side_channel_chunk as write_side_channel_chunk,
)
from spice.paths import atomic_write_json
from spice.agent.sidechannelnotify import (
    side_channel_marker_path as side_channel_marker_path,
)
from spice.agent.identity import ambient_thread_id
from spice.agent.paths import agent_state_dir, agent_thread_state_dir
from spice.agent.rtkrewrite import (
    RTK_CANONICAL_EXECUTABLE as RTK_CANONICAL_EXECUTABLE,
    RTK_REWRITE_MATCH_EXIT_CODE as RTK_REWRITE_MATCH_EXIT_CODE,
    RTK_REWRITE_NO_MATCH_EXIT_CODE as RTK_REWRITE_NO_MATCH_EXIT_CODE,
    _rtk_warned_keys as _rtk_warned_keys,
    emit_rewrite_diagnostic as _emit_rtk_rewrite_diagnostic,
    remap_rewrite_frontend,
    rewrite_command_text as _rewrite_rtk_command_text,
)
from spice.agent.shellhook import (
    BASH_ENV_ENV,
    BASH_HOOK_NAME,
    PROJECT_PYTHON_COMMANDS,
    UV_PYTHON_COMMAND,
    ZDOTDIR_ENV,
    packaged_shell_steering_static_hook_dir,
    project_routes_python,
    rtk_rewrite_yield_selectors,
)
from spice.errors import SpiceError

SHELL_EXECUTION_COMMANDS = frozenset(("bash", "dash", "sh", "zsh"))
SHELL_EXECUTION_FLAGS = frozenset(("-c", "-lc"))
RTK_DB_PATH_ENV = "RTK_DB_PATH"  # env-policy: allow

# The working-state banner is normally a change notification, not a periodic
# meter. Claim-expiry warnings are the exception: a quiet, long-running command
# still needs deadline-driven reminders before its lease can lapse.
AGENT_RUN_WORKING_STATE_REPEAT_SECONDS = math.inf
CLAIM_LEASE_WARNING_SECONDS = 10 * 60
CLAIM_LEASE_URGENT_SECONDS = 5 * 60
CLAIM_LEASE_CRITICAL_SECONDS = 60
CLAIM_LEASE_WARNING_REPEAT_SECONDS = 2 * 60.0
CLAIM_LEASE_URGENT_REPEAT_SECONDS = 60.0
CLAIM_LEASE_CRITICAL_REPEAT_SECONDS = 15.0
CLAIM_LEASE_TIMER_MIN_SECONDS = 0.05
AGENT_RUN_CONTEXT_METER_CACHE_SECONDS = 15.0
AGENT_RUN_CONTEXT_WARNING_REPEAT_SECONDS = 15.0 * 60.0
# The watcher's connect+hello to the supervisor socket carries this budget so a
# wedged or half-open socket cannot park the watch thread at startup. Once the
# hello is sent, the socket resets to blocking: the established stream is bound to
# parent exit, peer close, or server wake/stop, not this connect deadline.
INTERRUPTED_EXIT_CODE = 130
COMMAND_NOT_FOUND_EXIT_CODE = 127

ContextWarningSignature = tuple[str, str, int]
ContextWarningKey = tuple[str]
WorkingStateKey = tuple[int, str, str, str, str]


@dataclass(frozen=True)
class WorkingStateSnapshot:
    pending_inbox_count: int = 0
    claim_handle: str = ""
    claim_phase: str = ""
    claim_elapsed_seconds: int | None = None
    claim_remaining_seconds: int | None = None
    last_maxim_bag: str = ""

    def has_fields(self) -> bool:
        return bool(
            self.pending_inbox_count or self.claim_handle or self.last_maxim_bag
        )


ProcessFactory = Callable[..., Any]
TimeFactory = Callable[[], float]


def working_state_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "working-state.json"


def run_agent_command(
    repo_root: Path | None,
    raw_args: Sequence[str],
    *,
    popen_factory: ProcessFactory = subprocess.Popen,
    stderr: TextIO = sys.stderr,
) -> int:
    bind_ambient_thread_for_shell_stage(repo_root, stderr=stderr)
    initial_inbox_signature = emit_initial_side_channel_payload(
        repo_root, stderr=stderr
    )
    environment = build_agent_run_environment(
        raw_args,
        repo_root=repo_root,
    )
    command = build_agent_run_command(
        raw_args,
        repo_root=repo_root,
        rewrite_rtk=True,
        rtk_environment=environment,
        rtk_stderr=stderr,
    )
    try:
        if environment is None:
            process = popen_factory(command)
        else:
            process = popen_factory(command, env=environment)
    except FileNotFoundError:
        executable = command[0] if command else ""
        stderr.write(f"spice agent run: command not found: {executable}\n")
        stderr.flush()
        return COMMAND_NOT_FOUND_EXIT_CODE
    watch_thread = start_agent_side_channel_watch(
        repo_root,
        parent_pid=int(getattr(process, "pid", 0) or 0),
        stderr=stderr,
        initial_inbox_signature=initial_inbox_signature,
    )
    try:
        wait = getattr(process, "wait", None)
        if wait is None:
            returncode = process.poll()
        else:
            returncode = wait()
        return int(returncode if returncode is not None else INTERRUPTED_EXIT_CODE)
    finally:
        join_agent_side_channel_watch(watch_thread)


def bind_ambient_thread_for_shell_stage(
    repo_root: Path | None, *, stderr: TextIO = sys.stderr
) -> None:
    """Record the worktree's driver thread id as the agent's shell passes through.

    This is what lets a lane be discovered by session id without the agent ever
    running `spice agent activation`: every command it issues arrives here, and
    the first one binds the worktree.

    Degradation matches `emit_initial_side_channel_payload` below. The shell
    stage sits in front of every command an agent runs, so unusable spice state
    has to surface as a warning rather than take the agent's shell down.
    """
    if repo_root is None:
        return
    from spice.agent.lifecyclebinding import bind_ambient_agent_thread

    try:
        bind_ambient_agent_thread(repo_root)
    except Exception as exc:  # thread binding failure is non-fatal
        stderr.write(f"spice thread binding unavailable: {exc}\n")
        stderr.flush()


def emit_initial_side_channel_payload(
    repo_root: Path | None, *, stderr: TextIO = sys.stderr
) -> InboxSignature:
    if repo_root is None:
        return ()
    from spice.agent.sidechannel import render_side_channel_payload

    try:
        payload, initial_inbox_signature = render_side_channel_payload(repo_root)
    except Exception as exc:  # side-channel render failure is non-fatal
        stderr.write(f"spice side-channel unavailable: {exc}\n")
        stderr.flush()
        return ()
    if payload:
        stderr.write(payload)
        stderr.flush()
    return initial_inbox_signature


def build_agent_run_command(
    raw_args: Sequence[str],
    *,
    repo_root: Path | None = None,
    rewrite_rtk: bool = False,
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> list[str]:
    args = normalize_agent_run_args(raw_args)
    rtk_executable = (
        values.configured_rtk_executable(repo_root)
        if rewrite_rtk
        else RTK_CANONICAL_EXECUTABLE
    )
    if rewrite_rtk:
        args = rtk_rewrite_agent_run_args(
            args,
            repo_root=repo_root,
            rtk_executable=rtk_executable,
            rtk_environment=rtk_environment,
            rtk_stderr=rtk_stderr,
        )
    if rewrite_rtk:
        args = (
            rtk_rewrite_direct_args(
                args,
                repo_root=repo_root,
                rtk_executable=rtk_executable,
                rtk_environment=rtk_environment,
                rtk_stderr=rtk_stderr,
            )
            or args
        )
    return worktree_route_command(args, repo_root=repo_root)


def rtk_rewrite_agent_run_args(
    args: Sequence[str],
    *,
    repo_root: Path | None = None,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> list[str]:
    shell_command_index = shell_execution_command_index(args)
    if shell_command_index is None:
        return list(args)
    rewritten = rtk_rewrite_shell_execution_text(
        args[shell_command_index],
        repo_root=repo_root,
        rtk_executable=rtk_executable,
        rtk_environment=rtk_environment,
        rtk_stderr=rtk_stderr,
    )
    if rewritten is None:
        return list(args)
    result = list(args)
    result[shell_command_index] = rewritten
    return result


def rtk_rewrite_shell_execution_text(
    command_text: str,
    *,
    repo_root: Path | None = None,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> str | None:
    rewritten = _rtk_rewrite_frontend_with_environment(
        command_text,
        repo_root=repo_root,
        rtk_executable=rtk_executable,
        rtk_environment=rtk_environment,
        rtk_stderr=rtk_stderr,
    )
    if rewritten is not None:
        return rewritten
    trailing = rtk_rewrite_trailing_exec_shell_command(
        command_text,
        repo_root=repo_root,
        rtk_executable=rtk_executable,
        rtk_environment=rtk_environment,
        rtk_stderr=rtk_stderr,
    )
    if trailing is not None:
        return trailing
    return driver_for(repo_root).rewrite_tool_command(
        command_text,
        lambda *args: _rtk_rewrite_frontend_with_environment(
            *args,
            repo_root=repo_root,
            rtk_executable=rtk_executable,
            rtk_environment=rtk_environment,
            rtk_stderr=rtk_stderr,
        ),
    )


def rtk_rewrite_trailing_exec_shell_command(
    command_text: str,
    *,
    repo_root: Path | None = None,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> str | None:
    stripped = command_text.rstrip()
    trailing = command_text[len(stripped) :]
    line_start = stripped.rfind("\n") + 1
    prefix = stripped[:line_start]
    line = stripped[line_start:]
    try:
        parts = shlex.split(line)
    except ValueError:
        return None
    if len(parts) != 4 or parts[0] != "exec":
        return None
    shell, flag, nested_command = parts[1:]
    if (
        Path(shell).name not in SHELL_EXECUTION_COMMANDS
        or flag not in SHELL_EXECUTION_FLAGS
    ):
        return None
    rewritten = _rtk_rewrite_frontend_with_environment(
        nested_command,
        repo_root=repo_root,
        rtk_executable=rtk_executable,
        rtk_environment=rtk_environment,
        rtk_stderr=rtk_stderr,
    )
    if rewritten is None:
        return None
    return (
        f"{prefix}exec {shlex.quote(shell)} {flag} {shlex.quote(rewritten)}{trailing}"
    )


def rtk_rewrite_direct_args(
    args: Sequence[str],
    *,
    repo_root: Path | None = None,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> list[str] | None:
    if (
        not args
        or args[0] in {RTK_CANONICAL_EXECUTABLE, rtk_executable}
        or shell_execution_command_index(args) is not None
    ):
        return None
    rewritten = _rtk_rewrite_frontend_with_environment(
        *args,
        repo_root=repo_root,
        rtk_executable=rtk_executable,
        rtk_environment=rtk_environment,
        rtk_stderr=rtk_stderr,
    )
    if rewritten is None:
        return None
    try:
        parsed = shlex.split(rewritten)
    except ValueError as exc:
        _emit_rtk_rewrite_diagnostic(
            repo_root,
            rtk_stderr,
            executable=rtk_executable,
            failure_class="malformed-direct-argv",
            failure_signature=f"parse={type(exc).__name__}",
        )
        return None
    if parsed:
        return parsed
    _emit_rtk_rewrite_diagnostic(
        repo_root,
        rtk_stderr,
        executable=rtk_executable,
        failure_class="malformed-direct-argv",
        failure_signature="parse=empty-argv",
    )
    return None


def _rtk_rewrite_frontend_with_environment(
    *args: str,
    repo_root: Path | None,
    rtk_executable: str,
    rtk_environment: Mapping[str, str] | None,
    rtk_stderr: TextIO | None,
) -> str | None:
    rewritten = rtk_rewrite_command_text(
        *args,
        repo_root=repo_root,
        rtk_executable=rtk_executable,
        env=rtk_environment,
        stderr=rtk_stderr,
    )
    if rewritten is None:
        return None
    remapped = remap_rewrite_frontend(rewritten, rtk_executable)
    if _rewrite_shadows_selected_wrapper(
        remapped, repo_root=repo_root, rtk_executable=rtk_executable
    ):
        return None
    return remapped


def _rewrite_shadows_selected_wrapper(
    command_text: str, *, repo_root: Path | None, rtk_executable: str
) -> bool:
    """True when RTK claimed a word a selected shell wrapper expands elsewhere."""
    if repo_root is None:
        return False
    try:
        words = shlex.split(command_text)
    except ValueError:
        return False
    if len(words) < 2 or words[0] not in {RTK_CANONICAL_EXECUTABLE, rtk_executable}:
        return False
    return words[1] in rtk_rewrite_yield_selectors(repo_root)


def rtk_rewrite_command_text(
    *args: str,
    repo_root: Path | None = None,
    rtk_executable: str = RTK_CANONICAL_EXECUTABLE,
    env: Mapping[str, str] | None = None,
    stderr: TextIO | None = None,
    run: Callable[..., subprocess.CompletedProcess[str]] | None = None,
) -> str | None:
    return _rewrite_rtk_command_text(
        *args,
        repo_root=repo_root,
        rtk_executable=rtk_executable,
        env=env,
        stderr=stderr,
        run=run,
    )


def shell_execution_command_index(args: Sequence[str]) -> int | None:
    if len(args) < 3 or args[1] not in SHELL_EXECUTION_FLAGS:
        return None
    if Path(args[0]).name not in SHELL_EXECUTION_COMMANDS:
        return None
    return 2


def build_agent_run_environment(
    raw_args: Sequence[str],
    *,
    repo_root: Path | None = None,
) -> dict[str, str] | None:
    args = normalize_agent_run_args(raw_args)
    # The git shadow is exported once by the supervisor (lifecycle.agent_env) and
    # inherited by direct git commands when Popen gets no explicit env. git sees
    # the shadow (upstream=self); the control plane reads the real integration
    # branch via `git config --get`, where the command-scope true merge wins over
    # the system-scope self merge.
    env = None
    if shell_execution_command_index(args) is not None:
        env = agent_run_child_worktree_environment(args, repo_root=repo_root)
    return apply_scoped_rtk_history_environment(repo_root, env)


def apply_scoped_rtk_history_environment(
    repo_root: Path | None, env: dict[str, str] | None
) -> dict[str, str] | None:
    path = scoped_rtk_history_db_path(repo_root)
    if path is None:
        return env
    path.parent.mkdir(parents=True, exist_ok=True)
    result = dict(os.environ if env is None else env)  # env-policy: allow
    result[RTK_DB_PATH_ENV] = str(path)
    return result


def scoped_rtk_history_db_path(repo_root: Path | None) -> Path | None:
    if repo_root is None:
        return None
    thread_id = ambient_thread_id()
    if not thread_id:
        return None
    try:
        return agent_thread_state_dir(repo_root, thread_id) / "rtk" / "history.db"
    except SpiceError:
        return None


def agent_run_child_worktree_environment(
    args: Sequence[str],
    *,
    repo_root: Path | None,
    base_env: Mapping[str, str] | None = None,
) -> dict[str, str]:
    env = dict(os.environ if base_env is None else base_env)  # env-policy: allow
    if shell_execution_command_index(args) is not None:
        static_hook_dir = packaged_shell_steering_static_hook_dir()
        env[ZDOTDIR_ENV] = str(static_hook_dir)
        env[BASH_ENV_ENV] = str(static_hook_dir / BASH_HOOK_NAME)
    return env


def worktree_route_command(
    args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    return worktree_python_route_command(args, repo_root=repo_root)


def worktree_python_route_command(
    args: Sequence[str], *, repo_root: Path | None = None
) -> list[str]:
    if (
        args[:1]
        and args[0] in PROJECT_PYTHON_COMMANDS
        and project_routes_python(repo_root)
    ):
        return [*UV_PYTHON_COMMAND, *args[1:]]
    return list(args)


def normalize_agent_run_args(raw_args: Sequence[str]) -> list[str]:
    args = list(raw_args)
    if args[:1] == ["--"]:
        return args[1:]
    return args


class AgentWorkingStateInjector:
    """Collect live working state for the one-line stderr meter."""

    def __init__(
        self,
        repo_root: Path | None,
        *,
        stderr: TextIO,
        repeat_interval_seconds: float = AGENT_RUN_WORKING_STATE_REPEAT_SECONDS,
        time_factory: TimeFactory = time.monotonic,
        snapshot_factory: Callable[[Path | None], WorkingStateSnapshot] | None = None,
    ) -> None:
        self.repo_root = repo_root
        self.stderr = stderr
        self.repeat_interval_seconds = max(0.0, repeat_interval_seconds)
        self.time_factory = time_factory
        self.snapshot_factory = snapshot_factory or collect_working_state_snapshot
        self.displayed_at: float | None = None
        self.displayed_key: WorkingStateKey | None = None
        self.latest_snapshot = WorkingStateSnapshot()
        self.latest_snapshot_at: float | None = None

    def inject(self, *, force: bool) -> None:
        del force
        try:
            snapshot = self.snapshot_factory(self.repo_root)
        except Exception:
            return
        now = self.time_factory()
        self.latest_snapshot = snapshot
        self.latest_snapshot_at = now
        if not snapshot.has_fields():
            return
        key = working_state_key(snapshot)
        repeat_seconds = _working_state_repeat_seconds(
            snapshot, ceiling=self.repeat_interval_seconds
        )
        if self._should_suppress(key, now=now, repeat_seconds=repeat_seconds):
            return
        text = render_working_state_snapshot(snapshot)
        if not text:
            return
        self.stderr.write(text)
        self.stderr.write("\n")
        self.stderr.flush()
        self._record_displayed(key, now=now)

    def seconds_until_refresh(self) -> float | None:
        """Return the next claim-warning deadline for a streaming command."""
        snapshot_at = self.latest_snapshot_at
        remaining = self.latest_snapshot.claim_remaining_seconds
        if (
            snapshot_at is None
            or remaining is None
            or not self.latest_snapshot.claim_handle
        ):
            return None
        now = self.time_factory()
        estimated = max(0.0, float(remaining) - max(0.0, now - snapshot_at))
        level = _claim_lease_warning_level(int(estimated))
        displayed_level = self.displayed_key[-1] if self.displayed_key else ""
        if level != displayed_level:
            return CLAIM_LEASE_TIMER_MIN_SECONDS
        deadlines = [_seconds_until_claim_lease_boundary(estimated, level=level)]
        repeat_seconds = _claim_lease_warning_repeat_seconds(level)
        if math.isfinite(repeat_seconds):
            if self.displayed_at is None:
                deadlines.append(0.0)
            else:
                deadlines.append(repeat_seconds - max(0.0, now - self.displayed_at))
        finite = [delay for delay in deadlines if math.isfinite(delay)]
        if not finite:
            return None
        return max(CLAIM_LEASE_TIMER_MIN_SECONDS, min(finite))

    def _should_suppress(
        self, key: WorkingStateKey, *, now: float, repeat_seconds: float
    ) -> bool:
        if self._is_recent_match(
            self.displayed_key,
            self.displayed_at,
            key,
            now=now,
            repeat_seconds=repeat_seconds,
        ):
            return True
        stored_key, stored_at = read_working_state_state(self.repo_root)
        if self._is_recent_match(
            stored_key,
            stored_at,
            key,
            now=now,
            repeat_seconds=repeat_seconds,
        ):
            self.displayed_key = stored_key
            self.displayed_at = stored_at
            return True
        return False

    def _record_displayed(self, key: WorkingStateKey, *, now: float) -> None:
        self.displayed_key = key
        self.displayed_at = now
        write_working_state_state(self.repo_root, key, now=now)

    def _is_recent_match(
        self,
        displayed_key: WorkingStateKey | None,
        displayed_at: float | None,
        key: WorkingStateKey,
        *,
        now: float,
        repeat_seconds: float,
    ) -> bool:
        if displayed_key != key or displayed_at is None:
            return False
        age = now - displayed_at
        return 0 <= age < repeat_seconds


def collect_working_state_snapshot(
    repo_root: Path | None, *, now: float | None = None
) -> WorkingStateSnapshot:
    if repo_root is None:
        return WorkingStateSnapshot()
    root = Path(repo_root)
    (
        claim_handle,
        claim_phase,
        claim_elapsed_seconds,
        claim_remaining_seconds,
    ) = _working_state_claim(root, now=now)
    return WorkingStateSnapshot(
        pending_inbox_count=_working_state_pending_count(root),
        claim_handle=claim_handle,
        claim_phase=claim_phase,
        claim_elapsed_seconds=claim_elapsed_seconds,
        claim_remaining_seconds=claim_remaining_seconds,
        last_maxim_bag=_working_state_last_maxim_bag(root),
    )


def _working_state_pending_count(repo_root: Path) -> int:
    return len(inbox_pending_signature(repo_root))


def render_working_state_snapshot(snapshot: WorkingStateSnapshot) -> str:
    if not snapshot.has_fields():
        return ""
    parts: list[str] = []
    if snapshot.pending_inbox_count:
        inbox_label = (
            "pending inbox" if snapshot.pending_inbox_count == 1 else "pending inboxes"
        )
        parts.append(f"{snapshot.pending_inbox_count} {inbox_label}")
    if snapshot.claim_handle:
        claim = f"claim {_working_state_clean_text(snapshot.claim_handle)}"
        if snapshot.claim_phase:
            claim += f" {_working_state_clean_text(snapshot.claim_phase)}"
        if snapshot.claim_elapsed_seconds is not None:
            claim += f" for {_working_state_duration(snapshot.claim_elapsed_seconds)}"
        parts.append(claim)
        warning_level = _claim_lease_warning_level(snapshot.claim_remaining_seconds)
        if warning_level:
            handle = _working_state_clean_text(snapshot.claim_handle)
            remaining = _working_state_duration(snapshot.claim_remaining_seconds or 0)
            parts.append(
                f"CLAIM LEASE {warning_level.upper()}: {handle} has {remaining} "
                f"remaining; run spice task reclaim {handle}"
            )
    if snapshot.last_maxim_bag:
        parts.append(f"last maxim {_working_state_clean_text(snapshot.last_maxim_bag)}")
    if not parts:
        return ""
    return f"🌶️ Working state: {'; '.join(parts)}."


def working_state_key(snapshot: WorkingStateSnapshot) -> WorkingStateKey:
    return (
        max(0, int(snapshot.pending_inbox_count)),
        _working_state_clean_text(snapshot.claim_handle),
        _working_state_clean_text(snapshot.claim_phase),
        _working_state_clean_text(snapshot.last_maxim_bag),
        _claim_lease_warning_level(snapshot.claim_remaining_seconds),
    )


def read_working_state_state(
    repo_root: Path | None,
) -> tuple[WorkingStateKey | None, float | None]:
    if repo_root is None:
        return None, None
    payload = read_context_meter_cache_payload(working_state_state_path(repo_root))
    key = _working_state_key_payload(payload.get("key"))
    displayed_at = _float_payload_value(payload.get("displayedAt"))
    if key is None or displayed_at is None:
        return None, None
    return key, displayed_at


def write_working_state_state(
    repo_root: Path | None, key: WorkingStateKey, *, now: float
) -> None:
    if repo_root is None:
        return
    path = working_state_state_path(repo_root)
    atomic_write_json(
        path,
        {"displayedAt": now, "key": list(key)},
        compact=True,
    )


def _working_state_key_payload(value: Any) -> WorkingStateKey | None:
    if not isinstance(value, list) or len(value) not in (4, 5):
        return None
    pending = _int_payload_value(value[0])
    claim_handle = value[1]
    claim_phase = value[2]
    last_maxim = value[3]
    warning_level = value[4] if len(value) == 5 else ""
    if (
        pending is None
        or not isinstance(claim_handle, str)
        or not isinstance(claim_phase, str)
        or not isinstance(last_maxim, str)
        or not isinstance(warning_level, str)
    ):
        return None
    return (pending, claim_handle, claim_phase, last_maxim, warning_level)


def _working_state_duration(seconds: int) -> str:
    value = max(0, int(seconds))
    return f"{value}s"


def _working_state_clean_text(value: object) -> str:
    return " ".join(str(value or "").split())


def _working_state_claim(
    repo_root: Path, *, now: float | None
) -> tuple[str, str, int | None, int | None]:
    actor = ambient_thread_id()
    if not actor:
        return "", "", None, None
    try:
        from spice.tasks import identity, tw

        rows = tw.export(["+ACTIVE"])
    except Exception:
        return "", "", None, None
    own_rows = [
        row
        for row in rows
        if str(row.get("claim_by") or "") == actor
        and _claim_worktree_matches(row, repo_root)
    ]
    if not own_rows:
        return "", "", None, None
    row = max(
        own_rows,
        key=lambda item: str(item.get("claim_at") or item.get("start") or ""),
    )
    claim_started_at = _iso_timestamp_seconds(str(row.get("claim_at") or ""))
    claim_expires_at = _iso_timestamp_seconds(str(row.get("claim_until") or ""))
    current = time.time() if now is None else now
    elapsed = None
    if claim_started_at is not None:
        elapsed = int(max(0.0, current - claim_started_at))
    remaining = None
    if claim_expires_at is not None:
        remaining = int(max(0.0, claim_expires_at - current))
    return (
        identity.render_handle(row),
        str(row.get("phase") or ""),
        elapsed,
        remaining,
    )


def _claim_lease_warning_level(remaining_seconds: int | None) -> str:
    if remaining_seconds is None or remaining_seconds >= CLAIM_LEASE_WARNING_SECONDS:
        return ""
    if remaining_seconds < CLAIM_LEASE_CRITICAL_SECONDS:
        return "critical"
    if remaining_seconds < CLAIM_LEASE_URGENT_SECONDS:
        return "urgent"
    return "warning"


def _claim_lease_warning_repeat_seconds(level: str) -> float:
    return {
        "warning": CLAIM_LEASE_WARNING_REPEAT_SECONDS,
        "urgent": CLAIM_LEASE_URGENT_REPEAT_SECONDS,
        "critical": CLAIM_LEASE_CRITICAL_REPEAT_SECONDS,
    }.get(level, math.inf)


def _working_state_repeat_seconds(
    snapshot: WorkingStateSnapshot, *, ceiling: float
) -> float:
    warning_repeat = _claim_lease_warning_repeat_seconds(
        _claim_lease_warning_level(snapshot.claim_remaining_seconds)
    )
    return min(ceiling, warning_repeat)


def _seconds_until_claim_lease_boundary(remaining: float, *, level: str) -> float:
    target = {
        "": CLAIM_LEASE_WARNING_SECONDS - 1,
        "warning": CLAIM_LEASE_URGENT_SECONDS - 1,
        "urgent": CLAIM_LEASE_CRITICAL_SECONDS - 1,
    }.get(level)
    if target is None:
        return math.inf
    return max(0.0, remaining - target)


def _claim_worktree_matches(row: dict[str, Any], repo_root: Path) -> bool:
    raw = str(row.get("claim_worktree") or "").strip()
    if not raw:
        return False
    try:
        return Path(raw).expanduser().resolve() == repo_root.expanduser().resolve()
    except OSError:
        return False


def _working_state_last_maxim_bag(repo_root: Path) -> str:
    try:
        from spice.agent.maximmetrics import latest_fire_bag_name

        return latest_fire_bag_name(repo_root)
    except Exception:
        return ""


def _iso_timestamp_seconds(value: str) -> float | None:
    clean = str(value or "").strip()
    if not clean:
        return None
    if clean.endswith("Z"):
        clean = f"{clean[:-1]}+00:00"
    try:
        return datetime.datetime.fromisoformat(clean).timestamp()
    except ValueError:
        return None


def read_context_meter_cache_payload(path: Path) -> dict[str, Any]:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


def _float_payload_value(value: Any) -> float | None:
    return float(value) if isinstance(value, int | float) else None


def _int_payload_value(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None
