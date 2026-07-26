"""Revision-coherent observation of the current task board."""

from __future__ import annotations

import threading
import time
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from types import MappingProxyType
from typing import Any, cast

from spice.errors import SpiceError
from spice.tasks import config as task_config
from spice.tasks import tw
from spice.tasks.opslog import OPERATIONS_DB_FILENAME

TASK_FILTER_STATE_COUNT_FIELDS = (
    "openTaskCount",
    "readyTaskCount",
    "inFlightTaskCount",
    "blockedTaskCount",
    "deferredTaskCount",
)
TASK_ACTOR_FIELDS = ("claim_by", "claim_thread", "review_author", "review_by")
TASK_BOARD_OBSERVATION_TIMEOUT_SECONDS = 2 * (
    tw.TASK_COMMAND_TIMEOUT_SECONDS + task_config.TASK_BOOTSTRAP_LOCK_TIMEOUT_SECONDS
)


@dataclass(frozen=True, slots=True, weakref_slot=True)
class TaskBoardObservation:
    """One stable task-backend revision, the store it was read from, and rows."""

    backend_identity: str
    revision: str
    rows: tuple[Mapping[str, Any], ...]
    error: str | None = None
    # Which store these rows were read out of, beside the revision that says
    # when. Defaulting to the empty witness is what an observation assembled
    # without one deserves: it matches no store that exists, so it is replaced
    # rather than reused.
    store_identity: str = ""
    _projection_lock: threading.Lock = field(
        default_factory=threading.Lock,
        compare=False,
        repr=False,
    )
    _projections: dict[str, object] = field(
        default_factory=dict,
        compare=False,
        repr=False,
    )


@dataclass(frozen=True, slots=True)
class OpenTaskBoardProjection:
    """Task indexes and the open-task payload over one observation."""

    backend_identity: str
    revision: str
    task_filter_inventory: dict[str, Any]
    _active_claims: Mapping[str, Mapping[str, Any]]
    _task_cards_by_origin: Mapping[str, tuple[Mapping[str, Any], ...]]
    _completed_reviews_by_author: Mapping[str, tuple[Mapping[str, Any], ...]]
    _open_followups_by_reviewed: Mapping[str, int]
    _drained_counts_by_actor: Mapping[str, int]
    _row_positions: Mapping[int, int]

    def active_claim(self, actor: str) -> Mapping[str, Any] | None:
        """Return the canonical actor's latest active claim, if any."""
        canonical_actor = tw.canonical_actor(actor)
        return self._active_claims.get(canonical_actor) if canonical_actor else None

    def task_card_rows(self, actor: str) -> tuple[Mapping[str, Any], ...]:
        """Return rows whose origin_thread exactly matches the canonical actor."""
        canonical_actor = tw.canonical_actor(actor)
        if not canonical_actor:
            return ()
        return self._task_cards_by_origin.get(canonical_actor, ())

    def completed_review_rows(
        self, actors: Iterable[str]
    ) -> tuple[Mapping[str, Any], ...]:
        """Return completed rows indexed by their exact review_author."""
        keys = frozenset(str(value) for value in actors if value)
        if not keys:
            return ()
        rows = [
            row
            for actor in keys
            for row in self._completed_reviews_by_author.get(actor, ())
        ]
        rows.sort(
            key=lambda row: (
                _review_pressure_sort_key(row),
                -self._row_positions[id(row)],
            ),
            reverse=True,
        )
        return tuple(rows)

    def open_review_followup_count(self, reviewed_uuid: str) -> int:
        """Return the number of open rows depending on one reviewed UUID."""
        return self._open_followups_by_reviewed.get(reviewed_uuid, 0)

    def drained_task_count(self, actor: str) -> int:
        """Return completed rows associated with the canonical actor."""
        canonical_actor = tw.canonical_actor(actor)
        if not canonical_actor:
            return 0
        return self._drained_counts_by_actor.get(canonical_actor, 0)


_task_board_condition = threading.Condition()
_task_board_observations: dict[str, TaskBoardObservation] = {}
_task_board_builds: set[str] = set()


def _backend_identity(root: Path) -> str:
    return str(root.expanduser().resolve())


