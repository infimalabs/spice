"""Event-driven task and inbox wake publication for Serve lifecycle decisions."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from threading import Event, Thread
from time import monotonic
from typing import Any

from spice.mail.inbox import ensure_inbox_event_file
from spice.serve.lifecycle import (
    AutomaticLifecycleWake,
    LifecycleOutcome,
    LifecycleOutcomeStatus,
    LifecycleWakeSource,
)
from spice.serve.livebuswatch import FileChangeWatch
from spice.serve.worktree.target import WorktreeTarget
from spice.tasks import config

AVAILABLE_WORK_WATCH_THREAD_NAME = "spice-serve-available-work-watch"
AVAILABLE_WORK_WATCH_JOIN_SECONDS = 3.0
# A deadline that has already passed still has to leave room for the evaluation
# it triggers to take effect, or a candidate the scheduler declined for some
# other reason would turn every wake into the next wake.
AVAILABLE_WORK_WATCH_MIN_SECONDS = 1.0


@dataclass(frozen=True, slots=True)
class _EventSnapshot:
    task_identity: str
    inbox_identities: dict[str, str]


@dataclass(frozen=True, slots=True)
class _LifecycleTimer:
    due: float
    source_identity: str


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
        self._timers: dict[str, _LifecycleTimer] = {}
        self._event_state: _EventSnapshot | None = None

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
            targets = self._state.worktree_targets()
            watch_paths = self._watch_paths(targets)
            # Arm before the first evaluation.  A zero bound returns as soon as
            # native registration is known to be live, without closing it.
            watch.wait(
                watch_paths,
                self._stop,
                timeout=0.0,
                activated=self.armed,
            )
            wakes = self.observe_events(targets)
            while not self._stop.is_set():
                self.evaluate(wakes)
                if self._stop.is_set():
                    break
                refreshed_targets = self._state.worktree_targets()
                refreshed_paths = self._watch_paths(refreshed_targets)
                if refreshed_paths != watch_paths:
                    watch_paths = refreshed_paths
                    watch.wait(
                        watch_paths,
                        self._stop,
                        timeout=0.0,
                        activated=self.armed,
                    )
                    wakes = self.observe_events(refreshed_targets)
                    continue
                watch.wait(
                    watch_paths,
                    self._stop,
                    timeout=self.next_timer_timeout(),
                    activated=self.armed,
                )
                if self._stop.is_set():
                    break
                refreshed_targets = self._state.worktree_targets()
                refreshed_paths = self._watch_paths(refreshed_targets)
                if refreshed_paths != watch_paths:
                    watch_paths = refreshed_paths
                    watch.wait(
                        watch_paths,
                        self._stop,
                        timeout=0.0,
                        activated=self.armed,
                    )
                wakes = self.observe_events(refreshed_targets)
                wakes += self.due_timer_wakes(
                    excluding={wake.target_id for wake in wakes},
                )
        except Exception as exc:
            # Losing this thread means no lane ever starts on its own again,
            # which reads exactly like an idle board. Say so where serve's other
            # startup and exit lines are read.
            self.error = str(exc)
            print(f"spice serve: available-work watch stopped: {exc}")
        finally:
            watch.close()

    def _watch_paths(
        self,
        targets: list[WorktreeTarget],
    ) -> tuple[Path, ...]:
        return (
            self._events_path,
            *(ensure_inbox_event_file(target.repo_root) for target in targets),
        )

    def evaluate(
        self,
        wakes: tuple[AutomaticLifecycleWake, ...],
    ) -> tuple[LifecycleOutcome, ...]:
        """Publish compact wakes and block on their reconciliation completions."""
        futures = [(wake, self._state.submit_lifecycle_wake(wake)) for wake in wakes]
        outcomes: list[LifecycleOutcome] = []
        for wake, future in futures:
            outcome = future.result()
            if outcome.status is LifecycleOutcomeStatus.FAILED:
                raise RuntimeError(
                    "lifecycle reconciliation failed "
                    f"target={outcome.target_id}: {outcome.detail}"
                )
            outcomes.append(outcome)
            self._schedule_timer(wake, outcome)
        return tuple(outcomes)

    def _event_snapshot(
        self,
        targets: list[WorktreeTarget],
    ) -> _EventSnapshot:
        return _EventSnapshot(
            task_identity=_event_identity(self._events_path),
            inbox_identities={
                target.id: _event_identity(ensure_inbox_event_file(target.repo_root))
                for target in targets
            },
        )

    def observe_events(
        self,
        targets: list[WorktreeTarget],
    ) -> tuple[AutomaticLifecycleWake, ...]:
        """Return revision-backed wakes since this watcher's prior observation."""
        target_ids = {target.id for target in targets}
        for target_id in self._timers.keys() - target_ids:
            self._timers.pop(target_id, None)
        current = self._event_snapshot(targets)
        wakes = self._event_wakes(self._event_state, current, targets)
        self._event_state = current
        return wakes

    def _event_wakes(
        self,
        previous: _EventSnapshot | None,
        current: _EventSnapshot,
        targets: list[WorktreeTarget],
    ) -> tuple[AutomaticLifecycleWake, ...]:
        wakes: list[AutomaticLifecycleWake] = []
        task_changed = (
            previous is None or current.task_identity != previous.task_identity
        )
        previous_inboxes = previous.inbox_identities if previous is not None else {}
        for target in targets:
            inbox_identity = current.inbox_identities.get(target.id, "")
            prior_inbox_identity = previous_inboxes.get(target.id)
            inbox_changed = (
                prior_inbox_identity is None or inbox_identity != prior_inbox_identity
            )
            if inbox_changed and inbox_identity not in {"", "0"}:
                source = LifecycleWakeSource.INBOX
                source_identity = inbox_identity
            elif task_changed or prior_inbox_identity is None:
                source = LifecycleWakeSource.TASK
                source_identity = current.task_identity
            else:
                continue
            if not source_identity:
                continue
            wakes.append(
                AutomaticLifecycleWake(
                    target_id=target.id,
                    source=source,
                    source_identity=source_identity,
                )
            )
        return tuple(wakes)

    def _schedule_timer(
        self,
        wake: AutomaticLifecycleWake,
        outcome: LifecycleOutcome,
    ) -> None:
        retry_after = outcome.retry_after_seconds
        if retry_after is None:
            self._timers.pop(wake.target_id, None)
            return
        delay = max(retry_after, AVAILABLE_WORK_WATCH_MIN_SECONDS)
        due = monotonic() + delay
        self._timers[wake.target_id] = _LifecycleTimer(
            due=due,
            source_identity=f"{outcome.input_identity}@{due:.9f}",
        )

    def next_timer_timeout(self) -> float | None:
        """Return the blocking wait bound for the next scheduled notification."""
        if not self._timers:
            return None
        return max(
            min(timer.due for timer in self._timers.values()) - monotonic(),
            0.0,
        )

    def due_timer_wakes(
        self,
        *,
        excluding: set[str] | None = None,
    ) -> tuple[AutomaticLifecycleWake, ...]:
        """Consume timer notifications that are due for reconciliation."""
        now = monotonic()
        for target_id in excluding or set():
            timer = self._timers.get(target_id)
            if timer is not None and timer.due <= now:
                self._timers.pop(target_id, None)
        due_target_ids = sorted(
            target_id for target_id, timer in self._timers.items() if timer.due <= now
        )
        wakes = tuple(
            AutomaticLifecycleWake(
                target_id=target_id,
                source=LifecycleWakeSource.TIMER,
                source_identity=self._timers.pop(target_id).source_identity,
            )
            for target_id in due_target_ids
        )
        return wakes


def _event_identity(path: Path) -> str:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return "0"
    token = (text.split(maxsplit=1) or ["0"])[0]
    return token if token.isdigit() else "0"
