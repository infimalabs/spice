"""Worktree-bound agent lifecycle: ensure, supervise, status, activation.

One agent inhabits one worktree. `ensure` starts a fresh agent, resumes the
recorded thread, or — under renewal — forces a new successor; the launch is
serialized by an ensure-lock and recorded in durable state under
git-backed agent state. Runtime state always lives under this worktree's git
dir at `spice/agents/<driver>/`; once the real thread id is known, thread-owned
state and logs live under `spice/agents/<driver>/<thread-id>/` in that same
worktree git dir.
The facade delegates immutable launch decisions and blocking supervisor watches
to named seams while preserving its public compatibility surface.

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
from dataclasses import replace
from pathlib import Path
from threading import Event, Thread
from typing import Any, cast

from spice.agent.driver import (
    FAST_MODE_LAUNCH_KNOB,
    AgentDriver,
    PERSONALITY_LAUNCH_KNOB,
    driver_for,
)
from spice.agent.identity import canonical_thread_id, uuid_thread_id
from spice.agent.agentmodel import (
    AgentEnsureResult as AgentEnsureResult,
    AgentOutOfCreditsError as AgentOutOfCreditsError,
    AgentRestartRefusedError as AgentRestartRefusedError,
    LaunchClaim as LaunchClaim,
    PreparedLaunch as PreparedLaunch,
    agent_startup_error as agent_startup_error,
    attachable_thread_id as _attachable_thread_id,
    build_agent_state as build_agent_state,
    claimed_task_phase_launch as _claimed_task_phase_launch,
    next_agent_log_path as _next_agent_log_path,
    requested_launch_knobs as _requested_launch_knobs,
    supervisor_command_from_json as supervisor_command_from_json,
    touch_agent_state as touch_agent_state,
)
from spice.agent.startpreflight import (
    require_no_pending_authority_migration as _require_no_pending_authority_migration,
)
from spice.agent.promptskill import (
    available_skill_path as available_skill_path,
    prompt_skill_invocation_path as prompt_skill_invocation_path,
    resolve_agent_prompt_skill_path as resolve_agent_prompt_skill_path,
)
from spice.agent.paths import (
    agent_state_dir as agent_state_dir,
    agent_thread_state_dir,
)
from spice.agent.shadow import ensure_origin_head
from spice.agent.supervisorwatch import (
    CLAIM_RENEWAL_QUIET_REASONS as CLAIM_RENEWAL_QUIET_REASONS,
    CLAIM_RENEWAL_TERMINAL_REASONS as CLAIM_RENEWAL_TERMINAL_REASONS,
    LANE_UNCAPTURED_NUDGE as LANE_UNCAPTURED_NUDGE,
    SUPERVISOR_CLAIM_LEASE_SECONDS as SUPERVISOR_CLAIM_LEASE_SECONDS,
    SUPERVISOR_CLAIM_RENEWAL_SECONDS as SUPERVISOR_CLAIM_RENEWAL_SECONDS,
    SUPERVISOR_HEALTHY_CLAIM_LEASE_SECONDS as SUPERVISOR_HEALTHY_CLAIM_LEASE_SECONDS,
    SUPERVISOR_LANE_WATCH_SECONDS as SUPERVISOR_LANE_WATCH_SECONDS,
    SUPERVISOR_UNCAPTURED_NUDGE_SECONDS as SUPERVISOR_UNCAPTURED_NUDGE_SECONDS,
    SupervisorLaneSignal as SupervisorLaneSignal,
    flag_uncaptured_lane as _flag_uncaptured_lane_impl,
    notice_contract_mutations as _notice_contract_mutations,
    renew_held_claim as _renew_held_claim_impl,
    renew_supervised_claim as _renew_supervised_claim_impl,
    report_contract_watch_error as _report_contract_watch_error,
    supervised_claim_lease_seconds as _supervised_claim_lease_seconds_impl,
    transition_agent_startup_state as _transition_agent_startup_state,
    watch_agent_startup,
    watch_supervised_lane,
    worktree_dirty,
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
    SUPERVISOR_SCHEMA_VERSION_FIELD,
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
    bind_ambient_agent_thread,
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
    configured_agent_model_for_driver,
    configured_agent_personality,
)
from spice.errors import SpiceError
from spice.process.git import git_probe, git_read
from spice.process.groups import (
    PROCESS_GROUP_TERMINATION_BOUND_SECONDS,
    popen_new_process_group_kwargs,
    process_id_is_running,
    terminate_process_group,
)
from spice.serve.team.schema import TEAM_AUTHORITY_SCHEMA_VERSION
from spice.tasks.git import boundaries

STARTUP_GRACE_SECONDS = 0.25
SUPERVISOR_STARTUP_TIMEOUT_SECONDS = 3.0
FIRST_ACTIVITY_GRACE_SECONDS = 120.0
# A resume that must compact first cannot produce activity until the compaction
# returns, and a large transcript takes far longer than the first-activity
# grace. Killing it mid-compaction aborts the compaction, so the transcript
# stays oversized and the next launch compacts into the same kill -- the window
# is generous because escaping that loop matters more than detecting a wedged
# compaction quickly, and it stays bounded so a truly stuck one still reports.
COMPACTING_GRACE_SECONDS = 900.0
STARTUP_STATE_PERSISTENCE_ALLOWANCE_SECONDS = 3.0
STARTUP_WATCH_JOIN_SECONDS = (
    PROCESS_GROUP_TERMINATION_BOUND_SECONDS
    + STARTUP_STATE_PERSISTENCE_ALLOWANCE_SECONDS
)
AGENT_FAILURE_OUT_OF_CREDITS = "out-of-credits"
AGENT_FAILURE_RESTART_REFUSED = "restart-refused"
AGENT_FAILURE_STARTUP_STALLED = AGENT_STARTUP_STALLED
AGENT_FAILURE_CONFIG_APPROVAL_REQUIRED = "config-approval-required"


def _worktree_dirty(repo_root: Path) -> bool:
    return worktree_dirty(repo_root)


def _flag_uncaptured_lane(repo_root: Path, thread_id: str, log_path: Path) -> None:
    _flag_uncaptured_lane_impl(
        repo_root,
        thread_id,
        log_path,
        dirty=_worktree_dirty,
    )


def _supervised_claim_lease_seconds(repo_root: Path, thread_id: str) -> float:
    return _supervised_claim_lease_seconds_impl(
        repo_root,
        thread_id,
        read_state=read_agent_state,
    )


def _renew_held_claim(
    repo_root: Path,
    thread_id: str,
    held: dict[str, str],
) -> Any:
    return _renew_held_claim_impl(
        repo_root,
        thread_id,
        held,
        lease_resolver=_supervised_claim_lease_seconds,
    )


def _renew_supervised_claim(
    repo_root: Path,
    thread_id: str,
    log_path: Path,
    reported: dict[str, str],
    contract_cursors: dict[str, int],
    held: dict[str, str],
) -> None:
    _renew_supervised_claim_impl(
        repo_root,
        thread_id,
        log_path,
        reported,
        contract_cursors,
        held,
        renew_held=_renew_held_claim,
        notice=_notice_contract_mutations,
        report=_report_contract_watch_error,
    )


def next_agent_log_path(repo_root: Path) -> Path:
    return _next_agent_log_path(repo_root, timestamp=utc_now())


def agent_ensure_lock(repo_root: Path):
    return _agent_ensure_lock(
        repo_root, timeout_seconds=AGENT_ENSURE_LOCK_TIMEOUT_SECONDS
    )


def _refuse_restart_after_rapid_deaths(repo_root: Path) -> None:
    """Raise when launches keep dying young and this wake is automatic.

    Only automatic wake paths honor the refusal; an explicit operator start is
    itself the grant of exactly one new attempt, and the journal it leaves
    behind re-arms the refusal if that attempt also dies young.
    """
    refusal = launch_refusal(repo_root)
    if refusal is None:
        return
    raise AgentRestartRefusedError(
        "automatic restart refused: "
        f"{refusal['consecutive_rapid_deaths']} consecutive launches "
        f"died within {RAPID_DEATH_LIFETIME_SECONDS:g}s; "
        f"holding until epoch {refusal['hold_until_epoch']} "
        "unless an operator starts the agent explicitly",
        refusal=refusal,
    )


def preflight_automatic_agent_launch(repo_root: Path) -> boundaries.SyncResult:
    """Refresh and validate a lane before Serve reserves work for it.

    Available-work dispatch used to claim first and discover launch refusals
    afterward. A repository fast-forward can change executable configuration,
    so that ordering repeatedly assigned and released the same task while each
    failed spawn left an empty pre-supervisor log. Run the same safe launch
    refresh first, then render the shell configuration the supervisor will
    install. The render is the exact executable-config approval boundary; a
    refusal therefore reaches Serve before any task or launch state changes.
    """
    if (
        git_probe(repo_root, "rev-parse", "--is-inside-work-tree").stdout.strip()
        != "true"
    ):
        return boundaries.SyncResult(notes=["skipped:not-a-worktree"])
    _require_no_pending_authority_migration(repo_root)
    ensure_origin_head(repo_root)
    sync = boundaries.fast_forward_if_safe(repo_root)
    from spice.agent.shellhook import render_shell_runtime_wrapper_lines

    render_shell_runtime_wrapper_lines(repo_root)
    return sync


def _prepare_launch(
    driver: AgentDriver,
    repo_root: Path,
    status: AgentStatus,
    *,
    prompt: str,
    force_new: bool,
    model: str,
    reasoning_effort: str,
    personality: str | None,
    agent_bin: str,
    fast_mode: bool,
) -> PreparedLaunch:
    """Resolve the command and every value that decided it.

    Model and effort resolve in one order: explicit argument, then the claimed
    task's phase mapping for this driver, then the effective three-layer
    configuration, then the driver's shipped default. Personality and fast mode
    resolve instead against what the driver declares it honors, so a knob
    without a launch-time seam stops here, in the open.
    """
    resume_thread_id = _attachable_thread_id(
        driver, repo_root, "" if force_new else status.thread_id
    )
    resolved_personality = personality or configured_agent_personality(repo_root)
    honors = driver.honored_launch_knobs
    unhonored = driver.unhonored_launch_knobs(
        _requested_launch_knobs(
            personality=personality,
            resolved_personality=resolved_personality,
            fast_mode=fast_mode,
        )
    )
    launch_personality = (
        resolved_personality if PERSONALITY_LAUNCH_KNOB in honors else ""
    )
    launch_fast_mode = fast_mode and FAST_MODE_LAUNCH_KNOB in honors
    phase_launch = _claimed_task_phase_launch(repo_root, driver.name, status)
    model = driver.resolve_model(
        model
        or phase_launch.get("model", "")
        or configured_agent_model_for_driver(repo_root, driver.name)
    )
    reasoning_effort = (
        reasoning_effort
        or phase_launch.get("effort", "")
        or configured_agent_effort(repo_root)
        or driver.default_reasoning_effort
    )
    return PreparedLaunch(
        action="renew" if force_new else ("resume" if resume_thread_id else "start"),
        command=driver.build_exec_command(
            repo_root=repo_root,
            prompt=prompt,
            thread_id=resume_thread_id,
            model=model,
            reasoning_effort=reasoning_effort,
            personality=launch_personality,
            binary=agent_bin,
            fast_mode=launch_fast_mode,
        ),
        resume_thread_id=resume_thread_id,
        model=model,
        reasoning_effort=reasoning_effort,
        fast_mode=launch_fast_mode,
        unhonored_knobs=unhonored,
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
    launch_claim: LaunchClaim | None = None,
    launch_preflighted: bool = False,
) -> AgentEnsureResult:
    if launch_claim is not None and not supervise_stdout:
        raise SpiceError(
            "a launch claim rides the supervisor: an unsupervised launch never "
            f"reports the startup failure that releases {launch_claim.uuid}"
        )
    resolved_root = repo_root.resolve()
    with agent_ensure_lock(resolved_root):
        status = agent_status(resolved_root)
        if not status.running:
            _require_no_pending_authority_migration(resolved_root)
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
        if automatic:
            _refuse_restart_after_rapid_deaths(resolved_root)
        launch = _prepare_launch(
            driver,
            resolved_root,
            status,
            prompt=prompt,
            force_new=force_new,
            model=model,
            reasoning_effort=reasoning_effort,
            personality=personality,
            agent_bin=agent_bin,
            fast_mode=fast_mode,
        )
        if dry_run:
            return AgentEnsureResult(
                action=f"would-{launch.action}",
                status=status,
                command=launch.command,
                prompt=prompt,
                log_path=None,
                unhonored_launch_knobs=launch.unhonored_knobs,
            )
        ensure_origin_head(resolved_root)
        log_path = start_agent(
            resolved_root,
            action=launch.action,
            command=launch.command,
            model=launch.model,
            reasoning_effort=launch.reasoning_effort,
            resume_thread_id=launch.resume_thread_id,
            prompt_skill_path=prompt_skill_path,
            fast_mode=launch.fast_mode,
            supervise_stdout=supervise_stdout,
            launch_claim=launch_claim,
            sync_before_start=not launch_preflighted,
        )
        return AgentEnsureResult(
            action=launch.action,
            status=agent_status(resolved_root),
            command=launch.command,
            prompt=prompt,
            log_path=log_path,
            unhonored_launch_knobs=launch.unhonored_knobs,
        )


def start_agent(
    repo_root: Path,
    *,
    action: str,
    command: list[str],
    model: str,
    reasoning_effort: str,
    resume_thread_id: str,
    prompt_skill_path: Path,
    fast_mode: bool,
    supervise_stdout: bool,
    launch_claim: LaunchClaim | None,
    sync_before_start: bool = True,
) -> Path:
    # This shared boundary covers both launch modes. It intentionally runs in
    # the globally installed parent before the detached ``python -m spice``
    # supervisor can import the worktree checkout. It is the same opportunistic
    # fast-forward-quiet-advance activation uses: it never raises and never
    # mangles the tree, so an unsafe checkout still launches the agent that can
    # reconcile it rather than dying pre-start.
    if sync_before_start:
        boundaries.fast_forward_if_safe(repo_root)
    log_path = next_agent_log_path(repo_root)
    if supervise_stdout:
        supervisor = spawn_agent_supervisor(
            repo_root,
            action=action,
            command=command,
            model=model,
            reasoning_effort=reasoning_effort,
            resume_thread_id=resume_thread_id,
            log_path=log_path,
            fast_mode=fast_mode,
            launch_claim=launch_claim,
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
            thread_id=started_thread_id,
            prompt_skill_path=prompt_skill_path,
            log_path=log_path,
            fast_mode=fast_mode,
        ),
    )
    reap_process_when_done(process, repo_root=repo_root)
    return log_path


def spawn_agent_supervisor(
    repo_root: Path,
    *,
    action: str,
    command: list[str],
    model: str,
    reasoning_effort: str,
    resume_thread_id: str,
    log_path: Path,
    fast_mode: bool,
    launch_claim: LaunchClaim | None,
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
        "--resume-thread-id",
        resume_thread_id,
        "--log-path",
        str(log_path),
        "--command-json",
        json.dumps(command, separators=(",", ":")),
        "--launch-claim-uuid",
        launch_claim.uuid if launch_claim else "",
        "--launch-claim-actor",
        launch_claim.actor if launch_claim else "",
    ]
    if fast_mode:
        supervisor_command.append("--fast-mode")
    environment = agent_supervisor_environment(repo_root)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_handle = log_path.open("ab")
    try:
        process = subprocess.Popen(
            supervisor_command,
            cwd=repo_root,
            stdin=subprocess.DEVNULL,
            stdout=log_handle,
            stderr=subprocess.STDOUT,
            env=environment,
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


def _watch_supervised_lane(
    repo_root: Path,
    thread_id: str,
    log_path: Path,
    process: subprocess.Popen[str],
    lane_signal: SupervisorLaneSignal,
) -> None:
    watch_supervised_lane(
        repo_root,
        thread_id,
        log_path,
        process,
        lane_signal,
        renew=_renew_supervised_claim,
        flag=_flag_uncaptured_lane,
    )


def _watch_agent_startup(
    repo_root: Path,
    process: subprocess.Popen[str],
    log_path: Path,
    signal: AgentStartupSignal,
    stalled: Event,
    *,
    grace_seconds: float = FIRST_ACTIVITY_GRACE_SECONDS,
    compacting_seconds: float = COMPACTING_GRACE_SECONDS,
) -> None:
    watch_agent_startup(
        repo_root,
        process,
        log_path,
        signal,
        stalled,
        grace_seconds=grace_seconds,
        compacting_seconds=compacting_seconds,
        transition=_transition_agent_startup_state,
        terminate=terminate_process_group,
    )


def launch_claim_from_args(args: argparse.Namespace) -> LaunchClaim | None:
    """The reservation this supervised launch must hand back if it never starts."""
    uuid = str(args.launch_claim_uuid or "")
    actor = str(args.launch_claim_actor or "")
    if not uuid and not actor:
        return None
    if not uuid or not actor:
        raise SpiceError(
            "a launch claim names both the task and its owner: "
            f"got uuid={uuid or '-'} actor={actor or '-'}"
        )
    return LaunchClaim(uuid=uuid, actor=actor)


def _release_unready_launch_claim(
    repo_root: Path,
    process: subprocess.Popen[str],
    launch_claim: LaunchClaim | None,
    log_path: Path,
) -> str:
    """Hand back the task this launch reserved when it never reached readiness.

    The reservation is taken before the process exists so no peer can take the
    row out from under a starting lane, and the supervisor renews it only while
    its child lives. A launch that dies before first activity would therefore
    hold a READY task for a whole lease with nobody left to work it; releasing
    it on the same event that ends the launch puts the row back on the board
    immediately. Two launches are deliberately left alone: one that reached
    `ready` may have left work on disk under that claim, and one whose state
    binding already moved to another pid no longer speaks for the lane. Deaths
    before the state is published belong to the parent ensure instead -- it is
    still inside `require_supervisor_started` and sees the exit itself.

    Cleanup runs on the terminal path a startup failure already travels, so it
    reports its own failures rather than raising over the launch's. Which side
    of the handback a failure lands on decides how it reads: before the row goes
    back the reservation really does survive, while after it the row is already
    allocatable and only the witness file is stale, so that outcome names the
    release it earned and carries the write fault beside it.
    """
    if launch_claim is None:
        return ""
    try:
        from spice.tasks import claimstate

        state = read_agent_state(repo_root)
        if state_int(state.get("pid")) != process.pid:
            return ""
        if str(state.get("startup_status") or "") == AGENT_STARTUP_READY:
            return ""
        result = claimstate.release_claim(launch_claim.uuid, launch_claim.actor)
    except Exception as exc:
        # The state read is filesystem I/O and the release can still raise on a
        # Taskwarrior fault. This terminal cleanup boundary must preserve the
        # launch's own outcome even when those fail outside SpiceError, and
        # nothing reached the board, so the reservation genuinely stands.
        _note_launch_claim_outcome(log_path, launch_claim, f"kept: {exc}")
        return ""
    if not result.released:
        _note_launch_claim_outcome(log_path, launch_claim, "kept: owned elsewhere now")
        return ""
    outcome = "released"
    if result.witness_error:
        outcome = f"released, witness unwritten: {result.witness_error}"
    _note_launch_claim_outcome(log_path, launch_claim, outcome)
    return launch_claim.uuid


def _note_launch_claim_outcome(
    log_path: Path, launch_claim: LaunchClaim, outcome: str
) -> None:
    """Leave the reservation's fate beside this launch's own failure evidence."""
    try:
        with log_path.open("a", encoding="utf-8") as log_handle:
            log_handle.write(
                f"spice launch claim {outcome}: {launch_claim.uuid} "
                f"reserved for {launch_claim.actor}\n"
            )
            log_handle.flush()
    except OSError:
        pass


def run_agent_supervisor(args: argparse.Namespace) -> int:
    from spice.agent.sidechannel import AgentSideChannelServer

    repo_root = Path(str(args.repo_root)).expanduser().resolve()
    log_path = Path(str(args.log_path)).expanduser()
    command = supervisor_command_from_json(str(args.command_json))
    launch_claim = launch_claim_from_args(args)
    prompt_skill_path = resolve_agent_prompt_skill_path(repo_root)
    env = agent_environment(repo_root)
    lane_signal = SupervisorLaneSignal()

    with AgentSideChannelServer(repo_root, on_claim=lane_signal.notify):
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
                thread_id=started_thread_id,
                prompt_skill_path=prompt_skill_path,
                log_path=log_path,
                fast_mode=bool(getattr(args, "fast_mode", False)),
                startup_status=AGENT_STARTUP_STARTING,
            )
            state["supervisor_pid"] = os.getpid()
            # Stamped beside the pid because it describes the same process: this
            # supervisor will write the team authority store at this version for
            # its whole life, whatever the deployment moves to underneath it.
            state[SUPERVISOR_SCHEMA_VERSION_FIELD] = TEAM_AUTHORITY_SCHEMA_VERSION
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
                kwargs={
                    "grace_seconds": FIRST_ACTIVITY_GRACE_SECONDS,
                    "compacting_seconds": COMPACTING_GRACE_SECONDS,
                },
                name=f"spice-startup-watch-{started_thread_id or process.pid}",
                daemon=False,
            )
            startup_watch.start()
            lane_watch = Thread(
                target=_watch_supervised_lane,
                args=(repo_root, started_thread_id, log_path, process, lane_signal),
                name=f"spice-lane-watch-{started_thread_id or process.pid}",
                daemon=True,
            )
            lane_watch.start()
            try:
                exit_code = process.wait()
            finally:
                lane_signal.stop()
                startup_signal.note_finished()
                stdout_thread.join(timeout=1.0)
                startup_watch.join(timeout=STARTUP_WATCH_JOIN_SECONDS)
                lane_watch.join(timeout=1.0)
        finally:
            if process.poll() is None:
                terminate_process_group(process)
            released_claim = _release_unready_launch_claim(
                repo_root, process, launch_claim, log_path
            )
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
                    released_claim=released_claim,
                ),
            )
    return int(exit_code or 0)


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


def skill_invocation_prompt(repo_root: Path, skill_path: Path) -> str:
    return driver_for(repo_root).skill_invocation_prompt(
        prompt_skill_invocation_path(repo_root, skill_path)
    )


def import_agent(
    repo_root: Path, raw_thread_id: str, *, predecessor_thread: str = ""
) -> AgentStatus:
    """Bind this worktree to an externally-driven agent by thread id.

    The counterpart to :func:`bind_ambient_agent_thread` for an agent spice
    does not spawn: it writes the same worktree binding the hook points write,
    but for a thread id the operator supplies (dashed or dashless) rather than
    the ambient environment. `spice agent show`, serve lanes, and task attribution then
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
            "thread_id": thread_id,
            "prompt_skill_path": str(prompt_skill_path or ""),
            "log_path": "",
        },
    )
    from spice.tasks import claimstate

    claim_carry = claimstate.carry_claim(
        predecessor,
        thread_id,
        site=claimstate.ClaimSite(
            repo_root.resolve(),
            git_read(repo_root, "branch", "--show-current"),
            git_read(repo_root, "rev-parse", "HEAD"),
        ),
    )
    _carry_team_membership(predecessor, thread_id, driver)
    return replace(
        agent_status(repo_root),
        claim_carry=claimstate.claim_carry_status_line(claim_carry),
    )
