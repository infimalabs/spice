"""Cross-platform advisory file locks (flock on POSIX, msvcrt on Windows).

Blocking semantics differ by platform, and the difference is intrinsic to the
underlying primitive, not a spice choice:

* ``blocking=True``
    * POSIX: ``flock(LOCK_EX)`` waits **indefinitely** until the lock is free.
    * Windows: ``msvcrt.locking(LK_LOCK)`` retries internally for ~10 seconds
      and then raises ``OSError`` if the lock is still contended — a **bounded**
      wait, not an indefinite one. The timeout error is not ``EACCES``/``EAGAIN``
      so it surfaces as a raw ``OSError``, not ``FileLockUnavailable``.
* ``blocking=False`` (both platforms): attempt once and, if the lock is held,
    raise ``FileLockUnavailable`` immediately.

POSIX is the load-bearing platform; Windows is aspirational. Callers that need a
bounded wait on POSIX must pass ``blocking=False`` and retry themselves rather
than assume ``blocking=True`` ever times out.

Library seam: target-repo tools may import `exclusive_lock`,
`lock_fd_exclusive`, `unlock_fd`, and `FileLockUnavailable`; underscored names
remain private.
"""

from __future__ import annotations

import errno
import importlib
import os
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

WINDOWS_LOCK_BYTES = 1


class FileLockUnavailable(RuntimeError):
    pass


def lock_fd_exclusive(fd: int, *, blocking: bool) -> None:
    if os.name == "nt":
        _lock_fd_windows(fd, blocking=blocking)
        return
    _lock_fd_posix(fd, blocking=blocking)


def unlock_fd(fd: int) -> None:
    if os.name == "nt":
        _unlock_fd_windows(fd)
        return
    _unlock_fd_posix(fd)


@contextmanager
def exclusive_lock(path: Path, *, blocking: bool = True) -> Iterator[None]:
    """Hold an exclusive lock on `path` (created if missing) for the block.

    With ``blocking=True`` this waits indefinitely on POSIX but only ~10s on
    Windows before raising ``OSError``; with ``blocking=False`` it raises
    ``FileLockUnavailable`` at once if the lock is held. See the module
    docstring for the full platform contract.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = path.open("a+")
    try:
        lock_fd_exclusive(handle.fileno(), blocking=blocking)
        try:
            yield
        finally:
            unlock_fd(handle.fileno())
    finally:
        handle.close()


def _lock_fd_posix(fd: int, *, blocking: bool) -> None:
    fcntl = _posix_fcntl_module()
    flags = fcntl.LOCK_EX
    if not blocking:
        flags |= fcntl.LOCK_NB
    try:
        fcntl.flock(fd, flags)
    except BlockingIOError as exc:
        raise FileLockUnavailable from exc
    except OSError as exc:
        if exc.errno in (errno.EACCES, errno.EAGAIN):
            raise FileLockUnavailable from exc
        raise


def _unlock_fd_posix(fd: int) -> None:
    fcntl = _posix_fcntl_module()
    fcntl.flock(fd, fcntl.LOCK_UN)


def _lock_fd_windows(fd: int, *, blocking: bool) -> None:
    msvcrt = _windows_locking_module()
    mode = msvcrt.LK_LOCK if blocking else msvcrt.LK_NBLCK
    try:
        _ensure_windows_lock_range(fd)
        os.lseek(fd, 0, os.SEEK_SET)
        msvcrt.locking(fd, mode, WINDOWS_LOCK_BYTES)
    except OSError as exc:
        if exc.errno in (errno.EACCES, errno.EAGAIN):
            raise FileLockUnavailable from exc
        raise


def _unlock_fd_windows(fd: int) -> None:
    msvcrt = _windows_locking_module()
    os.lseek(fd, 0, os.SEEK_SET)
    msvcrt.locking(fd, msvcrt.LK_UNLCK, WINDOWS_LOCK_BYTES)


def _ensure_windows_lock_range(fd: int) -> None:
    original_position = os.lseek(fd, 0, os.SEEK_CUR)
    end_position = os.lseek(fd, 0, os.SEEK_END)
    if end_position < WINDOWS_LOCK_BYTES:
        os.write(fd, b"\0" * (WINDOWS_LOCK_BYTES - end_position))
    os.lseek(fd, original_position, os.SEEK_SET)


def _posix_fcntl_module() -> Any:
    return importlib.import_module("fcntl")


def _windows_locking_module() -> Any:
    return importlib.import_module("msvcrt")
