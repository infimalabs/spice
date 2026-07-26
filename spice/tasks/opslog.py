"""Contract-field mutation reads from the TaskChampion operations log.

TaskChampion (the Taskwarrior 3 storage engine) records every task mutation
in the backend SQLite database as a per-property Update operation carrying
uuid, property, old value, new value, and timestamp, indexed by uuid. That
log is the change signal for notifying a working agent when its claimed
task's contract fields move: no UDA, no field hash, no daemon — one indexed
read-only query against data Taskwarrior already writes.
"""

from __future__ import annotations

import json
import sqlite3
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from spice.errors import SpiceError
from spice.tasks import config

OPERATIONS_DB_FILENAME = "taskchampion.sqlite3"

# Operator/reviewer-meaningful fields only. description is the Taskwarrior
# native property carrying the task title; task_description carries the
# description body. Claim bookkeeping (claim_*, start, modified) mutates on
# every renewal and must never trigger a notice; annotations (annotation_<ts>)
# and dep_<uuid> markers shadow fields already covered here (notes are chatty,
# depends carries the aggregate edge list).
CONTRACT_PROPERTIES = frozenset(
    {
        "description",
        "task_description",
        "acceptance",
        "priority",
        "project",
        "phase",
        "depends",
        "review_finding",
        "review_note",
    }
    | {f"phase_{slot}" for slot in range(config.PHASE_SLOT_COUNT)}
)

VALUE_PREVIEW_CHARS = 60
REQUIRED_OPERATIONS_COLUMNS = frozenset({"id", "uuid", "data"})
_OPERATION_TIMESTAMP = "json_extract(data, '$.Update.timestamp')"


@dataclass(frozen=True)
class ContractMutation:
    property: str
    old_value: str
    new_value: str
    timestamp: str


@dataclass(frozen=True)
class OperationsLogIdentity:
    device: int
    inode: int


@dataclass(frozen=True)
class OperationsLog:
    path: Path
    identity: OperationsLogIdentity
    connection: sqlite3.Connection


def operations_db_path() -> Path:
    """Resolve the TaskChampion operations database for one connection attempt."""
    return (config.data_dir() / OPERATIONS_DB_FILENAME).resolve()


def operations_db_uri(path: Path) -> str:
    """Render one resolved database path as a percent-encoded read-only URI."""
    return f"{path.as_uri()}?mode=ro"


@contextmanager
def connect() -> Iterator[OperationsLog]:
    """Open and verify the one supported TaskChampion operations-log shape.

    Every read of this log opens through here, so an unreadable database, a
    missing table, a missing column, and a SQL failure mid-read all surface as
    the same named unsupported-schema error rather than as an empty result.
    Device and inode are verified across connection setup so caches can tell
    ordinary appends from an atomic same-path database replacement.
    """
    path = operations_db_path()
    if not path.is_file():
        raise unsupported_schema_error(path, "database file is missing")
    identity = _operations_log_identity(path)
    connection: sqlite3.Connection | None = None
    try:
        connection = sqlite3.connect(operations_db_uri(path), uri=True)
        table = connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            ("operations",),
        ).fetchone()
        if table is None:
            raise unsupported_schema_error(path, "operations table is missing")
        # TaskChampion's uuid is a generated VIRTUAL column, so table_info
        # omits it while table_xinfo exposes the complete queryable shape.
        columns = {
            str(row[1]) for row in connection.execute("PRAGMA table_xinfo(operations)")
        }
        missing = sorted(REQUIRED_OPERATIONS_COLUMNS - columns)
        if missing:
            raise unsupported_schema_error(
                path, f"operations table is missing columns: {', '.join(missing)}"
            )
        if _operations_log_identity(path) != identity:
            raise unsupported_schema_error(path, "database file changed while opening")
        yield OperationsLog(path=path, identity=identity, connection=connection)
    except sqlite3.Error as exc:
        raise unsupported_schema_error(path, f"SQLite read failed: {exc}") from exc
    finally:
        if connection is not None:
            connection.close()


def _operations_log_identity(path: Path) -> OperationsLogIdentity:
    try:
        status = path.stat()
    except OSError as exc:
        raise unsupported_schema_error(
            path, f"database file identity is unavailable: {exc}"
        ) from exc
    return OperationsLogIdentity(device=status.st_dev, inode=status.st_ino)


def unsupported_schema_error(path: Path, detail: str) -> SpiceError:
    return SpiceError(
        f"unsupported TaskChampion operations log at {path}: {detail}; "
        "Taskwarrior 3 TaskChampion storage with operations columns "
        "id, uuid, data is required"
    )


