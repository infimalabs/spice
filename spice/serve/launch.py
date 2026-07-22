"""Server-owned starts for lanes that sit stopped while work is ready.

The decision to start a stopped Drain lane was reachable only as a side effect of
building the worktree inventory, and that payload is built only when a client
asks for it. A board holding one ready task and one idle lane is exactly the
board nobody is looking at, so the lane that should have started was waiting on a
browser event rather than on the work.

This gives the decisions their own signal. Every task mutation rewrites the task
backend's event token (`spice.tasks.config.mark_task_backend_changed`), and every
completed inbox mutation rewrites its lane-local inbox event token. This thread
keeps all of those paths armed before it evaluates either pending steering or
available work. The wait is bounded by the starvation escape's own deadline,
because a lone ready task is not a change to wake on but an age to reach: nothing
further will be written on its behalf, and the bound is what lets that age arrive.

Starting still runs through the inventory's shared launch decision -- the same
pending-inbox retry/renewal behavior and the same available-work claim lock,
starvation escape, and retry gate -- so a wake here and an inventory build in a
request thread converge on the same guarded operations.
"""

from __future__ import annotations

from pathlib import Path
from threading import Event, Thread
from typing import Any

from spice.mail.inbox import ensure_inbox_event_file
from spice.serve.agentapi import AVAILABLE_WORK_STARVATION_SECONDS
from spice.serve.livebuswatch import FileChangeWatch
from spice.serve.payload.identity import resolve_thread_id_for_target
from spice.serve.worktree.inventory import ensure_work_tree_agent
from spice.tasks import config

AVAILABLE_WORK_WATCH_THREAD_NAME = "spice-serve-available-work-watch"
AVAILABLE_WORK_WATCH_JOIN_SECONDS = 3.0
# A deadline that has already passed still has to leave room for the evaluation
# it triggers to take effect, or a candidate the scheduler declined for some
# other reason would turn every wake into the next wake.
AVAILABLE_WORK_WATCH_MIN_SECONDS = 1.0


def start_available_work_watch(state: Any) -> AvailableWorkWatch | None:
    """Run server-owned launch decisions for as long as serve runs.

    Observer mode owns no lanes and cannot start one, so it gets no watcher.
    """
    if state.observer_mode:
        return None
    watch = AvailableWorkWatch(state)
    watch.start()
    return watch


class AvailableWorkWatch:
    """The wake loop that starts lanes for steering or Drain work."""

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
            watch_paths = self._watch_paths()
            # Arm before the first evaluation.  A zero bound returns as soon as
            # native registration is known to be live, without closing it.
            watch.wait(
                watch_paths,
                self._stop,
                timeout=0.0,
                activated=self.armed,
            )
            while not self._stop.is_set():
                timeout = self.evaluate()
                refreshed_paths = self._watch_paths()
                if refreshed_paths != watch_paths:
                    watch_paths = refreshed_paths
                    watch.wait(
                        watch_paths,
                        self._stop,
                        timeout=0.0,
                        activated=self.armed,
                    )
                    # The new paths were discovered by the evaluation above.
                    # Re-evaluate after arming them so a publication in that
                    # discovery-to-registration interval cannot be stranded.
                    continue
                watch.wait(
                    watch_paths,
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

    def _watch_paths(self) -> tuple[Path, ...]:
        return (
            self._events_path,
            *(
                ensure_inbox_event_file(target.repo_root)
                for target in self._state.worktree_targets()
            ),
        )

    def evaluate(self) -> float:
        """Start lanes for steering or Drain work, then bound the next look."""
        state = self._state
        # Nothing declined for capacity means nothing is waiting on an age to
        # arrive, and the whole interval is the longest a task filed a moment
        # from now could have to wait.
        remaining = AVAILABLE_WORK_STARVATION_SECONDS
        for target in state.worktree_targets():
            thread_id = resolve_thread_id_for_target(state, target) or ""
            *_, agent_ensure = ensure_work_tree_agent(state, target, thread_id)
            # The lane that declined is the one holding the oldest candidate's
            # deadline: it already read the rows under the claim lock, so the
            # wake it needs rides back out with the refusal instead of costing
            # this thread a second export of the same board.
            if agent_ensure is not None and "retryAfterSeconds" in agent_ensure:
                remaining = min(remaining, float(agent_ensure["retryAfterSeconds"]))
        return max(remaining, AVAILABLE_WORK_WATCH_MIN_SECONDS)
