"""Semantic task lifecycle transitions read from the canonical task plane.

TaskChampion is the allocation authority, and it already keeps the history:
every task mutation commits as one transaction that appends a per-property
Update operation for each field it touched. Claim, phase advance, review,
completion, and drain are therefore recoverable by folding those properties
back into task state and reporting where the state moved — no second series
has to be written beside the authoritative one to observe it.

One transaction yields at most one transition. A claim renewal rewrites the
lease on every cadence tick, and a single command writes a dozen properties
at once; neither moves lifecycle state, so neither is an event. That is the
difference between reading history and mirroring writes: a command retried
into the same state is one transition here and was two rows in the mirror.
"""

from __future__ import annotations

import threading
from collections.abc import Iterable, Iterator, Mapping, Sequence
from dataclasses import dataclass, field
from datetime import datetime
from enum import StrEnum
from pathlib import Path

from spice.tasks import opslog

CLAIM_ACTOR_PROPERTY = "claim_by"
PHASE_PROPERTY = "phase"
PHASE_INDEX_PROPERTY = "phase_i"
REVIEW_ACTOR_PROPERTY = "review_by"
REVIEW_FINDING_PROPERTY = "review_finding"
STATUS_PROPERTY = "status"
MODIFIED_PROPERTY = "modified"

# The properties a lifecycle state is folded from, plus the one that only names
# an actor and the one that dates the transaction. Everything else a mutation
# writes -- lease deadlines, context links, annotations, dependency edges --
# moves no lifecycle state.
TRACKED_PROPERTIES = frozenset(
    {
        CLAIM_ACTOR_PROPERTY,
        PHASE_PROPERTY,
        PHASE_INDEX_PROPERTY,
        REVIEW_ACTOR_PROPERTY,
        REVIEW_FINDING_PROPERTY,
        STATUS_PROPERTY,
        MODIFIED_PROPERTY,
    }
)

COMPLETED_STATUS = "completed"
DELETED_STATUS = "deleted"

_OPERATION_COLUMNS = (
    "id, "
    "uuid, "
    "json_extract(data, '$.Update.property') AS property, "
    "json_extract(data, '$.Update.value') AS value, "
    "json_extract(data, '$.Update.timestamp') AS timestamp"
)


class TaskTransitionKind(StrEnum):
    """The lifecycle movements a task plane records, named as Serve reads them."""

    CLAIM = "claim"
    PHASE_ADVANCE = "phaseAdvance"
    REVIEW = "review"
    COMPLETE = "complete"
    DRAIN = "drain"


# A task leaves the open board exactly once, by completing or by being
# deleted. Both drain it; only the first is a completion.
DRAINING_KINDS = frozenset({TaskTransitionKind.COMPLETE, TaskTransitionKind.DRAIN})
# Movement on a task an actor already holds and still holds afterwards: work
# in flight, as distinct from taking the task up or letting it go.
ACTIVE_KINDS = frozenset({TaskTransitionKind.PHASE_ADVANCE, TaskTransitionKind.REVIEW})


@dataclass(frozen=True, slots=True)
class TaskLifecycleState:
    """Where a task stands: status, position in its flow, holder, verdict."""

    status: str = ""
    phase: str = ""
    phase_index: int = 0
    claimed_by: str = ""
    review_finding: str = ""

    @property
    def label(self) -> str:
        """One stable rendering, for reports that show a transition's ends."""
        holder = self.claimed_by or "-"
        finding = self.review_finding or "-"
        return (
            f"{self.status or '-'}/{self.phase or '-'}[{self.phase_index}] "
            f"claim={holder} review={finding}"
        )


@dataclass(frozen=True, slots=True)
class TaskTransition:
    """One semantic lifecycle movement, identified by where history recorded it.

    ``id`` is the operations id of the transaction's first operation: stable
    across reads, monotonic with time, and unique because a transaction opens
    exactly once.
    """

    id: int
    task_id: str
    kind: TaskTransitionKind
    actor: str
    at: float
    old_state: TaskLifecycleState
    new_state: TaskLifecycleState


@dataclass(frozen=True, slots=True)
class _Operation:
    id: int
    task_id: str
    property: str
    value: str
    timestamp: str


@dataclass(frozen=True, slots=True)
class _History:
    """Everything folded from one operations log up to a tail id.

    ``states`` is the fold's resume point: the lifecycle state each task stood
    in after the last folded transaction, which is exactly what a later read of
    the operations appended since then commits against.
    """

    tail_id: int = 0
    states: Mapping[str, TaskLifecycleState] = field(default_factory=dict)
    transitions: tuple[TaskTransition, ...] = ()


_history_lock = threading.Lock()
_histories: dict[Path, _History] = {}


def task_transitions(task_ids: Iterable[str] = ()) -> tuple[TaskTransition, ...]:
    """Ordered semantic transitions for the named tasks, or for every task.

    Folding starts at each task's first recorded operation, so the reported
    old state is the real prior state rather than an assumption about where a
    read window happened to open. Ordering is by operations id, which is the
    order the task plane committed the transactions in.
    """
    selected = tuple(dict.fromkeys(str(task_id) for task_id in task_ids if task_id))
    if selected:
        with opslog.connect() as log:
            return _fold_log(log, selected, _History()).transitions
    return _whole_log_transitions()


