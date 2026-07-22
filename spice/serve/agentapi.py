"""Agent lifecycle endpoints for the serve UI: status, ensure, renewal."""

from __future__ import annotations

import subprocess
import threading
import time
from http import HTTPStatus
from collections.abc import Callable
from typing import Any, Sequence

from spice.agent.driver import driver_for
from spice.agent.identity import canonical_thread_id
from spice.agent.lifecycle import (
    AGENT_FAILURE_OUT_OF_CREDITS,
    AGENT_FAILURE_RESTART_REFUSED,
    AgentOutOfCreditsError,
    AgentRestartRefusedError,
    SUPERVISOR_CLAIM_LEASE_SECONDS,
    agent_binding_error,
    agent_status,
    ensure_agent,
    launch_refusal,
)
from spice.mail.inbox import (
    deadletter_inbox_item,
    inbox_item_key,
    inbox_request_priority,
    pending_operator_inbox_items,
)
from spice.process.git import git_read
from spice.serve.attachments import inbox_attachment_payloads
from spice.serve.markdown import render_message_html
from spice.serve.pending import pending_inbox_identity_payload
from spice.serve.payload.wire import validate_emitter_payload
from spice.serve.steering import SentSteeringMessage
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import alloc, claimstate, identity

PENDING_AGENT_ENSURE_RETRY_SECONDS = 5.0
AVAILABLE_WORK_ENSURE_RETRY_SECONDS = 5.0
# Before a start there must be one ready task for the new lane and one more
# still available than the number of lanes already running.
AVAILABLE_WORK_READY_TASK_MARGIN = 2
AVAILABLE_WORK_STARVATION_SECONDS = 5.0 * 60.0
# Capacity, candidate selection, claim, and startup are one serialized decision:
# another inventory refresh must observe the started lane before it can expand.
_AVAILABLE_WORK_CLAIM_LOCK = threading.RLock()


def agent_status_payload(target: WorktreeTarget) -> dict[str, Any]:
    status = agent_status(target.repo_root)
    binding_error = agent_binding_error(target.repo_root, status)
    payload = {
        "ok": True,
        "provider": driver_for(target.repo_root).name,
        "workTreeId": target.id,
        "status": status.process_status,
        "pid": status.pid or 0,
        "processGroupId": status.process_group_id or 0,
        "threadId": status.thread_id,
        "model": status.model,
        "effort": status.reasoning_effort,
        "serviceTier": status.service_tier,
        "readyAt": str(getattr(status, "ready_at", "") or ""),
        "startupFailure": str(getattr(status, "startup_failure", "") or ""),
        "launchable": not status.running,
        "bindingStatus": "mismatch"
        if binding_error
        else ("bound" if status.thread_id else "unbound"),
        "bindingError": binding_error,
        "restartRefusal": launch_refusal(target.repo_root) or {},
    }
    return validate_emitter_payload("agentapi.agent_status_payload", payload)


def agent_ensure_response_payload(
    target: WorktreeTarget,
    *,
    force_new: bool = False,
    fast_mode: bool = False,
    automatic: bool = False,
) -> tuple[dict[str, Any], HTTPStatus]:
    try:
        result = ensure_agent(
            target.repo_root,
            force_new=force_new,
            fast_mode=fast_mode,
            supervise_stdout=True,
            automatic=automatic,
        )
    except AgentRestartRefusedError as exc:
        return (
            {
                "ok": False,
                "failure": AGENT_FAILURE_RESTART_REFUSED,
                "error": f"Could not ensure agent: {exc}",
                "restartRefusal": exc.refusal,
            },
            HTTPStatus.TOO_MANY_REQUESTS,
        )
    except AgentOutOfCreditsError as exc:
        return (
            {
                "ok": False,
                "failure": AGENT_FAILURE_OUT_OF_CREDITS,
                "error": f"Could not ensure agent: {exc}",
            },
            HTTPStatus.PAYMENT_REQUIRED,
        )
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        return (
            {"ok": False, "error": f"Could not ensure agent: {exc}"},
            HTTPStatus.INTERNAL_SERVER_ERROR,
        )
    return agent_ensure_payload(result), HTTPStatus.OK


def agent_ensure_payload(result: Any) -> dict[str, Any]:
    return {
        "ok": True,
        "provider": driver_for(result.status.repo_root).name,
        "action": result.action,
        "status": result.status.process_status,
        "pid": result.status.pid or 0,
        "processGroupId": result.status.process_group_id or 0,
        "threadId": result.status.thread_id,
        "serviceTier": result.status.service_tier,
        "readyAt": str(getattr(result.status, "ready_at", "") or ""),
        "startupFailure": str(getattr(result.status, "startup_failure", "") or ""),
        "prompt": result.prompt,
        "logPath": str(result.log_path) if result.log_path else "",
    }


