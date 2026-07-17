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


@dataclass(frozen=True)
class ContractMutation:
    property: str
    old_value: str
    new_value: str
    timestamp: str


@dataclass(frozen=True)
class _OperationsLogConnection:
    path: Path
    connection: sqlite3.Connection


def operations_db_path() -> Path:
    """Resolve the TaskChampion operations database for one connection attempt."""
    return (config.data_dir() / OPERATIONS_DB_FILENAME).resolve()


def operations_db_uri(path: Path) -> str:
    """Render one resolved database path as a percent-encoded read-only URI."""
    return f"{path.as_uri()}?mode=ro"


@contextmanager
def _connect() -> Iterator[_OperationsLogConnection]:
    """Open and verify the one supported TaskChampion operations-log shape."""
    path = operations_db_path()
    if not path.is_file():
        raise _schema_error(path, "database file is missing")
    connection: sqlite3.Connection | None = None
    try:
        connection = sqlite3.connect(operations_db_uri(path), uri=True)
        table = connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            ("operations",),
        ).fetchone()
        if table is None:
            raise _schema_error(path, "operations table is missing")
        # TaskChampion's uuid is a generated VIRTUAL column, so table_info
        # omits it while table_xinfo exposes the complete queryable shape.
        columns = {
            str(row[1]) for row in connection.execute("PRAGMA table_xinfo(operations)")
        }
        missing = sorted(REQUIRED_OPERATIONS_COLUMNS - columns)
        if missing:
            raise _schema_error(
                path, f"operations table is missing columns: {', '.join(missing)}"
            )
        yield _OperationsLogConnection(path=path, connection=connection)
    except sqlite3.Error as exc:
        raise _schema_error(path, f"SQLite read failed: {exc}") from exc
    finally:
        if connection is not None:
            connection.close()


def _schema_error(path: Path, detail: str) -> SpiceError:
    return SpiceError(
        f"unsupported TaskChampion operations log at {path}: {detail}; "
        "Taskwarrior 3 TaskChampion storage with operations columns "
        "id, uuid, data is required"
    )


def task_version(uuid: str) -> int:
    """Tail operations id for the task: the highest operations.id recorded for it.

    TaskChampion appends per-property operations for every mutation, so a
    task's tail id is a cheap monotonic version — any edit lands a strictly
    higher id. One indexed MAX read; 0 only before the first recorded write.
    """
    with _connect() as log:
        row = log.connection.execute(
            "SELECT MAX(id) FROM operations WHERE uuid = ?", (uuid,)
        ).fetchone()
        return int(row[0]) if row is not None and row[0] is not None else 0


def claim_baseline_id(uuid: str, actor: str) -> int:
    """Operations id of the actor's claim_by write on the task; log tail otherwise.

    Baselining at the claim write means edits landed between claim time and
    the first cadence check are still reported, without persisting a cursor.
    """
    with _connect() as log:
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
    with _connect() as log:
        rows = log.connection.execute(
            "SELECT id, data FROM operations WHERE uuid = ? AND id > ? ORDER BY id",
            (uuid, after_id),
        ).fetchall()
    for op_id, data in rows:
        operation = _decode_operation(log.path, int(op_id), data)
        cursor = int(op_id)
        update = operation.get("Update")
        if not isinstance(update, dict):
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


def _decode_operation(path: Path, operation_id: int, data: object) -> dict[str, object]:
    """Decode one supported TaskChampion operation or reject its exact row."""
    if not isinstance(data, str):
        raise _schema_error(
            path,
            f"operation {operation_id} data is {type(data).__name__}, not JSON text",
        )
    try:
        operation = json.loads(data)
    except json.JSONDecodeError as exc:
        raise _schema_error(
            path,
            f"operation {operation_id} data is not valid JSON: {exc.msg}",
        ) from exc
    if not isinstance(operation, dict):
        raise _schema_error(
            path,
            f"operation {operation_id} data is not a JSON object",
        )
    return operation


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