def _whole_log_transitions() -> tuple[TaskTransition, ...]:
    """Fold every task's history, reusing what an earlier read already folded.

    An operations log only ever grows, so a fold that stopped at a tail id is
    still exactly right for everything at or below it. Holding the fold's
    resume state lets a later read cover only the operations appended since,
    which keeps a whole-plane read affordable on a per-request path.
    """
    with opslog.connect() as log:
        tail_id = _tail_id(log)
        with _history_lock:
            cached = _histories.get(log.path, _History())
        if cached.tail_id == tail_id:
            return cached.transitions
        if cached.tail_id > tail_id:
            # The log lost operations, so nothing folded from it still holds.
            cached = _History()
        history = _fold_log(log, (), cached, tail_id=tail_id)
    with _history_lock:
        current = _histories.get(log.path)
        if current is None or current.tail_id < history.tail_id:
            _histories[log.path] = history
    return history.transitions


def _fold_log(
    log: opslog.OperationsLog,
    task_ids: tuple[str, ...],
    resume: _History,
    *,
    tail_id: int = 0,
) -> _History:
    """Extend one fold with every transaction the log records past its tail."""
    states = dict(resume.states)
    transitions = list(resume.transitions)
    for transaction in _transactions(log, task_ids, resume.tail_id, tail_id):
        task_id = transaction[0].task_id
        old_state = states.get(task_id, TaskLifecycleState())
        new_state = _fold(old_state, transaction)
        states[task_id] = new_state
        transition = _transition(transaction, old_state, new_state)
        if transition is not None:
            transitions.append(transition)
        tail_id = max(tail_id, transaction[-1].id)
    return _History(tail_id=tail_id, states=states, transitions=tuple(transitions))


def _tail_id(log: opslog.OperationsLog) -> int:
    row = log.connection.execute("SELECT MAX(id) FROM operations").fetchone()
    return int(row[0]) if row is not None and row[0] is not None else 0


def _transactions(
    log: opslog.OperationsLog,
    task_ids: tuple[str, ...],
    after_id: int,
    through_id: int,
) -> Iterator[tuple[_Operation, ...]]:
    """Group the log's operations back into the transactions that wrote them.

    TaskChampion writes one task's properties as a contiguous run of ids and
    separates transactions with an ``UndoPoint`` operation carrying no uuid.
    Selecting only uuid-bearing rows therefore leaves a gap in the id sequence
    exactly at each transaction boundary, and a run of consecutive ids under
    one uuid is exactly one transaction's writes to one task.
    """
    current: list[_Operation] = []
    previous_id = 0
    previous_task = ""
    for row in log.connection.execute(*_select(task_ids, after_id, through_id)):
        operation_id = int(row[0])
        task_id = str(row[1] or "")
        if current and (task_id != previous_task or operation_id != previous_id + 1):
            yield _closed(log, current)
            current = []
        previous_id, previous_task = operation_id, task_id
        name = str(row[2] or "")
        # An untracked write still holds its place in the run so the
        # contiguity that delimits transactions stays readable.
        current.append(
            _Operation(
                id=operation_id,
                task_id=task_id,
                property=name if name in TRACKED_PROPERTIES else "",
                value=str(row[3] or ""),
                timestamp=str(row[4] or ""),
            )
        )
    if current:
        yield _closed(log, current)


def _select(
    task_ids: tuple[str, ...], after_id: int, through_id: int
) -> tuple[str, tuple[object, ...]]:
    """Build the one ordered read a fold makes, scoped to what it still needs.

    Reading through a tail id already observed keeps the fold and the tail it
    is remembered by describing the same operations, even when the task plane
    commits again mid-read.
    """
    where = ["uuid IS NOT NULL"]
    parameters: list[object] = []
    if task_ids:
        where[0] = f"uuid IN ({','.join('?' for _task_id in task_ids)})"
        parameters.extend(task_ids)
    if after_id:
        where.append("id > ?")
        parameters.append(after_id)
    if through_id:
        where.append("id <= ?")
        parameters.append(through_id)
    sql = (
        f"SELECT {_OPERATION_COLUMNS} FROM operations "
        f"WHERE {' AND '.join(where)} ORDER BY id"
    )
    return sql, tuple(parameters)


def _closed(
    log: opslog.OperationsLog, operations: Sequence[_Operation]
) -> tuple[_Operation, ...]:
    """Close one grouped run, refusing a run that holds two commands' writes.

    A command stamps ``modified`` at most once, so a run carrying two of them
    means two commands were read as one and every count derived from this log
    would be quietly wrong. A run carrying none is ordinary and common: the
    stamp has one-second resolution and the log records an operation only where
    a value changed, so a command finishing inside the same second as the last
    one writes no stamp at all. Contiguous ids under one uuid are what delimit
    a transaction; this is the corroborating check, not the boundary.
    """
    stamps = sum(1 for item in operations if item.property == MODIFIED_PROPERTY)
    if stamps > 1:
        raise opslog.unsupported_schema_error(
            log.path,
            f"operations {operations[0].id}-{operations[-1].id} carry {stamps} "
            "modified writes; one transaction writes at most one",
        )
    return tuple(operations)


