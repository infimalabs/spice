"""End-to-end deadline for the primary session rehydration commands."""

from __future__ import annotations

import signal
import threading
from queue import Queue
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Callable, TypeVar, cast

from spice.errors import SpiceError

DEFAULT_REHYDRATION_DEADLINE_SECONDS = 30.0
ResultT = TypeVar("ResultT")


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
    if threading.current_thread() is not threading.main_thread():
        yield
        return

    if not _can_use_setitimer():
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


def _can_use_setitimer() -> bool:
    return hasattr(signal, "setitimer") and hasattr(signal, "ITIMER_REAL")


def run_with_rehydration_deadline(
    callback: Callable[[], ResultT],
    *,
    action: str,
    inputs: tuple[str, ...],
    timeout_seconds: float,
) -> ResultT:
    """Run resolution+render under POSIX alarm or a portable daemon worker."""
    if timeout_seconds <= 0:
        raise SpiceError("session rehydration deadline must be positive")
    if threading.current_thread() is threading.main_thread() and _can_use_setitimer():
        with rehydration_deadline(
            action=action,
            inputs=inputs,
            timeout_seconds=timeout_seconds,
        ):
            return callback()

    terminal: Queue[tuple[str, object]] = Queue(maxsize=1)

    def run() -> None:
        try:
            terminal.put(("result", callback()))
        except BaseException as exc:
            terminal.put(("error", exc))

    worker = threading.Thread(
        target=run,
        name=f"spice-session-{action}-deadline",
        daemon=True,
    )
    worker.start()
    worker.join(timeout=timeout_seconds)
    if worker.is_alive():
        raise RehydrationDeadlineExceeded(
            action=action,
            inputs=inputs,
            timeout_seconds=timeout_seconds,
        )
    kind, value = terminal.get_nowait()
    if kind == "error":
        raise cast(BaseException, value)
    return cast(ResultT, value)