def latest_operation_epoch() -> float | None:
    """Epoch seconds of the newest operation the log holds, or None for no answer.

    This is what "the task authority last recorded something" means, and it is
    deliberately not the store's modification time. Taskwarrior rewrites the
    database on every read, so a bare export or `spice task list` that changed
    nothing still moves every timestamp the filesystem keeps; only the log's own
    contents distinguish a read from a write.

    Newest is taken by operations id rather than by comparing the recorded
    timestamps, because id is the order the task plane committed in and the
    stamps are text whose fractional digits need not all be the same width.
    Operations carrying no stamp — the UndoPoint that separates transactions —
    are skipped rather than ending the search, so a log whose tail is a
    separator still answers the transaction that separator closed. None is for
    a log holding no stamped operation at all.

    None is also the answer for a stamp this cannot turn into an instant. A
    zoneless one is refused rather than read as local time, which would date
    the authority differently on every machine that read it; a freshness a
    reader compares against now is worth omitting instead of guessing.
    """
    with connect() as log:
        row = log.connection.execute(
            f"SELECT {_OPERATION_TIMESTAMP} FROM operations "
            f"WHERE {_OPERATION_TIMESTAMP} IS NOT NULL ORDER BY id DESC LIMIT 1"
        ).fetchone()
    if row is None or row[0] is None:
        return None
    try:
        recorded = datetime.fromisoformat(str(row[0]))
    except ValueError:
        return None
    return None if recorded.tzinfo is None else recorded.timestamp()


def task_version(uuid: str) -> int:
    """Tail operations id for the task: the highest operations.id recorded for it.

    TaskChampion appends per-property operations for every mutation, so a
    task's tail id is a cheap monotonic version — any edit lands a strictly
    higher id. One indexed MAX read; 0 only before the first recorded write.
    """
    with connect() as log:
        row = log.connection.execute(
            "SELECT MAX(id) FROM operations WHERE uuid = ?", (uuid,)
        ).fetchone()
        return int(row[0]) if row is not None and row[0] is not None else 0


def claim_baseline_id(uuid: str, actor: str) -> int:
    """Operations id of the actor's claim_by write on the task; log tail otherwise.

    Baselining at the claim write means edits landed between claim time and
    the first cadence check are still reported, without persisting a cursor.
    """
    with connect() as log:
        row = log.connection.execute(
            "SELECT MAX(id) FROM operations WHERE uuid = ?"
            " AND json_extract(data, '$.Update.property') = 'claim_by'"
            " AND json_extract(data, '$.Update.value') = ?",
            (uuid, actor),
        ).fetchone()
        if row is not None and row[0] is not None:
            return int(row[0])
        tail = log.connection.execute("SELECT MAX(id) FROM operations").fetchone()
        return int(tail[0]) if tail is not None and tail[0] is not None else 0


def contract_mutations_since(
    uuid: str, after_id: int
) -> tuple[int, list[ContractMutation]]:
    """Ordered contract-field mutations for uuid strictly after an operations id.

    Returns the highest operations id scanned (the caller's next cursor, so
    renewal-only churn still advances it) with the contract mutations found.
    """
    cursor = after_id
    mutations: list[ContractMutation] = []
    with connect() as log:
        rows = log.connection.execute(
            "SELECT id, data FROM operations WHERE uuid = ? AND id > ? ORDER BY id",
            (uuid, after_id),
        ).fetchall()
    for op_id, data in rows:
        update = _decode_update(log.path, int(op_id), data)
        cursor = int(op_id)
        if update is None:
            continue
        prop = str(update.get("property") or "")
        if prop not in CONTRACT_PROPERTIES:
            continue
        mutations.append(
            ContractMutation(
                property=prop,
                old_value=str(update.get("old_value") or ""),
                new_value=str(update.get("value") or ""),
                timestamp=str(update.get("timestamp") or ""),
            )
        )
    return cursor, mutations


def _decode_update(
    path: Path, operation_id: int, data: object
) -> dict[str, object] | None:
    """Decode one Update object; return None for a valid non-Update operation."""
    if not isinstance(data, str):
        raise unsupported_schema_error(
            path,
            f"operation {operation_id} data is {type(data).__name__}, not JSON text",
        )
    try:
        operation = json.loads(data)
    except json.JSONDecodeError as exc:
        raise unsupported_schema_error(
            path,
            f"operation {operation_id} data is not valid JSON: {exc.msg}",
        ) from exc
    if not isinstance(operation, dict):
        raise unsupported_schema_error(
            path,
            f"operation {operation_id} data is not a JSON object",
        )
    if "Update" not in operation:
        return None
    update = operation["Update"]
    if not isinstance(update, dict):
        raise unsupported_schema_error(
            path,
            f"operation {operation_id} Update is {type(update).__name__}, "
            "not a JSON object",
        )
    return update


def render_notice(mutations: list[ContractMutation]) -> str:
    return "; ".join(
        f"{item.property}: {_preview(item.old_value)} -> {_preview(item.new_value)}"
        for item in mutations
    )


def _preview(value: str) -> str:
    text = " ".join(value.split())
    if not text:
        return "-"
    if len(text) <= VALUE_PREVIEW_CHARS:
        return text
    return text[: VALUE_PREVIEW_CHARS - 1] + "…"
