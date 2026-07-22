"""Native file-watch support shared by live bus and serve launch paths."""

from __future__ import annotations

import os
import select
import time
from importlib import import_module
from pathlib import Path
from threading import Event
from typing import Any, Callable, cast

_MS_PER_SECOND = 1000


def _select_has_attrs(*names: str) -> bool:
    return all(hasattr(select, name) for name in names)


def _select_attr(name: str) -> Any:
    return getattr(select, name)


_HAVE_KQUEUE = _select_has_attrs(
    "kqueue",
    "kevent",
    "KQ_FILTER_VNODE",
    "KQ_EV_ADD",
    "KQ_EV_CLEAR",
    "KQ_NOTE_WRITE",
    "KQ_NOTE_EXTEND",
    "KQ_NOTE_DELETE",
    "KQ_NOTE_RENAME",
)
_KQUEUE_VNODE_FFLAGS: Any = 0
_KQUEUE_INVALIDATING_FFLAGS: Any = 0
if _HAVE_KQUEUE:
    _KQUEUE_VNODE_FFLAGS = (
        _select_attr("KQ_NOTE_WRITE")
        | _select_attr("KQ_NOTE_EXTEND")
        | _select_attr("KQ_NOTE_DELETE")
        | _select_attr("KQ_NOTE_RENAME")
    )
    _KQUEUE_INVALIDATING_FFLAGS = _select_attr("KQ_NOTE_DELETE") | _select_attr(
        "KQ_NOTE_RENAME"
    )

# kqueue blocks until a vnode event arrives; this bounds how long a cancelled
# watcher waits before noticing its stop flag. It is a wakeup interval, not a
# filesystem poll.
LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S = 1.0


def wait_for_change(
    paths: tuple[Path, ...],
    stop: Event,
    watch: _KqueueWatch | None = None,
    *,
    activated: Event | None = None,
) -> bool:
    """Block until a watched path changes or ``stop`` is set.

    A persistent ``watch`` keeps the native observer armed across calls. This
    prevents a change that arrives while the caller pushes a payload from
    falling into an observe-before-arm gap.
    """
    watch_paths = _existing_watch_paths(paths)
    if not watch_paths:
        raise RuntimeError("file watch has no observable paths")
    if _HAVE_KQUEUE:
        if watch is not None:
            return watch.wait(watch_paths, stop, activated=activated)
        return _wait_for_change_kqueue(watch_paths, stop, activated=activated)
    return _wait_for_change_watchfiles(watch_paths, stop, activated=activated)


class FileChangeWatch:
    """A native file watch that remains armed between bounded waits."""

    def __init__(self) -> None:
        self._kqueue = _KqueueWatch() if _HAVE_KQUEUE else None
        self._watchfiles_paths: tuple[Path, ...] = ()
        self._watchfiles_stop: Event | None = None
        self._watchfiles_changes: Any = None

    def wait(
        self,
        paths: tuple[Path, ...],
        stop: Event,
        *,
        timeout: float | None = None,
        activated: Event | None = None,
    ) -> bool:
        """Wait for one change while preserving native registration on return."""
        watch_paths = _existing_watch_paths(paths)
        if not watch_paths:
            raise RuntimeError("file watch has no observable paths")
        if self._kqueue is not None:
            return self._kqueue.wait(
                watch_paths,
                stop,
                timeout=timeout,
                activated=activated,
            )
        return self._wait_watchfiles(
            watch_paths,
            stop,
            timeout=timeout,
            activated=activated,
        )

    def _wait_watchfiles(
        self,
        paths: tuple[Path, ...],
        stop: Event,
        *,
        timeout: float | None,
        activated: Event | None,
    ) -> bool:
        if paths != self._watchfiles_paths or stop is not self._watchfiles_stop:
            self._close_watchfiles()
            module = import_module("watchfiles")
            watch = cast(Callable[..., Any], getattr(module, "watch"))
            self._watchfiles_changes = watch(
                *paths,
                stop_event=stop,
                rust_timeout=int(LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S * _MS_PER_SECOND),
                yield_on_timeout=True,
            )
            self._watchfiles_paths = paths
            self._watchfiles_stop = stop

        deadline = None if timeout is None else time.monotonic() + max(0.0, timeout)
        while not stop.is_set():
            try:
                observed = next(self._watchfiles_changes)
            except StopIteration:
                self._close_watchfiles()
                if stop.is_set():
                    return False
                raise RuntimeError("file watcher stopped before cancellation") from None
            if activated is not None:
                activated.set()
            if observed:
                return True
            if deadline is not None and time.monotonic() >= deadline:
                return False
        return False

    def _close_watchfiles(self) -> None:
        if self._watchfiles_changes is not None:
            self._watchfiles_changes.close()
        self._watchfiles_paths = ()
        self._watchfiles_stop = None
        self._watchfiles_changes = None

    def close(self) -> None:
        """Release the selected backend's native resources."""
        if self._kqueue is not None:
            self._kqueue.close()
        self._close_watchfiles()