def sent_steering_payload(
    sent: SentSteeringMessage,
    *,
    target: WorktreeTarget | None,
    agent_ensure_override: dict[str, Any] | None = None,
    pending_count: int | None = None,
    pending_identity: dict[str, Any] | None = None,
) -> dict[str, Any]:
    payload = {
        "ok": True,
        "key": sent.key,
        "path": str(sent.path),
        "text": sent.text,
        "requestText": sent.request_text,
        "requestControls": list(sent.request_controls),
        "requestPriority": inbox_request_priority(sent.text) or "",
        "requestHtml": render_message_html(sent.request_text),
        "noSay": sent.no_say,
        "attachments": inbox_attachment_payloads(
            sent.attachments,
            repo_root=target.repo_root if target else None,
            worktree_id=target.id if target else None,
        ),
        "agentEnsure": agent_ensure_override or {},
    }
    if pending_identity is not None:
        payload.update(pending_identity)
    elif pending_count is not None:
        payload["pendingInboxCount"] = pending_count
        payload["pendingInboxLabel"] = str(pending_count)
    return payload


def sent_steering_response_payload(
    sent: SentSteeringMessage,
    *,
    state: Any,
    target: WorktreeTarget | None,
    fast_mode: bool = False,
    force_new: bool = False,
) -> dict[str, Any]:
    if target is None:
        return sent_steering_payload(sent, target=None)
    agent_ensure = ensure_agent_for_pending_inbox(
        target,
        attempt_cache=state.pending_agent_ensure_attempts,
        retry_seconds=0.0,
        fast_mode=fast_mode,
        force_new=force_new,
        # A fresh operator send is an explicit action: it grants exactly one
        # launch attempt even while automatic restarts are refused.
        automatic=False,
    )
    pending_identity = pending_inbox_identity_payload(target.repo_root)
    pending = int(pending_identity["pendingInboxCount"])
    return sent_steering_payload(
        sent,
        target=target,
        agent_ensure_override=agent_ensure or {},
        pending_count=pending,
        pending_identity=pending_identity,
    )


def ensure_agent_for_pending_inbox(
    target: WorktreeTarget,
    *,
    attempt_cache: dict[str, float] | None = None,
    retry_seconds: float = PENDING_AGENT_ENSURE_RETRY_SECONDS,
    fast_mode: bool = False,
    force_new: bool = False,
    automatic: bool = True,
) -> dict[str, Any] | None:
    """Start an idle agent when its inbox has pending steering.

    Inbox steering must never sit unheard: a send to an off lane brings the lane's
    agent up (or its renewed successor, under `force_new`).
    """
    operator_items = pending_operator_inbox_items(target.repo_root)
    pending_count = len(operator_items)
    if pending_count <= 0:
        return None
    status = agent_status(target.repo_root)
    if status.running:
        return None
    if not _ensure_due(
        target.id, attempt_cache=attempt_cache, retry_seconds=retry_seconds
    ):
        return None
    trigger_key = inbox_item_key(operator_items[0].name)
    payload, _status = agent_ensure_response_payload(
        target, fast_mode=fast_mode, force_new=force_new, automatic=automatic
    )
    if payload.get("failure") == AGENT_FAILURE_RESTART_REFUSED:
        return deadletter_refused_ensure_payload(target, payload, operator_items)
    if payload.get("ok") is False:
        return deadletter_failed_agent_ensure_payload(
            target,
            payload,
            trigger_key=trigger_key,
        )
    return payload


def ensure_agent_for_available_work(
    target: WorktreeTarget,
    *,
    thread_id: str,
    running_lane_count: Callable[[], int] | None = None,
    ready_since_cache: dict[tuple[str, str], float] | None = None,
    attempt_cache: dict[str, float] | None = None,
    retry_seconds: float = AVAILABLE_WORK_ENSURE_RETRY_SECONDS,
    fast_mode: bool = False,
    force_new: bool = False,
) -> dict[str, Any] | None:
    """Claim one task as a stopped lane's actor, then start only that lane.

    The Drain-only caller supplies the live lane count. Expansion requires two
    more ready tasks than running lanes unless one candidate has remained ready
    for the starvation interval. This is deliberately a single-candidate
    decision: a lost claim never falls through to another stale candidate.
    """
    actor = canonical_thread_id(thread_id)
    if not actor:
        return _available_work_skip("unbound")
    if agent_status(target.repo_root).running:
        forget_available_work_observations(actor, ready_since_cache)
        return None
    if not _ensure_due(
        target.id, attempt_cache=attempt_cache, retry_seconds=retry_seconds
    ):
        return None
    with _AVAILABLE_WORK_CLAIM_LOCK:
        if agent_status(target.repo_root).running:
            forget_available_work_observations(actor, ready_since_cache)
            return None
        candidates = alloc.ordered_visible_ready_rows(actor)
        if not candidates:
            forget_available_work_observations(actor, ready_since_cache)
            return None
        now = time.monotonic()
        starved = _observe_available_work_candidates(
            actor,
            candidates,
            ready_since_cache,
            now=now,
        )
        required_task_count = (
            max(0, int(running_lane_count() if running_lane_count else 0))
            + AVAILABLE_WORK_READY_TASK_MARGIN
        )
        if len(candidates) < required_task_count and not starved:
            return _available_work_skip("capacity")
        chosen = candidates[0]
        task_uuid = identity.uuid_of(chosen)
        handle = identity.render_handle(chosen)
        site = claimstate.ClaimSite(
            worktree=target.repo_root.resolve(),
            branch=target.branch
            or git_read(target.repo_root, "branch", "--show-current"),
            head=git_read(target.repo_root, "rev-parse", "HEAD"),
        )
        claimed = claimstate.do_claim(
            task_uuid,
            actor,
            site=site,
            context_thread=actor,
            lease_seconds=SUPERVISOR_CLAIM_LEASE_SECONDS,
            guard_unclaimed=True,
        )
        if not claimed:
            return _available_work_skip("claim-lost", task_handle=handle)
        forget_available_work_observation(actor, task_uuid, ready_since_cache)
        try:
            payload, _status = agent_ensure_response_payload(
                target,
                fast_mode=fast_mode,
                force_new=force_new,
                automatic=True,
            )
        except Exception:
            claimstate.release_claim(task_uuid, actor)
            raise
        payload.update({"trigger": "available-work", "taskHandle": handle})
        if payload.get("ok") is False:
            payload["claimReleased"] = claimstate.release_claim(task_uuid, actor)
        return payload


