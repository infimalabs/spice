"""Native event wait for allocator continuation and operator steering."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from threading import Event, Timer
from typing import Any, Callable, Iterable, Literal, cast

from spice.errors import SpiceError
from spice.mail.inbox import inbox_dir, pending_inbox_count
from spice.tasks import config

WATCHFILES_NATIVE_READY_MS = 1000


@dataclass(frozen=True)
class AllocatorWake:
    kind: Literal["task", "steering", "deadline"]
    task_token: str


def task_event_token() -> str:
    path = config.ensure_task_event_file()
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        raise SpiceError(f"cannot read allocator task event: {path}: {exc}") from exc


def wait_for_allocator_event(
    baseline_task_token: str, claim_deadline: str
) -> AllocatorWake:
    """Block on task/inbox events or the peer claim's one-shot expiry."""
    repo_root = config.repo_root()
    task_path = config.ensure_task_event_file().resolve()
    steering_dir = inbox_dir(repo_root).resolve()
    steering_dir.mkdir(parents=True, exist_ok=True)

    immediate = _current_wake(repo_root, baseline_task_token)
    if immediate is not None:
        return immediate
    deadline_delay = _deadline_delay_seconds(claim_deadline)
    if deadline_delay <= 0:
        return _deadline_wake(repo_root, baseline_task_token)

    roots = tuple(dict.fromkeys((task_path.parent, steering_dir)))
    activated = False
    deadline_elapsed = Event()
    deadline_timer = Timer(deadline_delay, deadline_elapsed.set)
    deadline_timer.daemon = True
    deadline_timer.start()
    try:
        for changes in _import_watch()(
            *roots,
            watch_filter=lambda change, changed_path: _include_change(
                change,
                changed_path,
                task_path=task_path,
                steering_dir=steering_dir,
            ),
            force_polling=False,
            debounce=50,
            recursive=False,
            stop_event=deadline_elapsed,
            rust_timeout=WATCHFILES_NATIVE_READY_MS,
            yield_on_timeout=True,
        ):
            if not activated:
                # The first native-ready yield closes the race between the
                # caller's pre-allocation token and watcher registration.
                activated = True
                wake = _current_wake(repo_root, baseline_task_token)
                if wake is not None:
                    return wake
            if deadline_elapsed.is_set():
                return _deadline_wake(repo_root, baseline_task_token)
            if not changes:
                continue
            wake = _current_wake(repo_root, baseline_task_token)
            if wake is not None:
                return wake
    except SpiceError:
        raise
    except Exception as exc:
        raise SpiceError(f"allocator event watch failed: {exc}") from exc
    finally:
        deadline_timer.cancel()
    if deadline_elapsed.is_set():
        return _deadline_wake(repo_root, baseline_task_token)
    raise SpiceError("allocator event watcher stopped before a wake event")


def _deadline_delay_seconds(claim_deadline: str) -> float:
    try:
        parsed = datetime.fromisoformat(claim_deadline.replace("Z", "+00:00"))
    except ValueError as exc:
        raise SpiceError(f"invalid peer claim deadline: {claim_deadline!r}") from exc
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=UTC)
    return (parsed.astimezone(UTC) - datetime.now(UTC)).total_seconds()


def _deadline_wake(repo_root: Path, baseline_task_token: str) -> AllocatorWake:
    concurrent = _current_wake(repo_root, baseline_task_token)
    if concurrent is not None:
        return concurrent
    return AllocatorWake("deadline", task_event_token())


def _current_wake(repo_root: Path, baseline_task_token: str) -> AllocatorWake | None:
    token = task_event_token()
    # Steering wins a simultaneous task/inbox edge so the agent can ACK or
    # honor shutdown before claiming more work.
    if pending_inbox_count(repo_root) > 0:
        return AllocatorWake("steering", token)
    if token != baseline_task_token:
        return AllocatorWake("task", token)
    return None


def _import_watch() -> Callable[..., Iterable[set[tuple[object, str]]]]:
    from watchfiles import watch

    return cast(Callable[..., Iterable[set[tuple[object, str]]]], watch)


def _include_change(
    _change: Any,
    changed_path: str,
    *,
    task_path: Path,
    steering_dir: Path,
) -> bool:
    path = Path(changed_path).resolve(strict=False)
    return path == task_path or (path.parent == steering_dir and path.suffix == ".txt")
