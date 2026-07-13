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
import re
import shlex
import socket as socket
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TextIO

from spice import config
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
from spice.agent.sidechannelnotify import (
    side_channel_marker_path as side_channel_marker_path,
)
from spice.agent.identity import ambient_thread_id
from spice.agent.paths import agent_state_dir, agent_thread_state_dir
from spice.agent.rtkrewrite import (
    RTK_REWRITE_MATCH_EXIT_CODE as RTK_REWRITE_MATCH_EXIT_CODE,
    RTK_REWRITE_NO_MATCH_EXIT_CODE as RTK_REWRITE_NO_MATCH_EXIT_CODE,
    _rtk_warned_keys as _rtk_warned_keys,
    emit_rewrite_diagnostic as _emit_rtk_rewrite_diagnostic,
    rewrite_command_text as _rewrite_rtk_command_text,
)
from spice.agent.shellhook import (
    BASH_ENV_ENV,
    BASH_HOOK_NAME,
    ZDOTDIR_ENV,
    packaged_shell_steering_static_hook_dir,
)
from spice.errors import SpiceError
from spice.sessions.meter import (
    ContextMeter,
    active_context_percent,
    collect_latest_context_meter,
    context_meter_cache_payload,
    context_meter_from_cache_payload,
    GuidanceState,
    context_meter_instruction,
    context_pressure_level,
    context_pressure_should_warn,
)

PYTHON_ROUTE_COMMANDS = frozenset(("python", "python3"))
SHELL_EXECUTION_COMMANDS = frozenset(("bash", "dash", "sh", "zsh"))
SHELL_EXECUTION_FLAGS = frozenset(("-c", "-lc"))
RTK_MINIMUM_VERSION = (0, 42, 4)
RTK_MINIMUM_VERSION_TEXT = ".".join(str(part) for part in RTK_MINIMUM_VERSION)
RTK_UPSTREAM = "https://github.com/rtk-ai/rtk"
RTK_INSTALL_GUIDANCE = (
    f"install RTK >= {RTK_MINIMUM_VERSION_TEXT} from {RTK_UPSTREAM} "
    "(`brew install rtk` or `cargo install --git https://github.com/rtk-ai/rtk`)"
)
RTK_VERSION_PATTERN = re.compile(r"\brtk\s+(\d+)\.(\d+)\.(\d+)\b", re.IGNORECASE)
RTK_PROTOCOL_PROBE = ("git", "status")
RTK_DB_PATH_ENV = "RTK_DB_PATH"  # env-policy: allow

# The working-state banner is a change notification, not a periodic meter: once a
# given state has been shown it stays silent until the state itself changes, so
# it never becomes repeated noise on each shell command.
AGENT_RUN_WORKING_STATE_REPEAT_SECONDS = math.inf
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
WorkingStateKey = tuple[int, str, str, str]


@dataclass(frozen=True)
class WorkingStateSnapshot:
    pending_inbox_count: int = 0
    claim_handle: str = ""
    claim_phase: str = ""
    claim_elapsed_seconds: int | None = None
    last_maxim_bag: str = ""

    def has_fields(self) -> bool:
        return bool(
            self.pending_inbox_count or self.claim_handle or self.last_maxim_bag
        )


ProcessFactory = Callable[..., Any]
TimeFactory = Callable[[], float]
ContextMeterFactory = Callable[[Path | None], ContextMeter | None]


def context_meter_cache_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-meter.json"


def context_warning_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "context-warning.json"


def working_state_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / "working-state.json"


