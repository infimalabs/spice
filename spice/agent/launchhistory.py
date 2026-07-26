"""Supervised-agent launch history, refusal policy, and log forensics."""

from __future__ import annotations

import json
import os
import re
import time
from collections.abc import Iterable
from pathlib import Path
from typing import Any, cast

from spice.agent.driver import driver_for
from spice.agent.identity import canonical_thread_id
from spice.agent.lifecyclebinding import utc_now
from spice.agent.paths import agent_worktree_state_dir
from spice.errors import SpiceError
from spice.paths import atomic_write_json
from spice.transcript.events import (
    AssistantText,
    FailureSignal,
    Image,
    ToolCall,
    TranscriptEvent,
)
from spice.transcript.reader import TranscriptEventReader, transcript_size
from spice.transcript.timestamps import parse_timestamp

LAUNCH_OUTCOMES_FILE = "launch-outcomes.json"
LAUNCH_OUTCOMES_LIMIT = 32
# A supervised launch that dies this young never did real work; healthy
# sessions run for minutes, the 2026-07-17 spend-limit storm's launches died
# in 0.75-3s.
RAPID_DEATH_LIFETIME_SECONDS = 60.0
RAPID_DEATH_REFUSAL_THRESHOLD = 3
RAPID_DEATH_REFUSAL_WINDOW_SECONDS = 30 * 60
STARTUP_SESSION_ID_TIMEOUT_SECONDS = 1.0
STARTUP_SESSION_ID_POLL_SECONDS = 0.05
STARTUP_LOG_HEAD_BYTES = 4096
STARTUP_LOG_TAIL_BYTES = 4096


def agent_process_failure_kind(
    repo_root: Path,
    *,
    exit_code: int,
    output: str,
) -> str:
    return driver_for(repo_root).process_failure_kind(
        exit_code=exit_code,
        output=output,
    )


def launch_outcomes_path(repo_root: Path) -> Path:
    return agent_worktree_state_dir(repo_root) / LAUNCH_OUTCOMES_FILE


def read_launch_outcomes(repo_root: Path) -> list[dict[str, Any]]:
    try:
        loaded = json.loads(launch_outcomes_path(repo_root).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, SpiceError):
        return []
    if not isinstance(loaded, list):
        return []
    return [entry for entry in loaded if isinstance(entry, dict)]


def launch_refusal(
    repo_root: Path, *, now: float | None = None
) -> dict[str, Any] | None:
    """Why automatic restarts are refused right now, or None when they may run.

    Message-agnostic by design: launch lifetime and the recorded rate-limit
    reset horizon are the ordinary signals, never failure prose. A trailing
    outcome that died under RAPID_DEATH_LIFETIME_SECONDS counts as rapid; a
    supervised ``startup-stalled`` classification also counts because its
    intentional readiness grace can exceed that threshold. Once the run reaches
    RAPID_DEATH_REFUSAL_THRESHOLD, automatic ensures hold off until
    RAPID_DEATH_REFUSAL_WINDOW_SECONDS pass the newest death — or until the
    largest recorded reset epoch, when the account itself named the retry
    horizon.
    """
    clock = time.time() if now is None else now
    rapid: list[dict[str, Any]] = []
    for outcome in reversed(read_launch_outcomes(repo_root)):
        lifetime = outcome.get("lifetime_seconds")
        if not isinstance(lifetime, (int, float)):
            break
        startup_stalled = outcome.get("failure_kind") == "startup-stalled"
        if float(lifetime) >= RAPID_DEATH_LIFETIME_SECONDS and not startup_stalled:
            break
        rapid.append(outcome)
    if len(rapid) < RAPID_DEATH_REFUSAL_THRESHOLD:
        return None
    newest_death = max(
        (_epoch_seconds(outcome.get("ended_at")) for outcome in rapid), default=0.0
    )
    reset_epoch = max(
        (
            int(outcome["reset_epoch"])
            for outcome in rapid
            if isinstance(outcome.get("reset_epoch"), int)
        ),
        default=0,
    )
    hold_until = max(
        newest_death + RAPID_DEATH_REFUSAL_WINDOW_SECONDS, float(reset_epoch)
    )
    if clock >= hold_until:
        return None
    refusal: dict[str, Any] = {
        "consecutive_rapid_deaths": len(rapid),
        "hold_until_epoch": int(hold_until),
    }
    if reset_epoch:
        refusal["reset_epoch"] = reset_epoch
    return refusal


