from __future__ import annotations

import argparse
from collections.abc import Iterable
from dataclasses import dataclass
from http.server import ThreadingHTTPServer
from pathlib import Path
import stat
from threading import Event, Thread

from spice.errors import SpiceError


@dataclass(frozen=True)
class FileWatchSnapshot:
    exists: bool
    is_file: bool
    size: int | None
    modified_ns: int | None
    metadata_changed_ns: int | None
    mode: int | None
    device: int | None
    inode: int | None
    owner: int | None
    group: int | None
    stat_error: str | None


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


def _import_watch():
    from watchfiles import watch

    return watch


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


def snapshot_file_watch_path(path: Path) -> FileWatchSnapshot:
    try:
        stat_result = path.stat()
    except FileNotFoundError:
        return FileWatchSnapshot(
            exists=False,
            is_file=False,
            size=None,
            modified_ns=None,
            metadata_changed_ns=None,
            mode=None,
            device=None,
            inode=None,
            owner=None,
            group=None,
            stat_error=None,
        )
    except OSError as exc:
        return FileWatchSnapshot(
            exists=False,
            is_file=False,
            size=None,
            modified_ns=None,
            metadata_changed_ns=None,
            mode=None,
            device=None,
            inode=None,
            owner=None,
            group=None,
            stat_error=f"{type(exc).__name__}:{exc.errno}",
        )
    return FileWatchSnapshot(
        exists=True,
        is_file=stat.S_ISREG(stat_result.st_mode),
        size=stat_result.st_size,
        modified_ns=stat_result.st_mtime_ns,
        metadata_changed_ns=stat_result.st_ctime_ns,
        mode=stat_result.st_mode,
        device=getattr(stat_result, "st_dev", None),
        inode=getattr(stat_result, "st_ino", None),
        owner=getattr(stat_result, "st_uid", None),
        group=getattr(stat_result, "st_gid", None),
        stat_error=None,
    )


def file_watch_path_changed(path: Path, baseline: FileWatchSnapshot) -> bool:
    return snapshot_file_watch_path(path) != baseline