def run_agent_command(
    repo_root: Path | None,
    raw_args: Sequence[str],
    *,
    popen_factory: ProcessFactory = subprocess.Popen,
    stderr: TextIO = sys.stderr,
) -> int:
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
    if rewrite_rtk:
        args = rtk_rewrite_agent_run_args(
            args,
            repo_root=repo_root,
            rtk_environment=rtk_environment,
            rtk_stderr=rtk_stderr,
        )
    if rewrite_rtk:
        args = (
            rtk_rewrite_direct_args(
                args,
                repo_root=repo_root,
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
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> list[str]:
    shell_command_index = shell_execution_command_index(args)
    if shell_command_index is None:
        return list(args)
    rewritten = rtk_rewrite_shell_execution_text(
        args[shell_command_index],
        repo_root=repo_root,
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
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> str | None:
    rewritten = _rtk_rewrite_with_environment(
        command_text,
        repo_root=repo_root,
        rtk_environment=rtk_environment,
        rtk_stderr=rtk_stderr,
    )
    if rewritten is not None:
        return rewritten
    trailing = rtk_rewrite_trailing_exec_shell_command(
        command_text,
        repo_root=repo_root,
        rtk_environment=rtk_environment,
        rtk_stderr=rtk_stderr,
    )
    if trailing is not None:
        return trailing
    return driver_for(repo_root).rewrite_tool_command(
        command_text,
        lambda *args: _rtk_rewrite_with_environment(
            *args,
            repo_root=repo_root,
            rtk_environment=rtk_environment,
            rtk_stderr=rtk_stderr,
        ),
    )


def rtk_rewrite_trailing_exec_shell_command(
    command_text: str,
    *,
    repo_root: Path | None = None,
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
    rewritten = _rtk_rewrite_with_environment(
        nested_command,
        repo_root=repo_root,
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
    rtk_environment: Mapping[str, str] | None = None,
    rtk_stderr: TextIO | None = None,
) -> list[str] | None:
    executable = config.configured_rtk_executable(repo_root)
    if (
        not args
        or args[:1] == [executable]
        or shell_execution_command_index(args) is not None
    ):
        return None
    rewritten = _rtk_rewrite_with_environment(
        *args,
        repo_root=repo_root,
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
            executable=executable,
            failure_class="malformed-direct-argv",
            failure_signature=f"parse={type(exc).__name__}",
        )
        return None
    if parsed:
        return parsed
    _emit_rtk_rewrite_diagnostic(
        repo_root,
        rtk_stderr,
        executable=executable,
        failure_class="malformed-direct-argv",
        failure_signature="parse=empty-argv",
    )
    return None


def _rtk_rewrite_with_environment(
    *args: str,
    repo_root: Path | None,
    rtk_environment: Mapping[str, str] | None,
    rtk_stderr: TextIO | None,
) -> str | None:
    if rtk_environment is None:
        return rtk_rewrite_command_text(*args, repo_root=repo_root, stderr=rtk_stderr)
    return rtk_rewrite_command_text(
        *args,
        repo_root=repo_root,
        env=rtk_environment,
        stderr=rtk_stderr,
    )


def rtk_rewrite_command_text(
    *args: str,
    repo_root: Path | None = None,
    env: Mapping[str, str] | None = None,
    stderr: TextIO | None = None,
    run: Callable[..., subprocess.CompletedProcess[str]] | None = None,
) -> str | None:
    return _rewrite_rtk_command_text(
        *args,
        repo_root=repo_root,
        env=env,
        stderr=stderr,
        run=run,
    )


def validate_rtk_companion(
    *, run: Callable[..., subprocess.CompletedProcess[str]] | None = None
) -> str:
    runner = run or subprocess.run
    try:
        completed = runner(
            ["rtk", "--version"], capture_output=True, text=True, check=False
        )
    except OSError as exc:
        raise SpiceError(f"RTK unavailable: {exc}; {RTK_INSTALL_GUIDANCE}") from exc
    version_output = (completed.stdout or completed.stderr or "").strip()
    match = RTK_VERSION_PATTERN.search(version_output)
    if completed.returncode != 0 or match is None:
        raise SpiceError(
            f"could not validate RTK version from {version_output!r}; "
            f"{RTK_INSTALL_GUIDANCE}"
        )
    version = tuple(int(part) for part in match.groups())
    if version < RTK_MINIMUM_VERSION:
        raise SpiceError(
            f"RTK {'.'.join(str(part) for part in version)} is obsolete; "
            f"{RTK_INSTALL_GUIDANCE}"
        )
    rewritten = rtk_rewrite_command_text(*RTK_PROTOCOL_PROBE, run=runner)
    if rewritten is None:
        raise SpiceError(
            f"RTK rewrite probe did not rewrite {' '.join(RTK_PROTOCOL_PROBE)!r}; "
            f"{RTK_INSTALL_GUIDANCE}"
        )
    return ".".join(str(part) for part in version)


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
    del repo_root
    return worktree_python_route_command(args)


def worktree_python_route_command(args: Sequence[str]) -> list[str]:
    if args[:1] and args[0] in PYTHON_ROUTE_COMMANDS:
        return [sys.executable, *args[1:]]
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

    def inject(self, *, force: bool) -> None:
        del force
        try:
            snapshot = self.snapshot_factory(self.repo_root)
        except Exception:
            return
        if not snapshot.has_fields():
            return
        key = working_state_key(snapshot)
        now = self.time_factory()
        if self._should_suppress(key, now=now):
            return
        text = render_working_state_snapshot(snapshot)
        if not text:
            return
        self.stderr.write(text)
        self.stderr.write("\n")
        self.stderr.flush()
        self._record_displayed(key, now=now)

    def _should_suppress(self, key: WorkingStateKey, *, now: float) -> bool:
        if self._is_recent_match(self.displayed_key, self.displayed_at, key, now=now):
            return True
        stored_key, stored_at = read_working_state_state(self.repo_root)
        if self._is_recent_match(stored_key, stored_at, key, now=now):
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
    ) -> bool:
        if displayed_key != key or displayed_at is None:
            return False
        age = now - displayed_at
        return 0 <= age < self.repeat_interval_seconds


def collect_working_state_snapshot(
    repo_root: Path | None, *, now: float | None = None
) -> WorkingStateSnapshot:
    if repo_root is None:
        return WorkingStateSnapshot()
    root = Path(repo_root)
    claim_handle, claim_phase, claim_elapsed_seconds = _working_state_claim(
        root, now=now
    )
    return WorkingStateSnapshot(
        pending_inbox_count=_working_state_pending_count(root),
        claim_handle=claim_handle,
        claim_phase=claim_phase,
        claim_elapsed_seconds=claim_elapsed_seconds,
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
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps({"displayedAt": now, "key": list(key)}, separators=(",", ":"))
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def _working_state_key_payload(value: Any) -> WorkingStateKey | None:
    if not isinstance(value, list) or len(value) != 4:
        return None
    pending = _int_payload_value(value[0])
    claim_handle = value[1]
    claim_phase = value[2]
    last_maxim = value[3]
    if (
        pending is None
        or not isinstance(claim_handle, str)
        or not isinstance(claim_phase, str)
        or not isinstance(last_maxim, str)
    ):
        return None
    return (pending, claim_handle, claim_phase, last_maxim)


def _working_state_duration(seconds: int) -> str:
    value = max(0, int(seconds))
    return f"{value}s"


def _working_state_clean_text(value: object) -> str:
    return " ".join(str(value or "").split())


def _working_state_claim(
    repo_root: Path, *, now: float | None
) -> tuple[str, str, int | None]:
    actor = ambient_thread_id()
    if not actor:
        return "", "", None
    try:
        from spice.tasks import identity, tw

        rows = tw.export(["status:pending", "+ACTIVE"])
    except Exception:
        return "", "", None
    own_rows = [
        row
        for row in rows
        if str(row.get("claim_by") or "") == actor
        and _claim_worktree_matches(row, repo_root)
    ]
    if not own_rows:
        return "", "", None
    row = max(
        own_rows,
        key=lambda item: str(item.get("claim_at") or item.get("start") or ""),
    )
    claim_started_at = _iso_timestamp_seconds(str(row.get("claim_at") or ""))
    elapsed = None
    if claim_started_at is not None:
        elapsed = int(
            max(0.0, (time.time() if now is None else now) - claim_started_at)
        )
    return identity.render_handle(row), str(row.get("phase") or ""), elapsed


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
        from spice.agent.maximmetrics import MAXIM_EVENT_FIRE, maxim_metric_records

        records = maxim_metric_records(repo_root)
    except Exception:
        return ""
    for record in reversed(records):
        if record.event_type == MAXIM_EVENT_FIRE:
            return record.bag_name
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


class AgentContextMeterInjector:
    """Write keep-working guidance to stderr, repeat-suppressed on disk."""

    def __init__(
        self,
        repo_root: Path | None,
        *,
        stderr: TextIO,
        repeat_interval_seconds: float = AGENT_RUN_INBOX_REPEAT_SECONDS,
        time_factory: TimeFactory = time.monotonic,
        meter_factory: ContextMeterFactory,
    ) -> None:
        self.repo_root = repo_root
        self.stderr = stderr
        self.repeat_interval_seconds = max(0.0, repeat_interval_seconds)
        self.time_factory = time_factory
        self.meter_factory = meter_factory
        self.displayed_at: float | None = None
        self.displayed_key: ContextWarningKey | None = None

    def inject(self, *, force: bool) -> None:
        warning = render_agent_context_warning(self.meter_factory(self.repo_root))
        if warning is None:
            return
        signature, text = warning
        key = context_warning_key(signature)
        now = self.time_factory()
        if self._should_suppress(key, now=now):
            return
        self.stderr.write(text)
        if not text.endswith("\n"):
            self.stderr.write("\n")
        self.stderr.flush()
        self._record_displayed(key, now=now)

    def _should_suppress(self, key: ContextWarningKey, *, now: float) -> bool:
        if self._is_recent_match(self.displayed_key, self.displayed_at, key, now=now):
            return True
        stored_key, stored_at = read_context_warning_state(self.repo_root)
        if self._is_recent_match(stored_key, stored_at, key, now=now):
            self.displayed_key = stored_key
            self.displayed_at = stored_at
            return True
        return False

    def _record_displayed(self, key: ContextWarningKey, *, now: float) -> None:
        self.displayed_key = key
        self.displayed_at = now
        write_context_warning_state(self.repo_root, key, now=now)

    def _is_recent_match(
        self,
        displayed_key: ContextWarningKey | None,
        displayed_at: float | None,
        key: ContextWarningKey,
        *,
        now: float,
    ) -> bool:
        if displayed_key != key or displayed_at is None:
            return False
        age = now - displayed_at
        return 0 <= age < self.repeat_interval_seconds


def agent_context_meter(repo_root: Path | None) -> ContextMeter | None:
    thread_id = ambient_thread_id()
    if repo_root is None or not thread_id:
        return None
    now = time.time()
    cached = read_cached_agent_context_meter(repo_root, thread_id, now=now)
    if cached is not None:
        return cached
    try:
        transcript_path = driver_for(repo_root).thread_transcript_path(thread_id)
    except (RuntimeError, SystemExit):
        return None
    try:
        meter = collect_latest_context_meter([transcript_path])
    except OSError:
        return None
    write_cached_agent_context_meter(repo_root, thread_id, meter, now=now)
    return meter


def context_warning_key(signature: ContextWarningSignature) -> ContextWarningKey:
    return (signature[0],)


def read_context_warning_state(
    repo_root: Path | None,
) -> tuple[ContextWarningKey | None, float | None]:
    if repo_root is None:
        return None, None
    payload = read_context_meter_cache_payload(context_warning_state_path(repo_root))
    raw_key = payload.get("key")
    displayed_at = _float_payload_value(payload.get("displayedAt"))
    if (
        not isinstance(raw_key, list)
        or len(raw_key) != 1
        or not isinstance(raw_key[0], str)
        or displayed_at is None
    ):
        return None, None
    return (raw_key[0],), displayed_at


def write_context_warning_state(
    repo_root: Path | None, key: ContextWarningKey, *, now: float
) -> None:
    if repo_root is None:
        return
    path = context_warning_state_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps({"displayedAt": now, "key": list(key)}, separators=(",", ":"))
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def read_cached_agent_context_meter(
    repo_root: Path, thread_id: str, *, now: float
) -> ContextMeter | None:
    payload = read_context_meter_cache_payload(context_meter_cache_path(repo_root))
    if payload.get("threadId") != thread_id:
        return None
    checked_at = _float_payload_value(payload.get("checkedAt"))
    if checked_at is None:
        return None
    if now - checked_at > AGENT_RUN_CONTEXT_METER_CACHE_SECONDS:
        return None
    return context_meter_from_cache_payload(payload.get("meter"))


def read_context_meter_cache_payload(path: Path) -> dict[str, Any]:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


def write_cached_agent_context_meter(
    repo_root: Path, thread_id: str, meter: ContextMeter, *, now: float
) -> None:
    path = context_meter_cache_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps(
            {
                "checkedAt": now,
                "threadId": thread_id,
                "meter": context_meter_cache_payload(meter),
            },
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    tmp.replace(path)


def _float_payload_value(value: Any) -> float | None:
    return float(value) if isinstance(value, int | float) else None


def _int_payload_value(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def render_agent_context_warning(
    meter: ContextMeter | None,
) -> tuple[ContextWarningSignature, str] | None:
    if meter is None or meter.latest_snapshot is None:
        return None
    snapshot = meter.latest_snapshot
    percent = active_context_percent(snapshot)
    level = context_pressure_level(percent)
    if not context_pressure_should_warn(level):
        return None
    signature = (level, snapshot.ts, snapshot.total_tokens)
    instruction = context_meter_instruction(GuidanceState(level=level))
    return signature, instruction + "\n"
