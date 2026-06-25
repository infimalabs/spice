"""Advisory file-lock blocking semantics.

These pin the deterministic, cross-platform half of the contract: a
non-blocking acquire raises ``FileLockUnavailable`` when the lock is already
held, and the lock is released when the context block exits. The platform
divergence on ``blocking=True`` (indefinite wait on POSIX vs ~10s bounded wait
on Windows) is documented in spice/locking.py and is not exercised here because
asserting an indefinite wait would hang.
"""

from __future__ import annotations

import pytest

from spice.locking import FileLockUnavailable, exclusive_lock


def test_non_blocking_exclusive_lock_raises_when_already_held(tmp_path):
    lock_path = tmp_path / "lock"
    with exclusive_lock(lock_path, blocking=True):
        with pytest.raises(FileLockUnavailable):
            with exclusive_lock(lock_path, blocking=False):
                pass


def test_exclusive_lock_releases_on_block_exit(tmp_path):
    lock_path = tmp_path / "lock"
    with exclusive_lock(lock_path, blocking=True):
        pass
    # Released — a subsequent non-blocking acquire now succeeds rather than
    # raising, proving the lock did not outlive its block.
    reacquired = False
    with exclusive_lock(lock_path, blocking=False):
        reacquired = True
    assert reacquired
