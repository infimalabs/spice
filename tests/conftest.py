"""Suite-wide opt-in audit: every SQLite connection closes deterministically.

Set SPICE_SQLITE_LIFECYCLE_AUDIT to a writable file path to count every
in-process sqlite3 connection the suite opens against explicit closes. Each
pytest process (xdist workers included) forces a garbage-collection pass at
session finish and appends one `opened=N closed=N` line to that file. A
resource-clean run shows equal counts on every line: implicit destructor
cleanup bypasses the counted close, so a connection left to garbage
collection surfaces as an opened/closed imbalance.
"""

from __future__ import annotations

import gc
import os
import sqlite3
import threading

SQLITE_LIFECYCLE_AUDIT_ENV = "SPICE_SQLITE_LIFECYCLE_AUDIT"  # env-policy: allow

_counts_lock = threading.Lock()
_counts = {"opened": 0, "closed": 0}
_real_connect = sqlite3.connect


class _AuditedConnection(sqlite3.Connection):
    def __init__(self, *args, **kwargs) -> None:
        super().__init__(*args, **kwargs)
        self._audit_close_recorded = False
        with _counts_lock:
            _counts["opened"] += 1

    def close(self) -> None:
        if self._audit_close_recorded:
            super().close()
            return
        self._audit_close_recorded = True
        with _counts_lock:
            _counts["closed"] += 1
        super().close()


def _audited_connect(*args, **kwargs):
    kwargs.setdefault("factory", _AuditedConnection)
    return _real_connect(*args, **kwargs)


def pytest_configure(config) -> None:
    if os.environ.get(SQLITE_LIFECYCLE_AUDIT_ENV):  # env-policy: allow
        sqlite3.connect = _audited_connect


def pytest_sessionfinish(session, exitstatus) -> None:
    audit_path = os.environ.get(SQLITE_LIFECYCLE_AUDIT_ENV)  # env-policy: allow
    if not audit_path:
        return
    gc.collect()
    with _counts_lock:
        line = f"opened={_counts['opened']} closed={_counts['closed']}\n"
    with open(audit_path, "a", encoding="utf-8") as handle:
        handle.write(line)
