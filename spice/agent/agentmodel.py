"""Immutable decisions and records shared by the agent lifecycle."""

import json
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.agent.driver import (
    FAST_MODE_LAUNCH_KNOB,
    PERSONALITY_LAUNCH_KNOB,
    AgentDriver,
)
from spice.agent.launchhistory import agent_process_failure_kind
from spice.agent.lifecyclebinding import (
    AGENT_STARTUP_READY,
    AgentStatus,
    agent_state_path,
    utc_now,
)
from spice.agent.paths import agent_state_dir
from spice.config.values import DEFAULT_AGENT_PERSONALITY
from spice.errors import SpiceError


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
    # Knobs this launch was asked for that its driver has no seam to carry.
    # Empty on a launch that asked for nothing the driver cannot do.
    unhonored_launch_knobs: tuple[str, ...] = ()


def requested_launch_knobs(
    *, personality: str | None, resolved_personality: str, fast_mode: bool
) -> tuple[str, ...]:
    """Return launch knobs explicitly requested beyond shipped defaults."""
    requested = []
    if personality or resolved_personality != DEFAULT_AGENT_PERSONALITY:
        requested.append(PERSONALITY_LAUNCH_KNOB)
    if fast_mode:
        requested.append(FAST_MODE_LAUNCH_KNOB)
    return tuple(requested)


@dataclass(frozen=True)
class LaunchClaim:
    """The exact task reservation a supervised launch must eventually release."""

    uuid: str
    actor: str


def claimed_task_phase_launch(
    repo_root: Path, driver_name: str, status: AgentStatus
) -> dict[str, str]:
    """Return model/effort overrides from the currently claimed task phase."""
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


def attachable_thread_id(
    driver: AgentDriver,
    repo_root: Path,
    thread_id: str,
) -> str:
    """Return a resumable bound thread, or empty text to start fresh."""
    if thread_id and not driver.thread_resumable_here(repo_root, thread_id):
        return ""
    return thread_id


@dataclass(frozen=True)
class PreparedLaunch:
    """Everything one launch resolves before a process exists."""

    action: str
    command: list[str]
    resume_thread_id: str
    model: str
    reasoning_effort: str
    fast_mode: bool
    unhonored_knobs: tuple[str, ...]


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


def next_agent_log_path(repo_root: Path, *, timestamp: str | None = None) -> Path:
    stamp = (timestamp or utc_now()).replace(":", "").replace("-", "")
    return agent_state_dir(repo_root) / f"{stamp}.log"


def build_agent_state(
    *,
    process: subprocess.Popen[str],
    action: str,
    command: list[str],
    driver: str,
    model: str,
    reasoning_effort: str,
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
        "thread_id": thread_id,
        "prompt_skill_path": str(prompt_skill_path),
        "log_path": str(log_path),
        "fast_mode": fast_mode,
        "startup_status": startup_status,
        "ready_at": utc_now() if startup_status == AGENT_STARTUP_READY else "",
        "startup_failure": "",
    }


def touch_agent_state(repo_root: Path) -> None:
    try:
        path = agent_state_path(repo_root)
        if path.exists():
            path.touch()
    except (OSError, SpiceError):
        pass


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
        == "out-of-credits"
    ):
        return AgentOutOfCreditsError(rendered)
    return SpiceError(rendered)