class _KqueueWatch:
    """A kqueue VNODE watch kept armed across waits."""

    def __init__(self) -> None:
        self._paths: tuple[Path, ...] = ()
        self._descriptors: list[int] = []
        self._kqueue: Any = None
        self._events: list[Any] = []

    def wait(
        self,
        paths: tuple[Path, ...],
        stop: Event,
        *,
        timeout: float | None = None,
        activated: Event | None = None,
    ) -> bool:
        self._arm(paths)
        if not self._events:
            raise RuntimeError("lane watcher could not open observable paths")
        if activated is not None:
            activated.set()
        deadline = None if timeout is None else time.monotonic() + max(0.0, timeout)
        while not stop.is_set():
            wait_seconds = LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
            if deadline is not None:
                remaining = deadline - time.monotonic()
                if remaining <= 0.0:
                    return False
                wait_seconds = min(wait_seconds, remaining)
            triggered = self._kqueue.control([], len(self._events), wait_seconds)
            if triggered:
                if any(
                    getattr(event, "fflags", 0) & _KQUEUE_INVALIDATING_FFLAGS
                    for event in triggered
                ):
                    self.close()
                    self._arm(paths)
                    if not self._events:
                        raise RuntimeError(
                            "lane watcher could not rearm invalidated paths"
                        )
                return True
        return False

    def _arm(self, paths: tuple[Path, ...]) -> None:
        if paths == self._paths and self._kqueue is not None:
            return
        self.close()
        self._paths = paths
        descriptors: list[int] = []
        for path in paths:
            try:
                descriptors.append(os.open(path, os.O_RDONLY))
            except OSError:
                continue
        if not descriptors:
            return
        self._descriptors = descriptors
        self._kqueue = _select_attr("kqueue")()
        self._events = [
            _select_attr("kevent")(
                descriptor,
                filter=_select_attr("KQ_FILTER_VNODE"),
                flags=_select_attr("KQ_EV_ADD") | _select_attr("KQ_EV_CLEAR"),
                fflags=_KQUEUE_VNODE_FFLAGS,
            )
            for descriptor in descriptors
        ]
        self._kqueue.control(self._events, 0, 0)

    def close(self) -> None:
        if self._kqueue is not None:
            self._kqueue.close()
            self._kqueue = None
        for descriptor in self._descriptors:
            try:
                os.close(descriptor)
            except OSError:
                pass
        self._descriptors = []
        self._events = []
        self._paths = ()


def _existing_watch_paths(paths: tuple[Path, ...]) -> tuple[Path, ...]:
    result: list[Path] = []
    seen: set[Path] = set()
    for path in paths:
        if not path.exists() or path in seen:
            continue
        seen.add(path)
        result.append(path)
    return tuple(result)


def _wait_for_change_kqueue(
    paths: tuple[Path, ...], stop: Event, *, activated: Event | None = None
) -> bool:
    descriptors: list[int] = []
    try:
        for path in paths:
            try:
                descriptors.append(os.open(path, os.O_RDONLY))
            except OSError:
                continue
        if not descriptors:
            raise RuntimeError("lane watcher could not open observable paths")
        kqueue = _select_attr("kqueue")()
        try:
            events = [
                _select_attr("kevent")(
                    descriptor,
                    filter=_select_attr("KQ_FILTER_VNODE"),
                    flags=_select_attr("KQ_EV_ADD") | _select_attr("KQ_EV_CLEAR"),
                    fflags=_KQUEUE_VNODE_FFLAGS,
                )
                for descriptor in descriptors
            ]
            kqueue.control(events, 0, 0)
            if activated is not None:
                activated.set()
            while not stop.is_set():
                triggered = kqueue.control(
                    [], len(events), LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S
                )
                if triggered:
                    return True
            return False
        finally:
            kqueue.close()
    finally:
        for descriptor in descriptors:
            os.close(descriptor)


def _wait_for_change_watchfiles(
    paths: tuple[Path, ...], stop: Event, *, activated: Event | None = None
) -> bool:
    module = import_module("watchfiles")
    watch = cast(Callable[..., Any], getattr(module, "watch"))
    changes = watch(
        *paths,
        stop_event=stop,
        rust_timeout=int(LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S * _MS_PER_SECOND),
        yield_on_timeout=True,
    )
    for observed in changes:
        if activated is not None and not activated.is_set():
            activated.set()
        if observed:
            return True
    if activated is not None and not activated.is_set():
        raise RuntimeError("lane watcher stopped before native registration")
    return False
