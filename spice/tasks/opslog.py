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
from dataclasses import dataclass

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
        "phase",
        "depends",
        "review_finding",
        "review_note",
    }
    | {f"phase_{slot}" for slot in range(config.PHASE_SLOT_COUNT)}
)

VALUE_PREVIEW_CHARS = 60


@dataclass(frozen=True)
class ContractMutation:
    property: str
    old_value: str
    new_value: str
    timestamp: str


def operations_db_path() -> str:
    return str(config.data_dir() / OPERATIONS_DB_FILENAME)


def _connect() -> sqlite3.Connection:
    return sqlite3.connect(f"file:{operations_db_path()}?mode=ro", uri=True)


def claim_baseline_id(uuid: str, actor: str) -> int:
    """Operations id of the actor's claim_by write on the task; log tail otherwise.

    Baselining at the claim write means edits landed between claim time and
    the first cadence check are still reported, without persisting a cursor.
    """
    con = _connect()
    try:
        row = con.execute(
            "SELECT MAX(id) FROM operations WHERE uuid = ?"
            " AND json_extract(data, '$.Update.property') = 'claim_by'"
            " AND json_extract(data, '$.Update.value') = ?",
            (uuid, actor),
        ).fetchone()
        if row is not None and row[0] is not None:
            return int(row[0])
        tail = con.execute("SELECT MAX(id) FROM operations").fetchone()
        return int(tail[0]) if tail is not None and tail[0] is not None else 0
    finally:
        con.close()


def contract_mutations_since(
    uuid: str, after_id: int
) -> tuple[int, list[ContractMutation]]:
    """Ordered contract-field mutations for uuid strictly after an operations id.

    Returns the highest operations id scanned (the caller's next cursor, so
    renewal-only churn still advances it) with the contract mutations found.
    """
    cursor = after_id
    mutations: list[ContractMutation] = []
    con = _connect()
    try:
        rows = con.execute(
            "SELECT id, data FROM operations WHERE uuid = ? AND id > ? ORDER BY id",
            (uuid, after_id),
        ).fetchall()
    finally:
        con.close()
    for op_id, data in rows:
        cursor = int(op_id)
        operation = json.loads(data)
        update = operation.get("Update") if isinstance(operation, dict) else None
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
