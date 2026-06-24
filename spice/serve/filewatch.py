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
    _initialize_watch_path(path)
    print(f"spice serve: watching {path} for exit")
    thread = Thread(
        target=_stop_when_file_changes,
        args=(server, path, stop_event),
        name="spice-serve-file-watch",
        daemon=True,
    )
    thread.start()
    return thread


def _initialize_watch_path(path: Path) -> None:
    if path.exists():
        return
    try:
        path.touch()
    except OSError as exc:
        raise SpiceError(
            f"spice serve --until path could not be initialized: {path}: {exc}"
        ) from exc


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
    root = _watch_root_for(path)
    for changes in watch(
        root,
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
        if _changes_include_path(changes, target):
            print(f"spice serve: watched file changed; exiting ({path})")
            server.shutdown()
            return


def _watch_root_for(path: Path) -> Path:
    return path


def _normalized_watch_path(path: Path) -> Path:
    return path.expanduser().resolve(strict=False)


def _changes_include_path(changes: Iterable[tuple[object, str]], target: Path) -> bool:
    return any(
        _normalized_watch_path(Path(changed_path)) == target
        for _, changed_path in changes
    )


def _include_change(_change: object, path: str, *, target: Path) -> bool:
    return _normalized_watch_path(Path(path)) == target
