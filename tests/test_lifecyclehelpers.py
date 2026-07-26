"""Shared fakes for lifecycle direct and supervised tests."""

from pathlib import Path

from spice.agent.lifecyclebinding import AgentStatus


def status(*, thread_id: str = "") -> AgentStatus:
    """An idle agent, shaped by the dataclass the real probe returns.

    Built as the production value rather than a namespace of the fields one
    caller happened to read, so a consumer reaching for a field this stands in
    for gets the real answer and a field added to the dataclass fails here at
    construction instead of inside whichever renderer read it first.
    """
    return AgentStatus(
        repo_root=Path(),
        state_path=Path(),
        process_status="idle",
        pid=None,
        process_group_id=None,
        thread_id=thread_id,
        driver="",
        model="",
        reasoning_effort="",
        service_tier="",
        started_at="",
        ready_at="",
        startup_failure="",
        log_path=None,
        prompt_skill_path=None,
        command=(),
    )


class FakeProcess:
    def __init__(self, *, pid: int, returncode: int | None) -> None:
        self.pid = pid
        self.returncode = returncode
        self.wait_calls = 0

    def poll(self):
        return self.returncode

    def wait(self):
        self.wait_calls += 1
        return self.returncode


class FakeThread:
    def __init__(self) -> None:
        self.joined_timeouts: list[float | None] = []

    def join(self, timeout: float | None = None) -> None:
        self.joined_timeouts.append(timeout)