def _store_identity(root: Path) -> str:
    """Witness which TaskChampion store a backend root is holding right now.

    The revision that dates the board is carried by the wake file, which is a
    different file from the store the rows are read out of. A store deleted,
    remade, or atomically renamed into place under an untouched wake file
    therefore leaves that revision saying nothing happened, and rows exported
    from the store that is gone would keep being served as current.

    Device and inode name which file the store is, so a replacement renamed
    into place is a different store the moment it lands. Deliberately not size
    or modification time: an export writes the store it reads, so those move
    whenever the board is built and would make every store look replaced by the
    very read that observed it. Identity has to say which store, not which
    version of it -- the revision already says that.

    Creation time joins them wherever the platform records it, so that a
    filesystem free to reuse a freed inode number cannot hand a remade store
    the identity of the one it replaced.

    A store that is absent or cannot be stat'd witnesses nothing, and that is
    itself an identity: it equals no store that exists, so an observation taken
    across that boundary is replaced instead of reused.
    """
    try:
        stat = (task_config.data_dir(root) / OPERATIONS_DB_FILENAME).stat()
    except OSError:
        return ""
    return f"{stat.st_dev}:{stat.st_ino}:{getattr(stat, 'st_birthtime', '')}"


def _store_held_still(before: str, after: str) -> bool:
    """Say whether the rows just read can be trusted to one store.

    A store that witnessed the same identity on both sides of the export never
    moved. A root that had no store beforehand is the other way this holds: the
    export is what creates the store on first use, and rows read out of a store
    that has just come into being cannot be stale against a store that was not
    there. Anything else is a store swapped under the read, and the rows belong
    to no single authority.
    """
    return after == before or not before


def _normalize_task_row(row: Mapping[str, Any]) -> Mapping[str, Any]:
    return MappingProxyType(dict(row))


def _read_task_board(root: Path) -> list[dict[str, Any]]:
    taskrc = task_config.materialize_task_backend(root)
    return tw.export(["status.any:"], taskrc=taskrc)


def _empty_task_filter_counts() -> dict[str, int]:
    return {field: 0 for field in TASK_FILTER_STATE_COUNT_FIELDS}