def _observe_available_work_candidates(
    actor: str,
    candidates: Sequence[dict[str, Any]],
    ready_since_cache: dict[tuple[str, str], float] | None,
    *,
    now: float,
) -> bool:
    if ready_since_cache is None:
        return False
    candidate_keys = {(actor, identity.uuid_of(row)) for row in candidates}
    for key in tuple(ready_since_cache):
        if key[0] == actor and key not in candidate_keys:
            ready_since_cache.pop(key, None)
    for key in candidate_keys:
        ready_since_cache.setdefault(key, now)
    return any(
        now - ready_since_cache[key] >= AVAILABLE_WORK_STARVATION_SECONDS
        for key in candidate_keys
    )


def forget_available_work_observation(
    actor: str,
    task_uuid: str,
    ready_since_cache: dict[tuple[str, str], float] | None,
) -> None:
    resolved_actor = canonical_thread_id(actor)
    if ready_since_cache is not None and resolved_actor:
        with _AVAILABLE_WORK_CLAIM_LOCK:
            ready_since_cache.pop((resolved_actor, task_uuid), None)


def forget_available_work_observations(
    actor: str,
    ready_since_cache: dict[tuple[str, str], float] | None,
) -> None:
    resolved_actor = canonical_thread_id(actor)
    if ready_since_cache is None or not resolved_actor:
        return
    with _AVAILABLE_WORK_CLAIM_LOCK:
        for key in tuple(ready_since_cache):
            if key[0] == resolved_actor:
                ready_since_cache.pop(key, None)


def _available_work_skip(reason: str, *, task_handle: str = "") -> dict[str, Any]:
    payload: dict[str, Any] = {
        "ok": True,
        "action": "skipped",
        "trigger": "available-work",
        "reason": reason,
    }
    if task_handle:
        payload["taskHandle"] = task_handle
    return payload


def deadletter_refused_ensure_payload(
    target: WorktreeTarget,
    payload: dict[str, Any],
    operator_items: Sequence[Any],
) -> dict[str, Any]:
    """Park every pending operator item once automatic restarts are refused.

    The pending items are the wake condition: any left behind re-trigger the
    ensure on the next status pass, which is exactly the reinvocation storm
    the refusal exists to stop.
    """
    parked = [
        key
        for item in operator_items
        if (key := deadletter_inbox_item(target.repo_root, inbox_item_key(item.name)))
    ]
    if parked:
        payload["deadletteredInboxKeys"] = parked
        payload["deadletteredInboxKey"] = parked[0]
        payload["deadletterRequeueCommand"] = (
            f"spice agent requeue-deadletter {parked[0]}"
        )
        payload.update(pending_inbox_identity_payload(target.repo_root))
    return payload


def deadletter_failed_agent_ensure_payload(
    target: WorktreeTarget,
    payload: dict[str, Any],
    *,
    trigger_key: str,
) -> dict[str, Any]:
    """Park the operator item that caused a failed automatic ensure."""
    deadlettered = deadletter_inbox_item(target.repo_root, trigger_key)
    if deadlettered:
        payload["deadletteredInboxKey"] = deadlettered
        payload["deadletterRequeueCommand"] = (
            f"spice agent requeue-deadletter {deadlettered}"
        )
        payload.update(pending_inbox_identity_payload(target.repo_root))
    return payload


def _ensure_due(
    target_id: str,
    *,
    attempt_cache: dict[str, float] | None,
    retry_seconds: float,
) -> bool:
    if attempt_cache is None:
        return True
    now = time.monotonic()
    last_attempt = attempt_cache.get(target_id)
    if last_attempt is not None and now - last_attempt < retry_seconds:
        return False
    attempt_cache[target_id] = now
    return True
