"""Agent lifecycle endpoints for the serve UI: status, ensure, renewal."""

from __future__ import annotations

import subprocess
import time
from http import HTTPStatus
from typing import Any

from spice.agent.driver import driver_for
from spice.agent.lifecycle import (
    AGENT_FAILURE_OUT_OF_CREDITS,
    AgentOutOfCreditsError,
    agent_binding_error,
    agent_status,
    ensure_agent,
)
from spice.mail.inbox import (
    INBOX_CREDIT_FAILURE_DEADLETTER_THRESHOLD,
    collect_inbox_items,
    deadletter_inbox_item,
    inbox_item_is_automated_guidance,
    inbox_item_key,
    inbox_request_priority,
    pending_inbox_count,
)
from spice.serve.attachments import inbox_attachment_payloads
from spice.serve.markdown import render_message_html
from spice.serve.steering import SentSteeringMessage
from spice.serve.worktrees import WorktreeTarget

PENDING_AGENT_ENSURE_RETRY_SECONDS = 5.0


def agent_status_payload(target: WorktreeTarget) -> dict[str, Any]:
    status = agent_status(target.repo_root)
    binding_error = agent_binding_error(target.repo_root, status)
    return {
        "ok": True,
        "provider": driver_for(target.repo_root).name,
        "workTreeId": target.id,
        "status": status.process_status,
        "pid": status.pid or 0,
        "processGroupId": status.process_group_id or 0,
        "threadId": status.thread_id,
        "model": status.model,
        "thinking": status.reasoning_effort,
        "serviceTier": status.service_tier,
        "launchable": not status.running,
        "bindingStatus": "mismatch"
        if binding_error
        else ("bound" if status.thread_id else "unbound"),
        "bindingError": binding_error,
    }


def agent_ensure_response_payload(
    target: WorktreeTarget,
    *,
    force_new: bool = False,
    fast_mode: bool = False,
) -> tuple[dict[str, Any], HTTPStatus]:
    try:
        result = ensure_agent(
            target.repo_root,
            force_new=force_new,
            fast_mode=fast_mode,
            supervise_stdout=True,
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
        "prompt": result.prompt,
        "logPath": str(result.log_path) if result.log_path else "",
    }


def sent_steering_payload(
    sent: SentSteeringMessage,
    *,
    target: WorktreeTarget | None,
    agent_ensure_override: dict[str, Any] | None = None,
    pending_count: int | None = None,
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
    if pending_count is not None:
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
    pending = pending_inbox_count(target.repo_root)
    agent_ensure = ensure_agent_for_pending_inbox(
        target,
        pending,
        attempt_cache=state.pending_agent_ensure_attempts,
        retry_seconds=0.0,
        fast_mode=fast_mode,
        force_new=force_new,
    )
    return sent_steering_payload(
        sent,
        target=target,
        agent_ensure_override=agent_ensure or {},
        pending_count=pending,
    )


def ensure_agent_for_pending_inbox(
    target: WorktreeTarget,
    pending_count: int,
    *,
    attempt_cache: dict[str, float] | None = None,
    retry_seconds: float = PENDING_AGENT_ENSURE_RETRY_SECONDS,
    fast_mode: bool = False,
    force_new: bool = False,
) -> dict[str, Any] | None:
    """Start an idle agent when its inbox has pending steering.

    Inbox steering must never sit unheard: a send to an off lane brings the lane's
    agent up (or its renewed successor, under `force_new`).
    """
    if pending_count <= 0:
        return None
    # Automated guidance (maxim) is synthesized, not operator-sent: it must never
    # resurrect an idle agent on its own, or a down/out-of-credits lane restarts
    # in a loop driven by its own automated messages. Only genuine operator
    # steering brings a lane up.
    operator_items = [
        item
        for item in collect_inbox_items(target.repo_root)
        if not inbox_item_is_automated_guidance(item)
    ]
    if not operator_items:
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
        target, fast_mode=fast_mode, force_new=force_new
    )
    if payload.get("failure") == AGENT_FAILURE_OUT_OF_CREDITS:
        payload["creditFailureThreshold"] = INBOX_CREDIT_FAILURE_DEADLETTER_THRESHOLD
        deadlettered = deadletter_inbox_item(target.repo_root, trigger_key)
        if deadlettered:
            payload["deadletteredInboxKey"] = deadlettered
            payload["pendingInboxCount"] = pending_inbox_count(target.repo_root)
            payload["pendingInboxLabel"] = str(payload["pendingInboxCount"])
        return payload
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
