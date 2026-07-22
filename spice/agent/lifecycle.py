"""Worktree-bound agent lifecycle: ensure, supervise, status, activation.

One agent inhabits one worktree. `ensure` starts a fresh agent, resumes the
recorded thread, or — under renewal — forces a new successor; the launch is
serialized by an ensure-lock and recorded in durable state under
git-backed agent state. Runtime state always lives under this worktree's git
dir at `spice/agents/<driver>/`; once the real thread id is known, thread-owned
state and logs live under `spice/agents/<driver>/<thread-id>/` in that same
worktree git dir.
The agent runs under a detached supervisor process (`spice agent supervise`)
that owns the side-channel socket and the stdout watchdog, publishes the agent
state, and survives the parent that launched it.

The prompt boundary: the initial prompt is only a neutral skill invocation.
Operator prose never rides the start prompt — the agent recovers intent from
activation, session briefing, the task board, and inbox steering.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from threading import Event, Thread
from typing import Any, cast

from spice.agent.driver import driver_for
from spice.agent.identity import canonical_thread_id, uuid_thread_id
from spice.agent.shadow import ensure_origin_head
from spice.agent.paths import (
    agent_state_dir,
    agent_thread_state_dir,
)
from spice.agent.launchhistory import (  # noqa: F401 - lifecycle public surface
    LAUNCH_OUTCOMES_FILE,
    LAUNCH_OUTCOMES_LIMIT,
    RAPID_DEATH_LIFETIME_SECONDS,
    RAPID_DEATH_REFUSAL_THRESHOLD,
    RAPID_DEATH_REFUSAL_WINDOW_SECONDS,
    STARTUP_LOG_HEAD_BYTES,
    STARTUP_LOG_TAIL_BYTES,
    STARTUP_SESSION_ID_POLL_SECONDS,
    STARTUP_SESSION_ID_TIMEOUT_SECONDS,
    agent_process_failure_kind,
    head_text,
    launch_outcomes_path,
    launch_refusal,
    parse_agent_session_id,
    read_launch_outcomes,
    record_launch_outcome,
    scan_launch_log,
    started_agent_thread_id,
    supervised_launch_outcome,
    tail_text,
)
from spice.agent.lifecyclebinding import (  # noqa: F401 - lifecycle public surface
    AGENT_ENSURE_LOCK_TIMEOUT_SECONDS,
    AGENT_LOCK_FILE,
    AGENT_OUTPUT_STALL_SECONDS,
    AGENT_STATE_FILE,
    AGENT_STARTUP_READY,
    AGENT_STARTUP_STALLED,
    AGENT_STARTUP_STARTING,
    PACKAGED_SKILL_RESOURCE,
    SUPERVISOR_ENVIRONMENT_SCRUB_NAMES,
    WORKTREE_SKILL_GITIGNORE_CONTENT,
    WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH,
    WORKTREE_SKILL_RELATIVE_PATH,
    AgentStatus,
    AgentOutputObservation,
    _carry_member_driver,
    _carry_team_membership,
    agent_binding_error,
    agent_command_cwd,
    agent_ensure_lock as _agent_ensure_lock,
    agent_environment,
    agent_output_observation,
    agent_process_status,
    agent_state_is_authoritative,
    agent_state_path,
    agent_status,
    agent_supervisor_environment,
    available_skill_path as _available_skill_path,
    bind_ambient_agent_activation,
    git_tracks_relative_path,
    materialize_worktree_skill,
    materialize_worktree_skill_gitignore,
    packaged_skill_path,
    read_agent_state,
    settle_agent_log_path,
    state_command_value,
    state_int,
    state_path_value,
    utc_now,
    worktree_skill_gitignore_path,
    worktree_skill_path,
    write_agent_state,
)
from spice.agent.watchdog import (
    AgentStartupSignal,
    spawn_supervised_agent,
    startup_signal_for_supervised_thread,
)
from spice.config.values import (
    configured_agent_effort,
    configured_agent_model,
    configured_agent_personality,
)
from spice.errors import SpiceError
from spice.process.git import git_probe
from spice.process.groups import (
    PROCESS_GROUP_TERMINATION_BOUND_SECONDS,
    popen_new_process_group_kwargs,
    process_id_is_running,
    terminate_process_group,
)
from spice.tasks import gitsync

STARTUP_GRACE_SECONDS = 0.25
SUPERVISOR_STARTUP_TIMEOUT_SECONDS = 3.0
FIRST_ACTIVITY_GRACE_SECONDS = 120.0
STARTUP_STATE_PERSISTENCE_ALLOWANCE_SECONDS = 3.0
STARTUP_WATCH_JOIN_SECONDS = (
    PROCESS_GROUP_TERMINATION_BOUND_SECONDS
    + STARTUP_STATE_PERSISTENCE_ALLOWANCE_SECONDS
)
AGENT_FAILURE_OUT_OF_CREDITS = "out-of-credits"
AGENT_FAILURE_RESTART_REFUSED = "restart-refused"
AGENT_FAILURE_STARTUP_STALLED = AGENT_STARTUP_STALLED


class AgentOutOfCreditsError(SpiceError):
    """Agent driver reported a credit/usage-limit startup failure."""


class AgentRestartRefusedError(SpiceError):
    """Automatic restart refused: recent supervised launches keep dying young."""

    def __init__(self, message: str, *, refusal: dict[str, Any]) -> None:
        super().__init__(message)
        self.refusal = refusal


@dataclass(frozen=True)
class AgentEnsureResult:
    action: str
    status: AgentStatus
    command: list[str]
    prompt: str
    log_path: Path | None


def _claimed_task_phase_launch(
    repo_root: Path, driver_name: str, status: AgentStatus
) -> dict[str, str]:
    """Model/effort override from the phase of this worktree's claimed task.

    {} when no task is claimed, the task backend is unavailable, or the
    phase has no configured override — the caller falls through to its
    ordinary launch config in every one of those cases.
    """
    if not status.thread_id:
        return {}
    try:
        from spice.tasks.claimstate import active_claim_phase

        phase = active_claim_phase(status.thread_id)
    except SpiceError:
        return {}
    if not phase:
        return {}
    from spice.tasks.config import phase_launch_overrides

    return phase_launch_overrides(repo_root, driver_name, phase)


def agent_ensure_lock(repo_root: Path):
    return _agent_ensure_lock(
        repo_root, timeout_seconds=AGENT_ENSURE_LOCK_TIMEOUT_SECONDS
    )


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
    automatic: bool = False,
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
        # Only automatic wake paths honor the refusal; an explicit operator
        # start is itself the grant of exactly one new attempt, and the
        # journal it leaves behind re-arms the refusal if that attempt also
        # dies young.
        if automatic:
            refusal = launch_refusal(resolved_root)
            if refusal is not None:
                raise AgentRestartRefusedError(
                    "automatic restart refused: "
                    f"{refusal['consecutive_rapid_deaths']} consecutive launches "
                    f"died within {RAPID_DEATH_LIFETIME_SECONDS:g}s; "
                    f"holding until epoch {refusal['hold_until_epoch']} "
                    "unless an operator starts the agent explicitly",
                    refusal=refusal,
                )
        resume_thread_id = "" if force_new else status.thread_id
        service_tier = driver.default_service_tier if fast_mode else ""
        phase_launch = _claimed_task_phase_launch(resolved_root, driver.name, status)
        # Resolution order: explicit argument > the claimed task's phase
        # mapping for this driver > the effective four-layer configuration >
        # the driver's shipped default.
        phase_model = phase_launch.get("model", "")
        model = driver.resolve_model(
            model or phase_model or configured_agent_model(resolved_root)
        )
        reasoning_effort = (
            reasoning_effort
            or phase_launch.get("effort", "")
            or configured_agent_effort(resolved_root)
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
        ensure_origin_head(resolved_root)
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
    # This shared boundary covers both launch modes. It intentionally runs in
    # the globally installed parent before the detached ``python -m spice``
    # supervisor can import the worktree checkout.
    gitsync.prepare_for_agent_launch(repo_root)
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
        reap_process_when_done(supervisor, repo_root=repo_root)
        return log_path
    process = spawn_agent(command, cwd=repo_root, log_path=log_path)
    require_started_process(process, log_path, repo_root=repo_root)
    started_thread_id = started_agent_thread_id(
        log_path, repo_root=repo_root, fallback_thread_id=resume_thread_id
    )
    log_path = settle_agent_log_path(repo_root, log_path, started_thread_id)
    write_agent_state(
        repo_root,
        build_agent_state(
            process=process,
            action=action,
            command=command,
            driver=driver_for(repo_root).name,
            model=model,
            reasoning_effort=reasoning_effort,
            service_tier=service_tier,
            thread_id=started_thread_id,
            prompt_skill_path=prompt_skill_path,
            log_path=log_path,
            fast_mode=fast_mode,
        ),
    )
    reap_process_when_done(process, repo_root=repo_root)
    return log_path


def next_agent_log_path(repo_root: Path) -> Path:
    stamp = utc_now().replace(":", "").replace("-", "")
    return agent_state_dir(repo_root) / f"{stamp}.log"


def build_agent_state(
    *,
    process: subprocess.Popen[str],
    action: str,
    command: list[str],
    driver: str,
    model: str,
    reasoning_effort: str,
    service_tier: str,
    thread_id: str,
    prompt_skill_path: Path,
    log_path: Path,
    fast_mode: bool,
    startup_status: str = AGENT_STARTUP_READY,
) -> dict[str, Any]:
    return {
        "pid": process.pid,
        "process_group_id": process.pid,
        "started_at": utc_now(),
        "mode": action,
        "command": command,
        "driver": driver,
        "model": model,
        "reasoning_effort": reasoning_effort,
        "service_tier": service_tier,
        "thread_id": thread_id,
        "prompt_skill_path": str(prompt_skill_path),
        "log_path": str(log_path),
        "fast_mode": fast_mode,
        "startup_status": startup_status,
        "ready_at": utc_now() if startup_status == AGENT_STARTUP_READY else "",
        "startup_failure": "",
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
        if agent_state_matches_startup_log(repo_root, state, log_path):
            pid = state_int(state.get("pid"))
            if process_id_is_running(pid):
                return
        exit_code = process.poll()
        if exit_code is not None:
            detail = tail_text(log_path, STARTUP_LOG_TAIL_BYTES)
            message = f"agent supervisor exited during startup with code {exit_code}"
            raise agent_startup_error(
                repo_root,
                exit_code=exit_code,
                message=message,
                detail=detail,
            )
        if time.monotonic() >= deadline:
            detail = tail_text(log_path, STARTUP_LOG_TAIL_BYTES)
            message = "agent supervisor did not publish agent state during startup"
            raise agent_startup_error(
                repo_root,
                exit_code=0,
                message=message,
                detail=detail,
            )
        time.sleep(STARTUP_SESSION_ID_POLL_SECONDS)


def agent_state_matches_startup_log(
    repo_root: Path, state: dict[str, Any], log_path: Path
) -> bool:
    state_log_path = state_path_value(state.get("log_path"))
    if state_log_path is None:
        return False
    expected = log_path.expanduser().resolve()
    actual = state_log_path.expanduser().resolve()
    if actual == expected:
        return True
    thread_id = canonical_thread_id(state.get("thread_id"))
    if not thread_id:
        return False
    settled = (
        agent_thread_state_dir(repo_root, thread_id) / "logs" / expected.name
    ).resolve()
    return actual == settled


# A low-frequency check, not a spinner: the operator asked the supervisor to
# notice ~every 30-60s when its bound agent is holding no task yet the worktree
# is dirty -- uncaptured work that cannot land until a task is claimed.
SUPERVISOR_LANE_WATCH_SECONDS = 45.0
# Claim TTL is one hour; renewing every 15 minutes gives long-running agents a
# wide safety margin without turning the task backend into a heartbeat log.
SUPERVISOR_CLAIM_RENEWAL_SECONDS = 15.0 * 60.0
LANE_UNCAPTURED_NUDGE = (
    "your worktree has uncommitted or uncaptured changes but you hold no "
    "claimed task -- work cannot land without one. Claim a task before "
    "editing further, or fold the changes in with spice task capture."
)
CLAIM_RENEWAL_QUIET_REASONS = frozenset({"no_active_claim"})


# Supervisor-side git probes run on the lane-watch loop; a wedged git binary must
# not stall progress, so each rides the probe door's budget and reports the safe
# "no signal" answer on expiry (tree treated as clean; path treated as untracked).
def _worktree_dirty(repo_root: Path) -> bool:
    result = git_probe(repo_root, "status", "--porcelain")
    return result.returncode == 0 and result.stdout.strip() != ""


def _flag_uncaptured_lane(repo_root: Path, thread_id: str, log_path: Path) -> None:
    """Surface a nudge when the bound agent holds no task but the tree is dirty."""
    from spice.agent.watchdog import publish_supervisor_feedback
    from spice.tasks.claimstate import active_claim

    if not thread_id or active_claim(thread_id) is not None:
        return
    if not _worktree_dirty(repo_root):
        return
    with log_path.open("a", encoding="utf-8") as log_handle:
        publish_supervisor_feedback(
            repo_root, log_handle, "lane.uncaptured", message=LANE_UNCAPTURED_NUDGE
        )


def _claim_renewal_report_key(result: Any) -> str:
    return "\0".join(
        str(part)
        for part in (
            getattr(result, "reason", ""),
            getattr(result, "handle", ""),
            getattr(result, "detail", ""),
        )
    )


def _renew_supervised_claim(
    repo_root: Path,
    thread_id: str,
    log_path: Path,
    reported: dict[str, str],
    contract_cursors: dict[str, int],
) -> None:
    """Best-effort claim TTL renewal for the agent this supervisor owns."""
    if not thread_id:
        return
    from spice.agent.watchdog import publish_supervisor_feedback
    from spice.tasks import claimstate

    result = claimstate.renew_claim(actor=thread_id)
    if result.renewed:
        reported.pop("claim_renewal", None)
        try:
            _notice_contract_mutations(
                repo_root, thread_id, result, contract_cursors, log_path
            )
        except SpiceError as exc:
            _report_contract_watch_error(
                repo_root, result, log_path, reported, detail=str(exc)
            )
        else:
            reported.pop("contract_watch", None)
        return
    if result.reason in CLAIM_RENEWAL_QUIET_REASONS:
        return
    report_key = _claim_renewal_report_key(result)
    if reported.get("claim_renewal") == report_key:
        return
    reported["claim_renewal"] = report_key
    state = claimstate.claim_renewal_state(result)
    detail = f" detail={result.detail}" if result.detail else ""
    with log_path.open("a", encoding="utf-8") as log_handle:
        log_handle.write(
            f"spice claim renewal {state}: "
            f"reason={result.reason} handle={result.handle or '-'}{detail}\n"
        )
        log_handle.flush()
        publish_supervisor_feedback(
            repo_root,
            log_handle,
            f"claim.renewal-{state}",
            reason=result.reason,
            handle=result.handle,
            detail=result.detail,
        )


def _notice_contract_mutations(
    repo_root: Path,
    thread_id: str,
    result: Any,
    contract_cursors: dict[str, int],
    log_path: Path,
) -> None:
    """One renewal-cadence notice naming claimed-task contract fields that moved."""
    from spice.agent.watchdog import publish_supervisor_feedback
    from spice.tasks import opslog

    uuid = str(getattr(result, "uuid", "") or "")
    if not uuid:
        return
    if uuid not in contract_cursors:
        contract_cursors.clear()
        contract_cursors[uuid] = opslog.claim_baseline_id(uuid, thread_id)
    cursor, mutations = opslog.contract_mutations_since(uuid, contract_cursors[uuid])
    contract_cursors[uuid] = cursor
    if not mutations:
        return
    notice = opslog.render_notice(mutations)
    with log_path.open("a", encoding="utf-8") as log_handle:
        log_handle.write(f"spice claim contract changed: {result.handle} {notice}\n")
        log_handle.flush()
        publish_supervisor_feedback(
            repo_root,
            log_handle,
            "claim.contract-changed",
            handle=result.handle,
            fields=",".join(item.property for item in mutations),
            detail=notice,
        )


def _report_contract_watch_error(
    repo_root: Path,
    result: Any,
    log_path: Path,
    reported: dict[str, str],
    *,
    detail: str,
) -> None:
    """Publish one feedback item per distinct operations-log watch failure."""
    from spice.agent.watchdog import publish_supervisor_feedback

    if reported.get("contract_watch") == detail:
        return
    reported["contract_watch"] = detail
    with log_path.open("a", encoding="utf-8") as log_handle:
        log_handle.write(
            f"spice claim contract watch failed: {result.handle or '-'} {detail}\n"
        )
        log_handle.flush()
        publish_supervisor_feedback(
            repo_root,
            log_handle,
            "claim.contract-watch-error",
            handle=result.handle,
            detail=detail,
        )


def _watch_supervised_lane(
    repo_root: Path,
    thread_id: str,
    log_path: Path,
    process: subprocess.Popen[str],
    stop: Event,
) -> None:
    next_renewal = time.monotonic()
    reported: dict[str, str] = {}
    contract_cursors: dict[str, int] = {}
    while not stop.wait(SUPERVISOR_LANE_WATCH_SECONDS):
        if process.poll() is not None:
            return
        now = time.monotonic()
        try:
            if now >= next_renewal:
                _renew_supervised_claim(
                    repo_root, thread_id, log_path, reported, contract_cursors
                )
                next_renewal = now + SUPERVISOR_CLAIM_RENEWAL_SECONDS
            _flag_uncaptured_lane(repo_root, thread_id, log_path)
        except Exception:  # best-effort watch: never take down the supervisor
            pass


def _transition_agent_startup_state(
    repo_root: Path,
    process: subprocess.Popen[str],
    *,
    startup_status: str,
    ready_at: str = "",
    startup_failure: str = "",
) -> bool:
    """Update startup state only while this supervisor still owns the binding."""
    state = read_agent_state(repo_root)
    if state_int(state.get("pid")) != process.pid:
        return False
    state["startup_status"] = startup_status
    state["ready_at"] = ready_at
    state["startup_failure"] = startup_failure
    write_agent_state(repo_root, state)
    return True


def _watch_agent_startup(
    repo_root: Path,
    process: subprocess.Popen[str],
    log_path: Path,
    signal: AgentStartupSignal,
    stalled: Event,
    *,
    grace_seconds: float = FIRST_ACTIVITY_GRACE_SECONDS,
) -> None:
    outcome = signal.wait(grace_seconds)
    if outcome == "activity":
        if process.poll() is None:
            _transition_agent_startup_state(
                repo_root,
                process,
                startup_status=AGENT_STARTUP_READY,
                ready_at=utc_now(),
            )
        return
    if outcome == "finished":
        return
    if process.poll() is not None:
        return
    detail = (
        "agent startup stalled: no driver-defined first activity within "
        f"{grace_seconds:g}s"
    )
    with log_path.open("a", encoding="utf-8") as log_handle:
        log_handle.write(f"{detail}\n")
        log_handle.flush()
    stalled.set()
    try:
        terminate_process_group(process)
    finally:
        _transition_agent_startup_state(
            repo_root,
            process,
            startup_status=AGENT_STARTUP_STALLED,
            startup_failure=detail,
        )


def run_agent_supervisor(args: argparse.Namespace) -> int:
    from spice.agent.sidechannel import AgentSideChannelServer

    repo_root = Path(str(args.repo_root)).expanduser().resolve()
    log_path = Path(str(args.log_path)).expanduser()
    command = supervisor_command_from_json(str(args.command_json))
    prompt_skill_path = resolve_agent_prompt_skill_path(repo_root)
    env = agent_environment(repo_root)
    with AgentSideChannelServer(repo_root):
        started_at = utc_now()
        launch_clock = time.monotonic()
        started_thread_id = str(args.resume_thread_id or "")
        exit_code: int | None = None
        startup_stalled = Event()
        process, stdout_thread = spawn_supervised_agent(
            command,
            cwd=repo_root,
            log_path=log_path,
            env=env,
        )
        startup_signal = startup_signal_for_supervised_thread(stdout_thread)
        try:
            require_started_process(process, log_path, repo_root=repo_root)
            started_thread_id = started_agent_thread_id(
                log_path,
                repo_root=repo_root,
                fallback_thread_id=started_thread_id,
            )
            log_path = settle_agent_log_path(repo_root, log_path, started_thread_id)
            state = build_agent_state(
                process=process,
                action=str(args.action),
                command=command,
                driver=driver_for(repo_root).name,
                model=str(args.model),
                reasoning_effort=str(args.reasoning_effort),
                service_tier=str(args.service_tier or ""),
                thread_id=started_thread_id,
                prompt_skill_path=prompt_skill_path,
                log_path=log_path,
                fast_mode=bool(getattr(args, "fast_mode", False)),
                startup_status=AGENT_STARTUP_STARTING,
            )
            state["supervisor_pid"] = os.getpid()
            write_agent_state(repo_root, state)
            startup_watch = Thread(
                target=_watch_agent_startup,
                args=(
                    repo_root,
                    process,
                    log_path,
                    startup_signal,
                    startup_stalled,
                ),
                kwargs={"grace_seconds": FIRST_ACTIVITY_GRACE_SECONDS},
                name=f"spice-startup-watch-{started_thread_id or process.pid}",
                daemon=False,
            )
            startup_watch.start()
            stop_watch = Event()
            lane_watch = Thread(
                target=_watch_supervised_lane,
                args=(repo_root, started_thread_id, log_path, process, stop_watch),
                name=f"spice-lane-watch-{started_thread_id or process.pid}",
                daemon=True,
            )
            lane_watch.start()
            try:
                exit_code = process.wait()
            finally:
                stop_watch.set()
                startup_signal.note_finished()
                stdout_thread.join(timeout=1.0)
                startup_watch.join(timeout=STARTUP_WATCH_JOIN_SECONDS)
                lane_watch.join(timeout=1.0)
        finally:
            # Every supervised launch leaves a terminal outcome — including a
            # startup death, which otherwise only surfaces as a raised error —
            # so restart policy can see consecutive rapid deaths.
            record_launch_outcome(
                repo_root,
                supervised_launch_outcome(
                    repo_root,
                    thread_id=started_thread_id,
                    log_path=log_path,
                    started_at=started_at,
                    lifetime_seconds=time.monotonic() - launch_clock,
                    exit_code=process.poll() if exit_code is None else exit_code,
                    failure_kind=(
                        AGENT_FAILURE_STARTUP_STALLED
                        if startup_stalled.is_set()
                        else ""
                    ),
                ),
            )
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


def require_started_process(
    process: subprocess.Popen[str],
    log_path: Path,
    *,
    repo_root: Path | None = None,
) -> None:
    time.sleep(STARTUP_GRACE_SECONDS)
    exit_code = process.poll()
    if exit_code is None:
        return
    detail = tail_text(log_path, STARTUP_LOG_TAIL_BYTES)
    message = f"agent exited during startup with code {exit_code}"
    raise agent_startup_error(
        repo_root,
        exit_code=exit_code,
        message=message,
        detail=detail,
    )


def agent_startup_error(
    repo_root: Path | None,
    *,
    exit_code: int,
    message: str,
    detail: str,
) -> SpiceError:
    rendered = f"{message}: {detail}" if detail else message
    if (
        repo_root is not None
        and agent_process_failure_kind(repo_root, exit_code=exit_code, output=detail)
        == AGENT_FAILURE_OUT_OF_CREDITS
    ):
        return AgentOutOfCreditsError(rendered)
    return SpiceError(rendered)


def reap_process_when_done(
    process: subprocess.Popen[str], *, repo_root: Path | None = None
) -> None:
    def reap() -> None:
        process.wait()
        if repo_root is not None:
            touch_agent_state(repo_root)

    Thread(
        target=reap,
        name=f"spice-agent-reaper-{process.pid}",
        daemon=True,
    ).start()


def touch_agent_state(repo_root: Path) -> None:
    try:
        path = agent_state_path(repo_root)
        if path.exists():
            path.touch()
    except (OSError, SpiceError):
        pass


def skill_invocation_prompt(repo_root: Path, skill_path: Path) -> str:
    return driver_for(repo_root).skill_invocation_prompt(
        prompt_skill_invocation_path(repo_root, skill_path)
    )


def available_skill_path(repo_root: Path, *, required: bool) -> Path | None:
    return _available_skill_path(
        repo_root,
        required=required,
        packaged_path=packaged_skill_path(),
    )


def prompt_skill_invocation_path(repo_root: Path, skill_path: Path) -> Path:
    if not skill_path.is_absolute():
        return skill_path
    try:
        return skill_path.resolve().relative_to(repo_root.resolve())
    except ValueError:
        return skill_path


def import_agent(
    repo_root: Path, raw_thread_id: str, *, predecessor_thread: str = ""
) -> AgentStatus:
    """Bind this worktree to an externally-driven agent by thread id.

    The counterpart to :func:`bind_ambient_agent_activation` for an agent spice
    does not spawn: it writes the same worktree binding activation writes, but
    for a thread id the operator supplies (dashed or dashless) rather than the
    ambient environment. `spice agent show`, serve lanes, and task attribution then
    recognize the tree as driven by that agent. The binding owns no process, so
    it reads back idle -- spice tracks the agent without supervising it.

    `predecessor_thread` conveys lineage across a fresh worktree with nothing
    locally bound to inherit from (a forked conversation): it names the
    predecessor explicitly, taking precedence over any locally-resolved
    predecessor, so :func:`_carry_team_membership` still finds a team slot
    to carry forward.
    """
    thread_id = uuid_thread_id(raw_thread_id)
    if not thread_id:
        raise SpiceError(
            f"not a thread UUID: {raw_thread_id!r} -- expected dashed or "
            "dashless hex (e.g. f2249a9f-b996-41e2-9e18-54cb381cc634)"
        )
    running = agent_status(repo_root)
    if running.running:
        raise SpiceError(
            "refusing to import over the agent already running on this worktree "
            f"(thread {running.thread_id or '-'}, pid {running.pid}); "
            "stop it before importing another"
        )
    predecessor = canonical_thread_id(running.thread_id)
    if predecessor_thread.strip():
        explicit_predecessor = uuid_thread_id(predecessor_thread)
        if not explicit_predecessor:
            raise SpiceError(
                f"not a thread UUID: {predecessor_thread!r} -- expected dashed or "
                "dashless hex (e.g. f2249a9f-b996-41e2-9e18-54cb381cc634)"
            )
        predecessor = explicit_predecessor
    prompt_skill_path = available_skill_path(repo_root, required=False)
    driver = driver_for(repo_root).name
    write_agent_state(
        repo_root,
        {
            "pid": 0,
            "process_group_id": 0,
            "started_at": utc_now(),
            "mode": "import",
            "command": [],
            "driver": driver,
            "model": "",
            "reasoning_effort": "",
            "service_tier": "",
            "thread_id": thread_id,
            "prompt_skill_path": str(prompt_skill_path or ""),
            "log_path": "",
        },
    )
    _carry_team_membership(predecessor, thread_id, driver)
    return agent_status(repo_root)