def _fold(
    state: TaskLifecycleState, transaction: Sequence[_Operation]
) -> TaskLifecycleState:
    """Apply one transaction's tracked writes to the state it commits against."""
    status = state.status
    phase = state.phase
    phase_index = state.phase_index
    claimed_by = state.claimed_by
    review_finding = state.review_finding
    for operation in transaction:
        if operation.property == STATUS_PROPERTY:
            status = operation.value
        elif operation.property == PHASE_PROPERTY:
            phase = operation.value
        elif operation.property == PHASE_INDEX_PROPERTY:
            phase_index = _phase_index(operation.value)
        elif operation.property == CLAIM_ACTOR_PROPERTY:
            claimed_by = operation.value
        elif operation.property == REVIEW_FINDING_PROPERTY:
            review_finding = operation.value
    return TaskLifecycleState(
        status=status,
        phase=phase,
        phase_index=phase_index,
        claimed_by=claimed_by,
        review_finding=review_finding,
    )


def _transition(
    transaction: Sequence[_Operation],
    old_state: TaskLifecycleState,
    new_state: TaskLifecycleState,
) -> TaskTransition | None:
    """Name the one lifecycle movement a transaction made, if it made one."""
    kind = _transition_kind(old_state, new_state)
    if kind is None:
        return None
    return TaskTransition(
        id=transaction[0].id,
        task_id=transaction[0].task_id,
        kind=kind,
        actor=_transition_actor(kind, transaction, old_state, new_state),
        at=_transition_time(transaction),
        old_state=old_state,
        new_state=new_state,
    )


def _transition_kind(
    old_state: TaskLifecycleState, new_state: TaskLifecycleState
) -> TaskTransitionKind | None:
    """Classify a state move, most consequential movement first.

    A phase advance releases the claim in the same transaction that moves the
    phase, and a review verdict is recorded while the reviewer still holds the
    task, so the order below decides which single fact the transaction is
    about.
    """
    if new_state.status != old_state.status:
        if new_state.status == DELETED_STATUS:
            return TaskTransitionKind.DRAIN
        if new_state.status == COMPLETED_STATUS:
            return TaskTransitionKind.COMPLETE
    acquired_finding = (
        new_state.review_finding
        and new_state.review_finding != old_state.review_finding
    )
    if acquired_finding:
        return TaskTransitionKind.REVIEW
    moved_phase = (new_state.phase, new_state.phase_index) != (
        old_state.phase,
        old_state.phase_index,
    )
    if moved_phase and old_state.phase:
        return TaskTransitionKind.PHASE_ADVANCE
    if new_state.claimed_by and new_state.claimed_by != old_state.claimed_by:
        return TaskTransitionKind.CLAIM
    return None


def _transition_actor(
    kind: TaskTransitionKind,
    transaction: Sequence[_Operation],
    old_state: TaskLifecycleState,
    new_state: TaskLifecycleState,
) -> str:
    """The actor the task plane itself credits with the movement.

    A claim names its new holder. Every other movement is made by whoever held
    the task going in — the same transaction that advances a phase clears the
    claim, so the holder survives only in the state the transaction committed
    against.
    """
    if kind is TaskTransitionKind.CLAIM:
        return new_state.claimed_by
    if kind is TaskTransitionKind.REVIEW:
        reviewer = _written_value(transaction, REVIEW_ACTOR_PROPERTY)
        if reviewer:
            return reviewer
    return old_state.claimed_by or new_state.claimed_by


def _written_value(transaction: Sequence[_Operation], name: str) -> str:
    for operation in transaction:
        if operation.property == name:
            return operation.value
    return ""


def _transition_time(transaction: Sequence[_Operation]) -> float:
    """When the task plane recorded the transaction, in epoch seconds.

    Each operation carries its own stamp, taken as the command builds the
    transaction, so one run's stamps rise across a few microseconds and the
    first is where the transaction opened. Taskwarrior's own ``modified``
    epoch is the fallback for a log written without operation timestamps.
    """
    for operation in transaction:
        moment = _epoch_seconds(operation.timestamp)
        if moment is not None:
            return moment
    return _epoch_seconds(_written_value(transaction, MODIFIED_PROPERTY)) or 0.0


def _epoch_seconds(timestamp: str) -> float | None:
    if not timestamp:
        return None
    try:
        return datetime.fromisoformat(timestamp).timestamp()
    except ValueError:
        pass
    try:
        return float(timestamp)
    except ValueError:
        return None


def _phase_index(value: str) -> int:
    try:
        return int(value)
    except ValueError:
        return 0
