from __future__ import annotations

import argparse
from collections.abc import Iterable
from http.server import ThreadingHTTPServer
from importlib import import_module
from pathlib import Path
from threading import Event, Thread
from typing import Any, Callable, cast

from spice.errors import SpiceError


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
    thread = Thread(
        target=_stop_when_file_changes,
        args=(server, path, stop_event),
        name="spice-serve-file-watch",
        daemon=True,
    )
    thread.start()
    return thread


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
) -> None:
    watch = _import_watch()
    target = _normalized_watch_path(path)
    # Anchor on the parent directory (validated to exist) so the watch also
    # covers a not-yet-created file; the filter narrows to the exact target.
    baseline = _watch_file_bytes(target)
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
    ):
        if not _changes_include_path(changes, target):
            continue
        # Events alone are not an exit request: macOS FSEvents replays
        # writes from just before the watch started (a launcher writing the
        # file moments before serve boots) and fires on metadata-only churn.
        # Only a real content change -- the file appearing, disappearing, or
        # carrying different bytes -- stops the server.
        if _watch_file_bytes(target) == baseline:
            continue
        print(f"spice serve: watched file changed; exiting ({path})")
        server.shutdown()
        return


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
