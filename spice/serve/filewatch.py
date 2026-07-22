from __future__ import annotations

import argparse
from collections.abc import Iterable, Iterator
from http.server import ThreadingHTTPServer
from importlib import import_module
from pathlib import Path
from threading import Event, Thread
from typing import Any, Callable, cast

from spice.errors import SpiceError

WATCHFILES_NATIVE_READY_MS = 1000
SERVE_FILE_WATCH_ACTIVATION_TIMEOUT_SECONDS = 5.0


def start_exit_file_watch(
    server: ThreadingHTTPServer,
    args: argparse.Namespace,
    *,
    stop_event: Event,
) -> Thread | None:
    watched_path = getattr(args, "until", None)
    if watched_path is None:
        return None
    path = Path(watched_path).expanduser()
    _validate_watch_path(_normalized_watch_path(path))
    print(f"spice serve: watching {path} for exit")
    activated = Event()
    startup_errors: list[Exception] = []
    thread = Thread(
        target=_run_file_watch,
        args=(server, path, stop_event, activated, startup_errors),
        name="spice-serve-file-watch",
        daemon=True,
    )
    thread.start()
    if not activated.wait(timeout=SERVE_FILE_WATCH_ACTIVATION_TIMEOUT_SECONDS):
        stop_event.set()
        raise SpiceError(
            "spice serve --until watcher activation deadline exceeded "
            f"path={path} budget={SERVE_FILE_WATCH_ACTIVATION_TIMEOUT_SECONDS:g}s"
        )
    if startup_errors:
        thread.join()
        error = startup_errors[0]
        raise SpiceError(f"spice serve --until watch failed: {error}") from error
    return thread


def _run_file_watch(
    server: ThreadingHTTPServer,
    path: Path,
    stop_event: Event,
    activated: Event,
    startup_errors: list[Exception],
) -> None:
    try:
        _stop_when_file_changes(server, path, stop_event, activated=activated)
    except Exception as exc:
        startup_errors.append(exc)
        activated.set()


def _validate_watch_path(target: Path) -> None:
    # Never create the watched file. Only the final path component may be
    # missing (its later appearance is an exit signal); a missing parent
    # directory is an operator error, not something to walk up from.
    if target.is_dir():
        raise SpiceError(f"spice serve --until path is a directory: {target}")
    if not target.parent.is_dir():
        raise SpiceError(
            f"spice serve --until parent directory is missing: {target.parent}"
        )


def _import_watch() -> Callable[..., Any]:
    module = import_module("watchfiles")
    return cast(Callable[..., Any], getattr(module, "watch"))


def _stop_when_file_changes(
    server: ThreadingHTTPServer,
    path: Path,
    stop_event: Event,
    *,
    activated: Event,
) -> None:
    target = _normalized_watch_path(path)
    baseline = _watch_file_bytes(target)
    for _ in _watch_target_changes(target, stop_event, activated=activated):
        # Events alone are not an exit request: watcher backends may replay
        # writes from just before registration or report metadata-only churn.
        # Only a real content change -- the file appearing, disappearing, or
        # carrying different bytes -- stops the server.
        if _watch_file_bytes(target) == baseline:
            continue
        print(f"spice serve: watched file changed; exiting ({path})")
        server.shutdown()
        return


def _watch_target_changes(
    target: Path,
    stop_event: Event,
    *,
    activated: Event,
) -> Iterator[None]:
    if serve_until_uses_kqueue():
        yield from _watch_target_changes_kqueue(
            target,
            stop_event,
            activated=activated,
        )
        return
    yield from _watch_target_changes_watchfiles(
        target,
        stop_event,
        activated=activated,
    )


def serve_until_uses_kqueue() -> bool:
    from spice.serve.livebuswatch import _HAVE_KQUEUE

    return _HAVE_KQUEUE


def _watch_target_changes_kqueue(
    target: Path,
    stop_event: Event,
    *,
    activated: Event,
) -> Iterator[None]:
    from spice.serve.livebuswatch import _KqueueWatch

    watch = _KqueueWatch()
    try:
        while watch.wait(
            (target, target.parent),
            stop_event,
            activated=activated,
        ):
            yield None
    finally:
        watch.close()


def _watch_target_changes_watchfiles(
    target: Path,
    stop_event: Event,
    *,
    activated: Event,
) -> Iterator[None]:
    watch = _import_watch()
    # Anchor on the parent directory so creation of a missing target is
    # observable. A timeout yield proves native registration before serve
    # readiness without periodically checking the target itself.
    for changes in watch(
        target.parent,
        watch_filter=lambda change, changed_path: _include_change(
            change,
            changed_path,
            target=target,
        ),
        force_polling=False,
        debounce=50,
        stop_event=stop_event,
        recursive=False,
        rust_timeout=WATCHFILES_NATIVE_READY_MS,
        yield_on_timeout=True,
    ):
        if not activated.is_set():
            activated.set()
        if _changes_include_path(changes, target):
            yield None
    if not activated.is_set():
        raise RuntimeError("serve --until watcher stopped before native registration")


def _watch_file_bytes(path: Path) -> bytes | None:
    try:
        return path.read_bytes()
    except OSError:
        return None


def _normalized_watch_path(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def _changes_include_path(changes: Iterable[tuple[object, str]], target: Path) -> bool:
    return any(
        _normalized_watch_path(Path(changed_path)) == target
        for _, changed_path in changes
    )


def _include_change(_change: object, path: str, *, target: Path) -> bool:
    return _normalized_watch_path(Path(path)) == target