def _task_row_datetime(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.strptime(value, tw.TW_DATETIME_FORMAT).replace(tzinfo=UTC)
    except ValueError:
        return None


def _task_row_dependencies(row: Mapping[str, Any]) -> set[str]:
    raw = row.get("depends")
    if isinstance(raw, list | tuple):
        return {str(dependency) for dependency in raw if dependency}
    if isinstance(raw, str):
        return {dependency.strip() for dependency in raw.split(",") if dependency}
    return set()


def _is_open_task_row(row: Mapping[str, Any]) -> bool:
    # Taskwarrior's status.any export supplies the raw status. Test and adapter
    # rows that omit it retain the historical open-row default.
    return str(row.get("status") or "pending") in {"pending", "waiting"}


def _open_task_states(
    rows: tuple[Mapping[str, Any], ...],
) -> tuple[
    tuple[Mapping[str, Any], ...],
    set[str],
    set[str],
    set[str],
]:
    open_rows = tuple(row for row in rows if _is_open_task_row(row))
    now = datetime.now(UTC)
    open_uuids = {str(row.get("uuid") or "") for row in open_rows if row.get("uuid")}
    ready: set[str] = set()
    waiting: set[str] = set()
    blocked: set[str] = set()
    for row in open_rows:
        uuid = str(row.get("uuid") or "")
        if not uuid or row.get("claim_by") or row.get("start"):
            continue
        wait_at = _task_row_datetime(row.get("wait"))
        if wait_at is not None and wait_at > now:
            waiting.add(uuid)
            continue
        if _task_row_dependencies(row) & open_uuids:
            blocked.add(uuid)
            continue
        scheduled_at = _task_row_datetime(row.get("scheduled"))
        if scheduled_at is None or scheduled_at <= now:
            ready.add(uuid)
    return open_rows, ready, waiting, blocked


def _hidden_project_stem(project: str, hidden_stems: set[str]) -> str:
    if not project.startswith(task_config.HIDDEN_PROJECT_PREFIX):
        return ""
    try:
        stem = task_config.project_stem(project)
    except SpiceError:
        return ""
    return stem if stem in hidden_stems else ""


def _task_filter_row_state(
    row: Mapping[str, Any],
    *,
    uuid: str,
    ready_uuids: set[str],
    waiting_uuids: set[str],
    blocked_uuids: set[str],
) -> str:
    if str(row.get("claim_by") or ""):
        return "inFlightTaskCount"
    if uuid in waiting_uuids:
        return "deferredTaskCount"
    if uuid in blocked_uuids:
        return "blockedTaskCount"
    if uuid in ready_uuids:
        return "readyTaskCount"
    return "deferredTaskCount"


def _task_filter_project_counts(
    rows: tuple[Mapping[str, Any], ...],
    ready_uuids: set[str],
    waiting_uuids: set[str],
    blocked_uuids: set[str],
    *,
    hidden_stems: set[str],
) -> tuple[dict[str, dict[str, int]], int, dict[str, int]]:
    counts: dict[str, dict[str, int]] = {}
    waiting_count = 0
    hidden_counts: dict[str, int] = {}
    for row in rows:
        project = str(row.get("project") or "")
        if stem := _hidden_project_stem(project, hidden_stems):
            hidden_counts[stem] = hidden_counts.get(stem, 0) + 1
            continue
        uuid = str(row.get("uuid") or "")
        if uuid in waiting_uuids:
            waiting_count += 1
        if not project:
            continue
        project_counts = counts.setdefault(project, _empty_task_filter_counts())
        project_counts["openTaskCount"] += 1
        state = _task_filter_row_state(
            row,
            uuid=uuid,
            ready_uuids=ready_uuids,
            waiting_uuids=waiting_uuids,
            blocked_uuids=blocked_uuids,
        )
        project_counts[state] += 1
    return counts, waiting_count, hidden_counts


def _task_filter_system_stem(
    name: str, count: int, count_field: str | None = None
) -> dict[str, Any]:
    counts = _empty_task_filter_counts()
    counts["openTaskCount"] = count
    counts["deferredTaskCount"] = count
    stem: dict[str, Any] = {"name": name, **counts, "filters": []}
    if count_field is not None:
        stem[count_field] = count
    return stem


def _task_filter_payload_rows(
    counts: dict[str, dict[str, int]],
    waiting_count: int,
    hidden_counts: dict[str, int],
    *,
    assignable_stems: set[str],
    visible_stems: set[str],
) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]:
    filters: list[dict[str, Any]] = []
    stems: dict[str, dict[str, Any]] = {}
    for project, project_counts in sorted(counts.items()):
        stem = project.split(".", 1)[0]
        if stem not in visible_stems:
            continue
        entry = stems.setdefault(
            stem, {"name": stem, **_empty_task_filter_counts(), "filters": []}
        )
        for state_field, value in project_counts.items():
            entry[state_field] += value
        if stem in assignable_stems:
            filters.append({"name": project, "primaryStem": stem, **project_counts})
            entry["filters"].append(project)
    if waiting_count:
        stems["waiting"] = _task_filter_system_stem(
            "waiting", waiting_count, "waitingTaskCount"
        )
    oops_stem = task_config.project_stem(task_config.OOPS_PROJECT)
    for stem_name, count in sorted(hidden_counts.items()):
        count_field = "oopsTaskCount" if stem_name == oops_stem else None
        stems[stem_name] = _task_filter_system_stem(stem_name, count, count_field)
    return filters, stems


def _task_filter_inventory(
    observation: TaskBoardObservation,
    rows: tuple[Mapping[str, Any], ...],
    ready_uuids: set[str],
    waiting_uuids: set[str],
    blocked_uuids: set[str],
    catalog: dict[str, object],
) -> dict[str, Any]:
    assignable_stems = set(cast(list[str], catalog["approvedStems"]))
    hidden_stems = set(cast(list[str], catalog["hiddenStems"]))
    visible_stems = assignable_stems | set(task_config.INTERNAL_STEMS)
    counts, waiting_count, hidden_counts = _task_filter_project_counts(
        rows,
        ready_uuids,
        waiting_uuids,
        blocked_uuids,
        hidden_stems=hidden_stems,
    )
    filters, stems = _task_filter_payload_rows(
        counts,
        waiting_count,
        hidden_counts,
        assignable_stems=assignable_stems,
        visible_stems=visible_stems,
    )
    return {
        "revision": observation.revision,
        "filters": filters,
        "primaryStems": list(stems.values()),
        "openTaskCount": sum(item["openTaskCount"] for item in filters),
        "catalog": {
            "approvedStems": catalog["approvedStems"],
            "hiddenStems": catalog["hiddenStems"],
            "approvedPhases": catalog["approvedPhases"],
            "defaultFlow": catalog["defaultFlow"],
            "perStemFlows": catalog["perStemFlows"],
            "hiddenProjectPrefix": catalog["hiddenProjectPrefix"],
            "filterDelimiter": catalog["projectDelimiter"],
            "segmentPattern": catalog["segmentPattern"],
            "segmentRuleLabel": catalog["segmentRuleLabel"],
            "filterExamples": catalog["projectExamples"],
        },
    }