def _epoch_seconds(stamp: Any) -> float:
    parsed = parse_timestamp(stamp)
    return parsed.timestamp() if parsed is not None else 0.0


def record_launch_outcome(repo_root: Path, outcome: dict[str, Any]) -> None:
    """Append one terminal launch outcome to the bounded per-worktree journal.

    The journal is the wake path's memory that starts keep dying: restart
    policy reads it to refuse reinvoking a lane whose launches die young.
    Recording is diagnostic and must never alter the supervised agent's own
    exit path, so persistence failures are contained here.
    """
    try:
        outcomes = [*read_launch_outcomes(repo_root), outcome]
        atomic_write_json(
            launch_outcomes_path(repo_root), outcomes[-LAUNCH_OUTCOMES_LIMIT:]
        )
    except (OSError, SpiceError):
        pass


def supervised_launch_outcome(
    repo_root: Path,
    *,
    thread_id: str,
    log_path: Path,
    started_at: str,
    lifetime_seconds: float,
    exit_code: int | None,
    failure_kind: str = "",
    released_claim: str = "",
) -> dict[str, Any]:
    scan = scan_launch_log(repo_root, log_path)
    kind = (
        failure_kind
        or str(scan.get("kind") or "")
        or agent_process_failure_kind(
            repo_root,
            exit_code=int(exit_code or 0),
            output=tail_text(log_path, STARTUP_LOG_TAIL_BYTES),
        )
    )
    outcome: dict[str, Any] = {
        "thread_id": thread_id,
        "log_path": str(log_path),
        "started_at": started_at,
        "ended_at": utc_now(),
        "lifetime_seconds": round(lifetime_seconds, 3),
        "exit_code": exit_code,
        "assistant_messages": scan["assistant_messages"],
        "tool_calls": scan["tool_calls"],
        "failure_kind": kind,
        "released_claim": released_claim,
    }
    if scan.get("reset_epoch") is not None:
        outcome["reset_epoch"] = scan["reset_epoch"]
    return outcome


def scan_launch_log(repo_root: Path, log_path: Path) -> dict[str, Any]:
    """Activity counts and structural failure fields from one launch log.

    The bounded typed read includes a terminal unterminated record from a
    process that exited mid-flush. Marker-format stdout decodes only to unknown
    facts and contributes nothing, leaving text-pattern fallback to the caller.
    """
    driver = driver_for(repo_root)
    end_offset = transcript_size(log_path)
    if end_offset is None:
        return {"assistant_messages": 0, "tool_calls": 0}
    events = (
        TranscriptEventReader(log_path, driver)
        .read(
            "bounded",
            start_offset=0,
            end_offset=end_offset,
        )
        .events
    )
    return _launch_log_projection(events)


def _launch_log_projection(events: Iterable[TranscriptEvent]) -> dict[str, Any]:
    assistant_loci: set[int] = set()
    tool_loci: set[int] = set()
    failure: dict[str, Any] = {}
    for event in events:
        locus = event.at.line
        if isinstance(event, AssistantText) or (
            isinstance(event, Image)
            and event.role == "assistant"
            and event.payload_index is not None
        ):
            assistant_loci.add(locus)
        elif isinstance(event, ToolCall):
            tool_loci.add(locus)
        if isinstance(event, FailureSignal):
            failure["kind"] = event.kind
            if event.reset_epoch is not None:
                failure["reset_epoch"] = event.reset_epoch
    return {
        "assistant_messages": len(assistant_loci),
        # Claude's canonical response-item selection historically chose prose
        # over a tool call carried by the same source line. Keep that launch
        # outcome contract while the typed reader preserves both facts.
        "tool_calls": len(tool_loci - assistant_loci),
        **failure,
    }


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
