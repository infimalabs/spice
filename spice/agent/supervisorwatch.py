"""Supervisor-owned claim renewal, lane observation, and startup watching."""

from __future__ import annotations

import subprocess
import time
from collections.abc import Callable
from dataclasses import replace
from pathlib import Path
from threading import Condition, Event
from typing import Any

from spice.agent.identity import canonical_thread_id
from spice.agent.lifecyclebinding import (
    AGENT_STARTUP_READY,
    AGENT_STARTUP_STALLED,
    read_agent_state,
    state_int,
    utc_now,
    write_agent_state,
)
from spice.agent.watchdog import AgentStartupSignal
from spice.errors import SpiceError
from spice.process.git import git_probe

SUPERVISOR_LANE_WATCH_SECONDS = 20.0
SUPERVISOR_UNCAPTURED_NUDGE_SECONDS = 45.0
SUPERVISOR_CLAIM_RENEWAL_SECONDS = SUPERVISOR_LANE_WATCH_SECONDS
SUPERVISOR_CLAIM_LEASE_SECONDS = 3.0 * SUPERVISOR_CLAIM_RENEWAL_SECONDS
SUPERVISOR_HEALTHY_CLAIM_LEASE_SECONDS = 5.0 * SUPERVISOR_CLAIM_RENEWAL_SECONDS
LANE_UNCAPTURED_NUDGE = (
    "your worktree has uncommitted or uncaptured changes but you hold no "
    "claimed task -- work cannot land without one. Claim a task before "
    "editing further, or fold the changes in with spice task capture."
)
CLAIM_RENEWAL_QUIET_REASONS = frozenset({"no_active_claim"})
CLAIM_RENEWAL_TERMINAL_REASONS = frozenset(
    {
        "claim_ended",
        "claimed_by_other",
        "completed",
        "deleted",
        "different_worktree",
        "missing",
    }
)


class SupervisorLaneSignal:
    """One blocking wakeup surface for claim events, cadence, and shutdown."""

    def __init__(self) -> None:
        self._condition = Condition()
        self._generation = 0
        self._observed_generation = 0
        self._stopped = False

    def notify(self) -> None:
        with self._condition:
            self._generation += 1
            self._condition.notify_all()

    def stop(self) -> None:
        with self._condition:
            self._stopped = True
            self._condition.notify_all()

    def wait_for_event(self, timeout: float) -> bool:
        """Block until an event or timeout; return whether shutdown was requested."""
        with self._condition:
            if self._generation != self._observed_generation:
                self._observed_generation = self._generation
                return self._stopped
            self._condition.wait_for(
                lambda: self._stopped or self._generation != self._observed_generation,
                timeout=max(0.0, timeout),
            )
            self._observed_generation = self._generation
            return self._stopped


def worktree_dirty(repo_root: Path) -> bool:
    """Return the safe supervisor answer from a bounded Git dirtiness probe."""
    result = git_probe(repo_root, "status", "--porcelain")
    return result.returncode == 0 and result.stdout.strip() != ""


def flag_uncaptured_lane(
    repo_root: Path,
    thread_id: str,
    log_path: Path,
    *,
    dirty: Callable[[Path], bool] = worktree_dirty,
) -> None:
    """Surface a nudge when the bound agent holds no task but the tree is dirty."""
    from spice.agent.watchdog import publish_supervisor_feedback
    from spice.tasks.claimstate import active_claim

    if not thread_id or active_claim(thread_id) is not None:
        return
    if not dirty(repo_root):
        return
    with log_path.open("a", encoding="utf-8") as log_handle:
        publish_supervisor_feedback(
            repo_root, log_handle, "lane.uncaptured", message=LANE_UNCAPTURED_NUDGE
        )


def claim_renewal_report_key(result: Any) -> str:
    return "\0".join(
        str(part)
        for part in (
            getattr(result, "reason", ""),
            getattr(result, "handle", ""),
            getattr(result, "detail", ""),
        )
    )


def supervised_claim_lease_seconds(
    repo_root: Path,
    thread_id: str,
    *,
    read_state: Callable[[Path], dict[str, Any]] = read_agent_state,
) -> float:
    """Keep startup claims short, then promote the confirmed healthy holder."""
    state = read_state(repo_root)
    state_thread_id = canonical_thread_id(state.get("thread_id"))
    if (
        state_thread_id == canonical_thread_id(thread_id)
        and str(state.get("startup_status") or "") == AGENT_STARTUP_READY
    ):
        return SUPERVISOR_HEALTHY_CLAIM_LEASE_SECONDS
    return SUPERVISOR_CLAIM_LEASE_SECONDS


