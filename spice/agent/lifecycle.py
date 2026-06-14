"""Worktree-bound agent lifecycle: ensure, supervise, status, activation.

One agent inhabits one worktree. `ensure` starts a fresh agent, resumes the
recorded thread, or — under renewal — forces a new successor; the launch is
serialized by an ensure-lock and recorded in durable state under
`.spice/agents/<driver>/state.json`. The agent runs under a detached
supervisor process (`spice agent supervise`) that owns the side-channel
socket and the stdout watchdog, publishes the agent state, and survives the
parent that launched it.

The prompt boundary: the initial prompt is only a neutral skill invocation.
Operator prose never rides the start prompt — the agent recovers intent from
activation, session briefing, the task board, and inbox steering.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from threading import Thread
from typing import Any, Iterator, Sequence, cast

from spice.agent.driver import driver_for
from spice.agent.gitshadow import agent_git_shadow_environment
from spice.agent.identity import ambient_thread, ambient_thread_id, canonical_thread_id
from spice.agent.watchdog import spawn_supervised_agent
from spice.agent.wrap import agent_state_dir
from spice.config import (
    configured_agent_model,
    configured_agent_personality,
    configured_agent_thinking,
)
from spice.errors import SpiceError
from spice.locking import lock_fd_exclusive, unlock_fd
from spice.paths import worktree_spice_environment
from spice.procs import (
    popen_new_process_group_kwargs,
    process_group_is_running,
    process_id_is_running,
)

# The one skill location: the standard agent-skills path, in every worktree.
# The launch prompt must link a file inside the agent's own worktree: an
# absolute path into another checkout follows that checkout's edits and
# reinstalls, which is exactly the cross-worktree trust failure this file
# exists to prevent. A repo that tracks the file in git owns it (its copy is
# never rewritten); otherwise spice keeps it fresh from the packaged source.
WORKTREE_SKILL_RELATIVE_PATH = Path(".agents") / "skills" / "spice" / "SKILL.md"
PACKAGED_SKILL_RESOURCE = ("spice.agent", "SKILL.md")
AGENT_STATE_FILE = "state.json"
AGENT_LOCK_FILE = "ensure.lock"
SUPERVISOR_ENVIRONMENT_SCRUB_NAMES = (
    "VIRTUAL_ENV",
    "UV_PROJECT_ENVIRONMENT",
)
STARTUP_GRACE_SECONDS = 0.25
STARTUP_SESSION_ID_TIMEOUT_SECONDS = 1.0
STARTUP_SESSION_ID_POLL_SECONDS = 0.05
SUPERVISOR_STARTUP_TIMEOUT_SECONDS = 3.0
STARTUP_LOG_HEAD_BYTES = 4096
STARTUP_LOG_TAIL_BYTES = 4096


@dataclass(frozen=True)
class AgentStatus:
    repo_root: Path
    state_path: Path
    process_status: str
    pid: int | None
    process_group_id: int | None
    thread_id: str
    model: str
    reasoning_effort: str
    service_tier: str
    started_at: str
    log_path: Path | None
    prompt_skill_path: Path | None
    command: tuple[str, ...]

    @property
    def running(self) -> bool:
        return self.process_status == "running"


@dataclass(frozen=True)
class AgentEnsureResult:
    action: str
    status: AgentStatus
    command: list[str]
    prompt: str
    log_path: Path | None


def agent_status(repo_root: Path) -> AgentStatus:
    resolved_root = repo_root.resolve()
    state = read_agent_state(resolved_root)
    agent_state = state if agent_state_is_authoritative(state) else {}
    pid = state_int(agent_state.get("pid"))
    pgid = state_int(agent_state.get("process_group_id")) or pid
    running = process_id_is_running(pid) and process_group_is_running(pgid)
    thread_id = canonical_thread_id(agent_state.get("thread_id"))
    skill_path = state_path_value(agent_state.get("prompt_skill_path"))
    if skill_path is None:
        skill_path = available_skill_path(resolved_root, required=False)
    command = state_command_value(agent_state.get("command"))
    return AgentStatus(
        repo_root=resolved_root,
        state_path=agent_state_path(resolved_root),
        process_status=agent_process_status(
            running=running, state=agent_state, thread_id=thread_id
        ),
        pid=pid,
        process_group_id=pgid,
        thread_id=thread_id,
        model=str(agent_state.get("model") or ""),
        reasoning_effort=str(agent_state.get("reasoning_effort") or ""),
        service_tier=str(agent_state.get("service_tier") or ""),
        started_at=str(agent_state.get("started_at") or ""),
        log_path=state_path_value(agent_state.get("log_path")),
        prompt_skill_path=skill_path,
        command=command,
    )


def agent_binding_error(repo_root: Path, status: Any) -> str:
    expected_root = repo_root.expanduser().resolve()
    status_root = (
        Path(getattr(status, "repo_root", expected_root) or expected_root)
        .expanduser()
        .resolve()
    )
    if status_root != expected_root:
        return (
            "agent binding mismatch: status root "
            f"{status_root} != lane root {expected_root}"
        )
    command_cwd = agent_command_cwd(getattr(status, "command", ()))
    command_root = command_cwd.expanduser().resolve() if command_cwd else None
    if command_root is not None and command_root != expected_root:
        return (
            "agent binding mismatch: launch cwd "
            f"{command_root} != lane root {expected_root}"
        )
    return ""


def agent_command_cwd(command: Sequence[str]) -> Path | None:
    args = list(command)
    for index, part in enumerate(args[:-1]):
        if part == "--cd":
            return Path(args[index + 1])
    return None


def ensure_agent(
    repo_root: Path,
    *,
    dry_run: bool = False,
    force_new: bool = False,
    model: str = "",
    reasoning_effort: str = "",
    personality: str | None = None,
    agent_bin: str = "",
    fast_mode: bool = False,
    supervise_stdout: bool = True,
) -> AgentEnsureResult:
    resolved_root = repo_root.resolve()
    with agent_ensure_lock(resolved_root):
        status = agent_status(resolved_root)
        driver = driver_for(resolved_root)
        prompt_skill_path = resolve_agent_prompt_skill_path(resolved_root)
        prompt = skill_invocation_prompt(resolved_root, prompt_skill_path)
        if status.running:
            return AgentEnsureResult(
                action="already-running",
                status=status,
                command=[],
                prompt=prompt,
                log_path=status.log_path,
            )
        resume_thread_id = "" if force_new else status.thread_id
        service_tier = driver.default_service_tier if fast_mode else ""
        # Resolution order: explicit argument > worktree-local config >
        # tracked project config > the driver's shipped default.
        model = model or configured_agent_model(resolved_root) or driver.default_model
        reasoning_effort = (
            reasoning_effort
            or configured_agent_thinking(resolved_root)
            or driver.default_reasoning_effort
        )
        command = driver.build_exec_command(
            repo_root=resolved_root,
            prompt=prompt,
            thread_id=resume_thread_id,
            model=model,
            reasoning_effort=reasoning_effort,
            personality=personality or configured_agent_personality(resolved_root),
            service_tier=service_tier,
            binary=agent_bin,
            fast_mode=fast_mode,
        )
        action = "renew" if force_new else ("resume" if resume_thread_id else "start")
        if dry_run:
            return AgentEnsureResult(
                action=f"would-{action}",
                status=status,
                command=command,
                prompt=prompt,
                log_path=None,
            )
        log_path = start_agent(
            resolved_root,
            action=action,
            command=command,
            model=model,
            reasoning_effort=reasoning_effort,
            service_tier=service_tier,
            resume_thread_id=resume_thread_id,
            prompt_skill_path=prompt_skill_path,
            fast_mode=fast_mode,
            supervise_stdout=supervise_stdout,
        )
        return AgentEnsureResult(
            action=action,
            status=agent_status(resolved_root),
            command=command,
            prompt=prompt,
            log_path=log_path,
        )


def start_agent(
    repo_root: Path,
    *,
    action: str,
    command: list[str],
    model: str,
    reasoning_effort: str,
    service_tier: str,
    resume_thread_id: str,
    prompt_skill_path: Path,
    fast_mode: bool,
    supervise_stdout: bool,
) -> Path:
    log_path = next_agent_log_path(repo_root)
    if supervise_stdout:
        supervisor = spawn_agent_supervisor(
            repo_root,
            action=action,
            command=command,
            model=model,
            reasoning_effort=reasoning_effort,
            service_tier=service_tier,
            resume_thread_id=resume_thread_id,
            log_path=log_path,
            fast_mode=fast_mode,
        )
        require_supervisor_started(supervisor, repo_root=repo_root, log_path=log_path)
        reap_process_when_done(supervisor)
        return log_path
    process = spawn_agent(command, cwd=repo_root, log_path=log_path)
    require_started_process(process, log_path)
    started_thread_id = started_agent_thread_id(
        log_path, repo_root=repo_root, fallback_thread_id=resume_thread_id
    )
    write_agent_state(
        repo_root,
        build_agent_state(
            process=process,
            action=action,
            command=command,
            model=model,
            reasoning_effort=reasoning_effort,
            service_tier=service_tier,
            thread_id=started_thread_id,
            prompt_skill_path=prompt_skill_path,
            log_path=log_path,
            fast_mode=fast_mode,
        ),
    )
    reap_process_when_done(process)
    return log_path


def build_agent_state(
    *,
    process: subprocess.Popen[str],
    action: str,
    command: list[str],
    model: str,
    reasoning_effort: str,
    service_tier: str,
    thread_id: str,
    prompt_skill_path: Path,
    log_path: Path,
    fast_mode: bool,
) -> dict[str, Any]:
    return {
        "pid": process.pid,
        "process_group_id": process.pid,
        "started_at": utc_now(),
        "mode": action,
        "command": command,
        "model": model,
        "reasoning_effort": reasoning_effort,
        "service_tier": service_tier,
        "thread_id": thread_id,
        "prompt_skill_path": str(prompt_skill_path),
        "log_path": str(log_path),
        "fast_mode": fast_mode,
    }


def spawn_agent_supervisor(
    repo_root: Path,
    *,
    action: str,
    command: list[str],
    model: str,
    reasoning_effort: str,
    service_tier: str,
    resume_thread_id: str,
    log_path: Path,
    fast_mode: bool,
) -> subprocess.Popen[str]:
    supervisor_command = [
        sys.executable,
        "-m",
        "spice",
        "agent",
        "supervise",
        "--repo-root",
        str(repo_root),
        "--action",
        action,
        "--model",
        model,
        "--reasoning-effort",
        reasoning_effort,
        "--service-tier",
        service_tier,
        "--resume-thread-id",
        resume_thread_id,
        "--log-path",
        str(log_path),
        "--command-json",
        json.dumps(command, separators=(",", ":")),
    ]
    if fast_mode:
        supervisor_command.append("--fast-mode")
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_handle = log_path.open("ab")
    try:
        process = subprocess.Popen(
            supervisor_command,
            cwd=repo_root,
            stdin=subprocess.DEVNULL,
            stdout=log_handle,
            stderr=subprocess.STDOUT,
            env=agent_supervisor_environment(repo_root),
            **popen_new_process_group_kwargs(),
        )
        return cast(subprocess.Popen[str], process)
    finally:
        log_handle.close()


def require_supervisor_started(
    process: subprocess.Popen[str], *, repo_root: Path, log_path: Path
) -> None:
    deadline = time.monotonic() + SUPERVISOR_STARTUP_TIMEOUT_SECONDS
    while True:
        state = read_agent_state(repo_root)
        if state_path_value(state.get("log_path")) == log_path:
            pid = state_int(state.get("pid"))
            if process_id_is_running(pid):
                return
        exit_code = process.poll()
        if exit_code is not None:
            detail = tail_text(log_path, STARTUP_LOG_TAIL_BYTES)
            message = f"agent supervisor exited during startup with code {exit_code}"
            raise SpiceError(f"{message}: {detail}" if detail else message)
        if time.monotonic() >= deadline:
            detail = tail_text(log_path, STARTUP_LOG_TAIL_BYTES)
            message = "agent supervisor did not publish agent state during startup"
            raise SpiceError(f"{message}: {detail}" if detail else message)
        time.sleep(STARTUP_SESSION_ID_POLL_SECONDS)


def run_agent_supervisor(args: argparse.Namespace) -> int:
    from spice.agent.sidechannel import AgentSideChannelServer

    repo_root = Path(str(args.repo_root)).expanduser().resolve()
    log_path = Path(str(args.log_path)).expanduser()
    command = supervisor_command_from_json(str(args.command_json))
    prompt_skill_path = resolve_agent_prompt_skill_path(repo_root)
    env = agent_environment(repo_root)
    with AgentSideChannelServer(repo_root):
        process, stdout_thread = spawn_supervised_agent(
            command,
            cwd=repo_root,
            log_path=log_path,
            env=env,
        )
        require_started_process(process, log_path)
        started_thread_id = started_agent_thread_id(
            log_path,
            repo_root=repo_root,
            fallback_thread_id=str(args.resume_thread_id or ""),
        )
        state = build_agent_state(
            process=process,
            action=str(args.action),
            command=command,
            model=str(args.model),
            reasoning_effort=str(args.reasoning_effort),
            service_tier=str(args.service_tier or ""),
            thread_id=started_thread_id,
            prompt_skill_path=prompt_skill_path,
            log_path=log_path,
            fast_mode=bool(getattr(args, "fast_mode", False)),
        )
        state["supervisor_pid"] = os.getpid()
        write_agent_state(repo_root, state)
        exit_code = process.wait()
        stdout_thread.join(timeout=1.0)
    return int(exit_code or 0)


def supervisor_command_from_json(raw: str) -> list[str]:
    try:
        loaded = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SpiceError(f"invalid supervisor command JSON: {exc}") from exc
    if not isinstance(loaded, list) or not all(
        isinstance(item, str) for item in loaded
    ):
        raise SpiceError("supervisor command JSON must be a list of strings")
    return loaded


def resolve_agent_prompt_skill_path(
    repo_root: Path,
) -> Path:
    located = available_skill_path(repo_root, required=True)
    if located is None:
        raise SpiceError("missing spice skill")
    return located


def spawn_agent(
    command: list[str], *, cwd: Path, log_path: Path
) -> subprocess.Popen[str]:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_handle = log_path.open("ab")
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=log_handle,
            stderr=subprocess.STDOUT,
            env=agent_environment(cwd),
            **popen_new_process_group_kwargs(),
        )
        return cast(subprocess.Popen[str], process)
    finally:
        log_handle.close()


def require_started_process(process: subprocess.Popen[str], log_path: Path) -> None:
    time.sleep(STARTUP_GRACE_SECONDS)
    exit_code = process.poll()
    if exit_code is None:
        return
    detail = tail_text(log_path, STARTUP_LOG_TAIL_BYTES)
    message = f"agent exited during startup with code {exit_code}"
    raise SpiceError(f"{message}: {detail}" if detail else message)


def reap_process_when_done(process: subprocess.Popen[str]) -> None:
    Thread(
        target=process.wait,
        name=f"spice-agent-reaper-{process.pid}",
        daemon=True,
    ).start()


def tail_text(path: Path, limit: int) -> str:
    try:
        with path.open("rb") as handle:
            handle.seek(0, os.SEEK_END)
            size = handle.tell()
            handle.seek(max(0, size - limit), os.SEEK_SET)
            return handle.read().decode("utf-8", errors="replace").strip()
    except OSError:
        return ""


def head_text(path: Path, limit: int) -> str:
    try:
        with path.open("rb") as handle:
            return handle.read(limit).decode("utf-8", errors="replace").strip()
    except OSError:
        return ""


def started_agent_thread_id(
    log_path: Path, *, repo_root: Path, fallback_thread_id: str
) -> str:
    if fallback_thread_id:
        return canonical_thread_id(fallback_thread_id)
    deadline = time.monotonic() + STARTUP_SESSION_ID_TIMEOUT_SECONDS
    while True:
        thread_id = parse_agent_session_id(
            head_text(log_path, STARTUP_LOG_HEAD_BYTES), repo_root
        )
        if thread_id:
            return thread_id
        if time.monotonic() >= deadline:
            return ""
        time.sleep(STARTUP_SESSION_ID_POLL_SECONDS)


def parse_agent_session_id(text: str, repo_root: Path) -> str:
    pattern = cast(re.Pattern[str], driver_for(repo_root).session_id_pattern)
    match = pattern.search(text)
    return canonical_thread_id(match.group(1)) if match else ""


@contextmanager
def agent_ensure_lock(repo_root: Path) -> Iterator[None]:
    lock_path = agent_state_dir(repo_root) / AGENT_LOCK_FILE
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    handle = lock_path.open("a+")
    try:
        lock_fd_exclusive(handle.fileno(), blocking=True)
        yield
    finally:
        unlock_fd(handle.fileno())
        handle.close()


def skill_invocation_prompt(repo_root: Path, skill_path: Path) -> str:
    return driver_for(repo_root).skill_invocation_prompt(skill_path)


def available_skill_path(repo_root: Path, *, required: bool) -> Path | None:
    """The spice skill, always inside `repo_root`.

    The materialized (or repo-owned) worktree file is the only runtime path.
    The packaged skill is only a source for writing that file into the tree.
    """
    materialized = materialize_worktree_skill(repo_root)
    if materialized is not None:
        return materialized
    if required:
        raise SpiceError(f"missing spice skill at {worktree_skill_path(repo_root)}")
    return None


def worktree_skill_path(repo_root: Path) -> Path:
    return (repo_root / WORKTREE_SKILL_RELATIVE_PATH).expanduser().resolve()


def materialize_worktree_skill(repo_root: Path) -> Path | None:
    """The worktree's skill file, kept fresh; None when the tree can't hold one.

    A git-tracked copy is repo-owned and used verbatim. An untracked copy is
    rewritten whenever it drifts from the packaged source, so reinstalling
    spice updates every worktree on its next activation or launch.
    """
    target = worktree_skill_path(repo_root)
    packaged = packaged_skill_path()
    if not packaged.is_file():
        return target if target.is_file() else None
    content = packaged.read_text(encoding="utf-8")
    try:
        if target.is_file():
            if target.read_text(encoding="utf-8") == content:
                return target
            if _skill_is_repo_owned(repo_root):
                return target
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
    except OSError:
        return target if target.is_file() else None
    return target


def _skill_is_repo_owned(repo_root: Path) -> bool:
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(repo_root),
            "ls-files",
            "--error-unmatch",
            "--",
            WORKTREE_SKILL_RELATIVE_PATH.as_posix(),
        ],
        capture_output=True,
        check=False,
        text=True,
    )
    return completed.returncode == 0


def packaged_skill_path() -> Path:
    return Path(__file__).resolve().parent / PACKAGED_SKILL_RESOURCE[1]


def agent_state_path(repo_root: Path) -> Path:
    return agent_state_dir(repo_root) / AGENT_STATE_FILE


def read_agent_state(repo_root: Path) -> dict[str, Any]:
    path = agent_state_path(repo_root)
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}
    return data if isinstance(data, dict) else {}


def write_agent_state(repo_root: Path, state: dict[str, Any]) -> None:
    path = agent_state_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(state, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def agent_state_is_authoritative(state: dict[str, Any]) -> bool:
    return all(
        str(state.get(key) or "") for key in ("mode", "started_at", "prompt_skill_path")
    )


def next_agent_log_path(repo_root: Path) -> Path:
    stamp = utc_now().replace(":", "").replace("-", "")
    return agent_state_dir(repo_root) / "logs" / f"{stamp}.log"


def state_int(value: Any) -> int | None:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return None
    return parsed if parsed > 0 else None


def state_path_value(value: Any) -> Path | None:
    if not value:
        return None
    return Path(str(value)).expanduser()


def state_command_value(value: Any) -> tuple[str, ...]:
    if not isinstance(value, list):
        return ()
    return tuple(item for item in value if isinstance(item, str))


def agent_process_status(
    *, running: bool, state: dict[str, Any], thread_id: str
) -> str:
    if running:
        return "running"
    if not state:
        return "unstarted"
    return "idle" if thread_id else "stopped"


def agent_environment(repo_root: Path | None = None) -> dict[str, str]:
    ambient = ambient_thread()
    if ambient is not None:
        _thread_id, driver = ambient
        raise SpiceError(
            f"refusing to spawn an agent with ambient {driver.thread_id_env} set; "
            f"unset {driver.thread_id_env} before starting spice serve or "
            "agent ensure"
        )
    env = worktree_spice_environment(repo_root)
    return agent_git_shadow_environment(repo_root, base_env=env)


def agent_supervisor_environment(repo_root: Path | None = None) -> dict[str, str]:
    env = agent_environment(repo_root)
    for name in SUPERVISOR_ENVIRONMENT_SCRUB_NAMES:
        env.pop(name, None)
    env["GIT_EDITOR"] = "true"
    return env


def utc_now() -> str:
    return datetime.now(UTC).isoformat(timespec="microseconds").replace("+00:00", "Z")


def bind_ambient_agent_activation(repo_root: Path) -> AgentStatus:
    ambient = ambient_thread_id()
    if not ambient:
        return agent_status(repo_root)
    state = read_agent_state(repo_root)
    if agent_state_is_authoritative(state):
        state["thread_id"] = ambient
    else:
        prompt_skill_path = available_skill_path(repo_root, required=False)
        state = {
            "pid": os.getpid(),
            "process_group_id": os.getpgrp(),
            "started_at": utc_now(),
            "mode": "activation",
            "command": [],
            "model": "",
            "reasoning_effort": "",
            "service_tier": "",
            "thread_id": ambient,
            "prompt_skill_path": str(prompt_skill_path or ""),
            "log_path": "",
        }
    write_agent_state(repo_root, state)
    return agent_status(repo_root)
