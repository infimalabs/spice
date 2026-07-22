"""Server-owned starts for lanes that sit stopped while work is ready.

The decision to start a stopped Drain lane was reachable only as a side effect of
building the worktree inventory, and that payload is built only when a client
asks for it. A board holding one ready task and one idle lane is exactly the
board nobody is looking at, so the lane that should have started was waiting on a
browser event rather than on the work.

This gives the decision its own signal. Every task mutation rewrites the task
backend's event token (`spice.tasks.config.mark_task_backend_changed`) -- the
same file the lane watchers already wake on -- so a task entering READY is a
write to one known path, and this thread blocks on that path. The wait is bounded
by the starvation escape's own deadline, because a lone ready task is not a
change to wake on but an age to reach: nothing further will be written on its
behalf, and the bound is what lets that age arrive.

Starting is still `agentapi.ensure_agent_for_available_work` and nothing else --
same claim lock, same ready-since observations, same retry gate -- so a wake here
and an inventory build in a request thread cannot both start a lane for one task.
"""

from __future__ import annotations

import time
from pathlib import Path
from threading import Event, Thread
from typing import Any

from spice.serve.agentapi import (
    available_work_next_deadline,
    ensure_agent_for_available_work,
)
from spice.serve.livebus import FileChangeWatch
from spice.serve.payload.identity import (
    resolve_thread_id_for_target,
    team_facts_for_target,
)
from spice.tasks import config

AVAILABLE_WORK_WATCH_THREAD_NAME = "spice-serve-available-work-watch"
AVAILABLE_WORK_WATCH_JOIN_SECONDS = 3.0
# A deadline that has already passed still has to leave room for the evaluation
# it triggers to take effect, or a candidate the scheduler declined for some
# other reason would turn every wake into the next wake.
AVAILABLE_WORK_WATCH_MIN_SECONDS = 1.0


def start_available_work_watch(state: Any) -> AvailableWorkWatch | None:
    """Run the available-work decision on its own signal for as long as serve runs.

    Observer mode owns no lanes and cannot start one, so it gets no watcher.
    """
    if state.observer_mode:
        return None
    watch = AvailableWorkWatch(state)
    watch.start()
    return watch


class AvailableWorkWatch:
    """The wake loop that starts stopped Drain lanes."""

    def __init__(self, state: Any, *, events_path: Path | None = None) -> None:
        self._state = state
        self._events_path = events_path or config.ensure_task_event_file()
        self._stop = Event()
        self._thread: Thread | None = None
        # Published once the backend has accepted the event token, which is the
        # difference between a thread that is running and one that is watching.
        self.armed = Event()
        self.error = ""

    def start(self) -> None:
        thread = Thread(
            target=self._run,
            name=AVAILABLE_WORK_WATCH_THREAD_NAME,
            daemon=True,
        )
        self._thread = thread
        thread.start()

    def cancel(self) -> None:
        self._stop.set()

    def join(self, timeout: float = AVAILABLE_WORK_WATCH_JOIN_SECONDS) -> None:
        if self._thread is not None:
            self._thread.join(timeout=timeout)

    def _run(self) -> None:
        watch = FileChangeWatch()
        try:
            # Arm before the first evaluation.  A zero bound returns as soon as
            # native registration is known to be live, without closing it.
            watch.wait(
                (self._events_path,),
                self._stop,
                timeout=0.0,
                activated=self.armed,
            )
            while not self._stop.is_set():
                timeout = self.evaluate()
                watch.wait(
                    (self._events_path,),
                    self._stop,
                    timeout=timeout,
                    activated=self.armed,
                )
        except Exception as exc:
            # Losing this thread means no lane ever starts on its own again,
            # which reads exactly like an idle board. Say so where serve's other
            # startup and exit lines are read.
            self.error = str(exc)
            print(f"spice serve: available-work watch stopped: {exc}")
        finally:
            watch.close()

    def evaluate(self) -> float:
        """Start every stopped Drain lane that has work, and answer when to look again."""
        state = self._state
        for target in state.worktree_targets():
            thread_id = resolve_thread_id_for_target(state, target) or ""
            facts = team_facts_for_target(state.team_store, target, thread_id)
            if facts.get("lifetime") != "Drain":
                continue
            ensure_agent_for_available_work(
                target,
                thread_id=thread_id,
                ready_since_cache=state.available_work_ready_since,
                attempt_cache=state.pending_agent_ensure_attempts,
                fast_mode=bool(state.team_store.global_fast_mode_enabled()),
            )
        remaining = available_work_next_deadline(
            state.available_work_ready_since, now=time.monotonic()
        )
        return max(remaining, AVAILABLE_WORK_WATCH_MIN_SECONDS)