def renew_held_claim(
    repo_root: Path,
    thread_id: str,
    held: dict[str, str],
    *,
    lease_resolver: Callable[[Path, str], float] = supervised_claim_lease_seconds,
) -> Any:
    """Renew the exact row last held, preserving terminal ownership evidence."""
    from spice.tasks import claimstate

    try:
        lease_seconds = lease_resolver(repo_root, thread_id)
        witness = claimstate.read_claim_witness(repo_root, thread_id)
    except (OSError, SpiceError) as exc:
        return claimstate.ClaimRenewalResult(
            False,
            "backend_error",
            handle=held.get("handle", ""),
            detail=str(exc),
            uuid=held.get("uuid", ""),
        )
    if witness is not None:
        held.clear()
        if witness.active:
            held.update({"handle": witness.handle, "uuid": witness.uuid})
    target = held.get("handle") or held.get("uuid", "")
    result = claimstate.renew_claim(
        handle=target or None,
        actor=thread_id,
        lease_seconds=lease_seconds,
    )
    if target and not result.renewed and result.reason == "no_active_claim":
        result = claimstate.renew_claim(
            actor=thread_id,
            lease_seconds=lease_seconds,
        )
        if not result.renewed and result.reason == "no_active_claim":
            result = replace(
                result,
                reason="claim_ended",
                handle=held.get("handle", ""),
                uuid=held.get("uuid", ""),
            )
    if result.renewed:
        held.clear()
        held.update({"handle": result.handle, "uuid": result.uuid})
    elif target and not result.uuid:
        result = replace(
            result,
            handle=result.handle or held.get("handle", ""),
            uuid=held.get("uuid", ""),
        )
    return result


def renew_supervised_claim(
    repo_root: Path,
    thread_id: str,
    log_path: Path,
    reported: dict[str, str],
    contract_cursors: dict[str, int],
    held: dict[str, str],
    *,
    renew_held: Callable[[Path, str, dict[str, str]], Any] = renew_held_claim,
    notice: Callable[..., None] | None = None,
    report: Callable[..., None] | None = None,
) -> None:
    """Best-effort claim TTL renewal for the agent this supervisor owns."""
    if not thread_id:
        return
    from spice.agent.watchdog import publish_supervisor_feedback
    from spice.tasks import claimstate

    notice_callback = notice or notice_contract_mutations
    report_callback = report or report_contract_watch_error
    result = renew_held(repo_root, thread_id, held)
    if result.renewed:
        reported.pop("claim_renewal", None)
        try:
            notice_callback(repo_root, thread_id, result, contract_cursors, log_path)
        except SpiceError as exc:
            report_callback(repo_root, result, log_path, reported, detail=str(exc))
        else:
            reported.pop("contract_watch", None)
        return
    if result.reason in CLAIM_RENEWAL_QUIET_REASONS:
        return
    report_key = claim_renewal_report_key(result)
    if reported.get("claim_renewal") != report_key:
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
    if result.reason in CLAIM_RENEWAL_TERMINAL_REASONS and result.uuid:
        claimstate.retire_claim_witness(
            repo_root,
            thread_id,
            uuid=result.uuid,
            handle=result.handle,
        )
        held.clear()


def notice_contract_mutations(
    repo_root: Path,
    thread_id: str,
    result: Any,
    contract_cursors: dict[str, int],
    log_path: Path,
) -> None:
    """Publish one renewal-cadence notice for claimed-task contract changes."""
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


def report_contract_watch_error(
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


def watch_supervised_lane(
    repo_root: Path,
    thread_id: str,
    log_path: Path,
    process: subprocess.Popen[str],
    lane_signal: SupervisorLaneSignal,
    *,
    renew: Callable[..., None],
    flag: Callable[[Path, str, Path], None],
) -> None:
    next_uncaptured_nudge = time.monotonic()
    reported: dict[str, str] = {}
    contract_cursors: dict[str, int] = {}
    held: dict[str, str] = {}
    while True:
        if process.poll() is not None:
            return
        now = time.monotonic()
        try:
            renew(repo_root, thread_id, log_path, reported, contract_cursors, held)
            if now >= next_uncaptured_nudge:
                flag(repo_root, thread_id, log_path)
                next_uncaptured_nudge = now + SUPERVISOR_UNCAPTURED_NUDGE_SECONDS
        except Exception:
            pass
        idle = SUPERVISOR_CLAIM_RENEWAL_SECONDS - (time.monotonic() - now)
        if lane_signal.wait_for_event(max(0.0, idle)):
            return


def transition_agent_startup_state(
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


def watch_agent_startup(
    repo_root: Path,
    process: subprocess.Popen[str],
    log_path: Path,
    signal: AgentStartupSignal,
    stalled: Event,
    *,
    grace_seconds: float,
    compacting_seconds: float,
    transition: Callable[..., bool],
    terminate: Callable[[subprocess.Popen[str]], None],
) -> None:
    """Publish readiness or terminate a supervised child that never starts."""
    outcome = signal.wait(grace_seconds, compacting_seconds=compacting_seconds)
    if outcome == "activity":
        if process.poll() is None:
            transition(
                repo_root,
                process,
                startup_status=AGENT_STARTUP_READY,
                ready_at=utc_now(),
            )
        return
    if outcome == "finished" or process.poll() is not None:
        return
    if outcome == "compacting-timeout":
        detail = (
            "agent startup stalled: compaction never settled within "
            f"{compacting_seconds:g}s"
        )
    else:
        detail = (
            "agent startup stalled: no driver-defined first activity within "
            f"{grace_seconds:g}s"
        )
    with log_path.open("a", encoding="utf-8") as log_handle:
        log_handle.write(f"{detail}\n")
        log_handle.flush()
    stalled.set()
    try:
        terminate(process)
    finally:
        transition(
            repo_root,
            process,
            startup_status=AGENT_STARTUP_STALLED,
            startup_failure=detail,
        )