def _active_claim_index(
    rows: tuple[Mapping[str, Any], ...],
) -> Mapping[str, Mapping[str, Any]]:
    latest: dict[str, Mapping[str, Any]] = {}
    for row in rows:
        if not row.get("start"):
            continue
        actor = tw.canonical_actor(str(row.get("claim_by") or ""))
        if not actor:
            continue
        current = latest.get(actor)
        if current is None or str(row.get("claim_at") or "") > str(
            current.get("claim_at") or ""
        ):
            latest[actor] = row
    return MappingProxyType(latest)


def _row_tuple_index(
    rows: Iterable[Mapping[str, Any]],
    *,
    field_name: str,
) -> Mapping[str, tuple[Mapping[str, Any], ...]]:
    indexed: dict[str, list[Mapping[str, Any]]] = {}
    for row in rows:
        key = str(row.get(field_name) or "")
        if key:
            indexed.setdefault(key, []).append(row)
    return MappingProxyType({key: tuple(values) for key, values in indexed.items()})


def _review_pressure_sort_key(row: Mapping[str, Any]) -> str:
    return str(
        row.get("review_at")
        or row.get("end")
        or row.get("modified")
        or row.get("entry")
        or ""
    )


def _completed_review_index(
    rows: tuple[Mapping[str, Any], ...],
) -> Mapping[str, tuple[Mapping[str, Any], ...]]:
    completed = (row for row in rows if str(row.get("status") or "") == "completed")
    return _row_tuple_index(completed, field_name="review_author")


def _open_review_followup_index(
    rows: tuple[Mapping[str, Any], ...],
) -> Mapping[str, int]:
    counts: dict[str, int] = {}
    for row in rows:
        if not _is_open_task_row(row):
            continue
        for reviewed_uuid in _task_row_dependencies(row):
            counts[reviewed_uuid] = counts.get(reviewed_uuid, 0) + 1
    return MappingProxyType(counts)


def _drained_task_count_index(
    rows: tuple[Mapping[str, Any], ...],
) -> Mapping[str, int]:
    counts: dict[str, int] = {}
    for row in rows:
        if str(row.get("status") or "") != "completed":
            continue
        actors = {
            str(row.get(field_name) or "")
            for field_name in TASK_ACTOR_FIELDS
            if row.get(field_name)
        }
        for actor in actors:
            counts[actor] = counts.get(actor, 0) + 1
    return MappingProxyType(counts)


def _build_open_task_board_projection(
    observation: TaskBoardObservation,
) -> OpenTaskBoardProjection:
    catalog = task_config.task_project_validation_catalog()
    open_rows, ready, waiting, blocked = _open_task_states(observation.rows)
    return OpenTaskBoardProjection(
        backend_identity=observation.backend_identity,
        revision=observation.revision,
        task_filter_inventory=_task_filter_inventory(
            observation,
            open_rows,
            ready,
            waiting,
            blocked,
            catalog,
        ),
        _active_claims=_active_claim_index(open_rows),
        _task_cards_by_origin=_row_tuple_index(
            observation.rows,
            field_name="origin_thread",
        ),
        _completed_reviews_by_author=_completed_review_index(observation.rows),
        _open_followups_by_reviewed=_open_review_followup_index(observation.rows),
        _drained_counts_by_actor=_drained_task_count_index(observation.rows),
        _row_positions=MappingProxyType(
            {id(row): position for position, row in enumerate(observation.rows)}
        ),
    )


