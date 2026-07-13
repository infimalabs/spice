"""End-to-end deadline for the primary session rehydration commands."""

from __future__ import annotations

import signal
import threading
from collections.abc import Iterator
from contextlib import contextmanager

from spice.errors import SpiceError

DEFAULT_REHYDRATION_DEADLINE_SECONDS = 30.0


class RehydrationDeadlineExceeded(SpiceError):
    """A briefing or sweep exhausted its complete render budget."""

    def __init__(
        self, *, action: str, inputs: tuple[str, ...], timeout_seconds: float
    ) -> None:
        self.action = action
        self.inputs = inputs
        self.timeout_seconds = timeout_seconds
        rendered_inputs = ",".join(inputs) if inputs else "ambient-transcript"
        super().__init__(
            f"session {action} deadline exceeded phase=end-to-end-render "
            f"inputs={rendered_inputs} budget={timeout_seconds:g}s"
        )


@contextmanager
def rehydration_deadline(
    *, action: str, inputs: tuple[str, ...], timeout_seconds: float
) -> Iterator[None]:
    """Interrupt a main-thread POSIX render when its complete budget expires."""
    if timeout_seconds <= 0:
        raise SpiceError("session rehydration deadline must be positive")
    if threading.current_thread() is not threading.main_thread() or not hasattr(
        signal, "setitimer"
    ):
        yield
        return

    def expire(_signum: int, _frame: object) -> None:
        raise RehydrationDeadlineExceeded(
            action=action,
            inputs=inputs,
            timeout_seconds=timeout_seconds,
        )

    previous_handler = signal.getsignal(signal.SIGALRM)
    signal.signal(signal.SIGALRM, expire)
    previous_timer = signal.setitimer(signal.ITIMER_REAL, timeout_seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous_handler)
        if previous_timer[0] > 0:
            signal.setitimer(signal.ITIMER_REAL, *previous_timer)
