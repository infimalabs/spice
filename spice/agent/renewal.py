"""Graceful agent succession: renewal never yanks a running agent.

Renew asks the live agent (by ordinary inbox steering) to reach a clean handoff;
a fresh successor starts on the next message once the agent is actually done.
The successor's steering is annotated with rehydration instructions pointing
at the ancestor thread so lane continuity survives the succession.
"""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any

from spice.agent.driver import driver_for
from spice.paths import STATE_DIRNAME

RENEWAL_WIND_DOWN_TEXT = (
    "You are being replaced by a renewed worktree agent. "
    "Stop taking new work, write a concise handoff, and wind down immediately."
)
RENEWAL_HANDOFF_REQUEST_SUFFIX = (
    "(RENEW: the operator requested renewal for this agent — bring your current "
    "work to a clean stop and post a handoff message. You are not being "
    "replaced right now; a fresh successor only starts once you are done.)"
)
RENEWAL_REHYDRATION_TEMPLATE = (
    "Previous agent thread id: {thread_id}. Before continuing, rehydrate deeply "
    "from that ancestor with `spice session briefing {thread_id}` and "
    "`spice session sweep {thread_id} --count 4`; use "
    "`spice session turns {thread_id} --view full` for exact prior turns "
    "when needed."
)


def renewal_request_path(repo_root: Path) -> Path:
    return (
        repo_root
        / STATE_DIRNAME
        / "agents"
        / driver_for(repo_root).state_dirname
        / "renew.json"
    )


def write_agent_renewal_request(
    repo_root: Path,
    *,
    target_thread_id: str,
    text: str,
    no_say: bool,
    fast_mode: bool = False,
    inbox_key: str = "",
) -> Path:
    path = renewal_request_path(repo_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "targetThreadId": target_thread_id,
        "text": text,
        "noSay": no_say,
        "fastMode": fast_mode,
        "inboxKey": inbox_key,
        "createdAt": time.time(),
    }
    tmp = path.with_name(f"{path.name}.tmp")
    tmp.write_text(json.dumps(payload, separators=(",", ":")) + "\n", encoding="utf-8")
    tmp.replace(path)
    return path


def read_agent_renewal_request(repo_root: Path) -> dict[str, Any] | None:
    try:
        loaded = json.loads(renewal_request_path(repo_root).read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return loaded if isinstance(loaded, dict) else None


def clear_agent_renewal_request(repo_root: Path) -> None:
    try:
        renewal_request_path(repo_root).unlink()
    except FileNotFoundError:
        return


def renewal_target_thread_id(request: dict[str, Any]) -> str:
    return str(request.get("targetThreadId") or "").strip()


def renewal_rehydration_text(thread_id: str) -> str:
    return RENEWAL_REHYDRATION_TEMPLATE.format(thread_id=thread_id)


def renewal_handoff_request_text(text: str) -> str:
    """Annotate an operator message so a still-running agent winds toward handoff."""
    body = str(text or "").strip()
    return (
        f"{body}\n{RENEWAL_HANDOFF_REQUEST_SUFFIX}"
        if body
        else RENEWAL_HANDOFF_REQUEST_SUFFIX
    )


def renewal_steering_text(text: str, *, previous_thread_id: str) -> str:
    if not previous_thread_id:
        return text
    stripped = text.rstrip()
    hint = renewal_rehydration_text(previous_thread_id)
    return f"{stripped}\n\n{hint}" if stripped else hint


def renewal_wind_down_rows(
    repo_root: Path | None, *, thread_id: str | None
) -> list[str]:
    if repo_root is None or not thread_id:
        return []
    request = read_agent_renewal_request(repo_root)
    if request is None or renewal_target_thread_id(request) != thread_id:
        return []
    return [
        f"renewal_request={renewal_request_path(repo_root).relative_to(repo_root).as_posix()}",
        f"target_thread_id={thread_id}",
        RENEWAL_WIND_DOWN_TEXT,
    ]