def _release_task_board_build(backend_identity: str) -> None:
    with _task_board_condition:
        _task_board_builds.discard(backend_identity)
        _task_board_condition.notify_all()


def current_task_board_observation(
    *, backend_root: Path | None = None
) -> TaskBoardObservation:
    """Return the current coherent board, coalescing concurrent cache misses.

    Coherent means both that the revision held still across the export and that
    the rows came out of the store still in place afterwards. The revision alone
    cannot say the second, because it is carried by the wake file rather than by
    the store, so a store replaced under an untouched wake file would leave the
    old rows looking current for as long as nothing else moved.

    Re-reading the identity keeps the export off the hot path all the same: an
    unchanged authority still matches on both and returns the cached rows
    without an export, and only a genuine change costs one.

    A backend failure degrades only the current call. Its empty observation is
    deliberately not cached, so a peer or later request can recover without a
    task revision change.
    """

    selected_root = (backend_root or task_config.backend_root()).expanduser().resolve()
    backend_identity = _backend_identity(selected_root)
    wait_deadline = time.monotonic() + TASK_BOARD_OBSERVATION_TIMEOUT_SECONDS

    while True:
        revision = task_config.task_event_revision(selected_root)
        store_identity = _store_identity(selected_root)
        with _task_board_condition:
            cached = _task_board_observations.get(backend_identity)
            if (
                cached is not None
                and cached.revision == revision
                and cached.store_identity == store_identity
            ):
                return cached
            if backend_identity in _task_board_builds:
                remaining = wait_deadline - time.monotonic()
                if remaining <= 0:
                    return TaskBoardObservation(
                        backend_identity=backend_identity,
                        revision=revision,
                        rows=(),
                        error="timed out waiting for the current task board",
                        store_identity=store_identity,
                    )
                _task_board_condition.wait(timeout=remaining)
                continue
            _task_board_builds.add(backend_identity)
            break

    try:
        while True:
            revision = task_config.task_event_revision(selected_root)
            store_identity = _store_identity(selected_root)
            rows = _read_task_board(selected_root)
            normalized = tuple(_normalize_task_row(row) for row in rows)
            observed_revision = task_config.task_event_revision(selected_root)
            observed_store_identity = _store_identity(selected_root)
            if observed_revision == revision and _store_held_still(
                store_identity, observed_store_identity
            ):
                break
            if time.monotonic() >= wait_deadline:
                _release_task_board_build(backend_identity)
                return TaskBoardObservation(
                    backend_identity=backend_identity,
                    revision=observed_revision,
                    rows=(),
                    error="timed out building the current task board",
                    store_identity=observed_store_identity,
                )
    except SpiceError as exc:
        _release_task_board_build(backend_identity)
        return TaskBoardObservation(
            backend_identity=backend_identity,
            revision=revision,
            rows=(),
            error=str(exc),
            store_identity=_store_identity(selected_root),
        )
    except BaseException:
        _release_task_board_build(backend_identity)
        raise

    # The store the rows are known to have come out of, which is the one seen
    # after the export rather than before it: on first use no store existed
    # until that export created it.
    settled_store_identity = observed_store_identity
    candidate = TaskBoardObservation(
        backend_identity=backend_identity,
        revision=revision,
        rows=normalized,
        store_identity=settled_store_identity,
    )
    with _task_board_condition:
        current = _task_board_observations.get(backend_identity)
        if (
            current is None
            or current.revision != revision
            or current.store_identity != settled_store_identity
        ):
            _task_board_observations[backend_identity] = candidate
            current = candidate
        _task_board_builds.discard(backend_identity)
        _task_board_condition.notify_all()
        return current


def open_task_board_projection(
    observation: TaskBoardObservation | None = None,
    *,
    backend_root: Path | None = None,
) -> OpenTaskBoardProjection:
    """Return the memoized open-board projection for one observation."""
    if observation is not None and backend_root is not None:
        raise ValueError("observation and backend_root are mutually exclusive")
    selected = observation or current_task_board_observation(backend_root=backend_root)
    with selected._projection_lock:
        cached = selected._projections.get("open")
        if isinstance(cached, OpenTaskBoardProjection):
            return cached
        projection = _build_open_task_board_projection(selected)
        selected._projections["open"] = projection
        return projection
