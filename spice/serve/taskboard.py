"""Revision-coherent observation of the current task board."""

from __future__ import annotations

import os
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
    # Which store these rows were read out of and which version of it, beside
    # the revision that says when. Defaulting to the empty witness is what an
    # observation assembled without one deserves: it matches no store that
    # exists, so it is replaced rather than reused.
    store_generation: str = ""
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
    """Task indexes and the open-task payload over one observation.

    Every index answers empty over a failed observation, because a caller
    reading one of them is asking what this pass saw and the answer is nothing.
    The inventory is the exception: it is published to a browser that orders it
    by the task revision it carries, and a board nobody could read carries the
    live one. Stamped that way an empty inventory reads as the newest truth
    about the board, and the recovered inventory that follows it at the same
    revision is refused as a redelivery. So a failed observation has no
    inventory at all, and a publisher with nothing to publish names no facet.
    """

    backend_identity: str
    revision: str
    task_filter_inventory: dict[str, Any] | None
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


def _store_stat(root: Path) -> os.stat_result | None:
    """Stat the TaskChampion store once, for every witness taken of it.

    Both witnesses below are read off one stat rather than one each, so the two
    can never describe stores from different instants: a comparison built from
    two stats could straddle a replacement and call it coherent.

    An absent or unstattable store answers `None`, which every witness renders
    as the empty string. That is itself an answer: it equals no store that
    exists, so an observation taken across that boundary is replaced instead of
    reused.
    """
    try:
        return (task_config.data_dir(root) / OPERATIONS_DB_FILENAME).stat()
    except OSError:
        return None


def _store_identity(stat: os.stat_result | None) -> str:
    """Witness which file the store is, unmoved by writing to it.

    Device and inode name the file, so a replacement renamed into place is a
    different store the moment it lands. Deliberately not size, modification
    time, or change time: an export writes the store it reads, so all of those
    move whenever the board is built and would make every store look replaced
    by the very read that observed it.

    This is the witness the across-the-export check compares, where being
    unmoved by writing is the whole requirement. It cannot tell a remade store
    from the one whose inode number it inherited; `_store_generation` is what
    answers that, on the comparison that is free to move under a write.
    """
    if stat is None:
        return ""
    return f"{stat.st_dev}:{stat.st_ino}"


def _store_generation(stat: os.stat_result | None) -> str:
    """Witness which store, and which version of it, a cached board came from.

    The revision that dates the board is carried by the wake file, which is a
    different file from the store the rows are read out of. A store deleted,
    remade, or atomically renamed into place under an untouched wake file
    therefore leaves that revision saying nothing happened, and rows exported
    from the store that is gone would keep being served as current.

    Change time joins device and inode here because a filesystem is free to
    hand a remade store the inode number the deleted one just freed, and on
    that filesystem device and inode alone would call the replacement the
    original. A store that is created after another is unlinked carries the
    later change time, so the two witness different generations. What this
    rests on is the platform separating those two events at all: a filesystem
    whose timestamps are too coarse to tell them apart can still collide, and
    no protection stronger than that is claimed.

    Being moved by an ordinary write is correct here and wrong for
    `_store_identity`. This witness is compared between one build and the next,
    where a store written since the last board is a store worth re-reading;
    settling it from the same stat that ended the build is what keeps the
    read after a rebuild measuring what the rebuild settled on, and hitting.
    """
    if stat is None:
        return ""
    return f"{stat.st_dev}:{stat.st_ino}:{stat.st_ctime_ns}"


def _store_held_still(before: str, after: str) -> bool:
    """Say whether the rows just read can be trusted to one store.

    A store that witnessed the same identity on both sides of the export never
    moved. A root that had no store beforehand is the other way this holds: the
    export is what creates the store on first use, and rows read out of a store
    that has just come into being cannot be stale against a store that was not
    there. Anything else is a store swapped under the read, and the rows belong
    to no single authority.

    Which file is the only question this can ask, so the one swap it cannot see
    is a store unlinked and remade onto the same inode number inside a single
    export. Every other property of a store moves when the export writes it, and
    a witness that moved would call every build a replacement. That residual is
    left visible here rather than closed on the platforms that happen to record
    a creation time, because a witness meaning different things on different
    platforms is what let a recreated store pass as the original to begin with.
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
    open_rows, ready, waiting, blocked = _open_task_states(observation.rows)
    return OpenTaskBoardProjection(
        backend_identity=observation.backend_identity,
        revision=observation.revision,
        task_filter_inventory=(
            None
            if observation.error
            else _task_filter_inventory(
                observation,
                open_rows,
                ready,
                waiting,
                blocked,
                task_config.task_project_validation_catalog(),
            )
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


def _reusable_task_board_observation(
    backend_identity: str, selected_root: Path, wait_deadline: float
) -> TaskBoardObservation | None:
    """Answer a board this caller may reuse, or `None` once it owns the build.

    Two questions decide reuse, because two files answer for a board. The
    revision says when the authority last moved, and `_store_generation` says
    which store and which version of it the cached rows came out of. A store
    rewritten, replaced, or remade onto a recycled inode number under an
    untouched wake file moves the second and not the first, so asking both is
    what keeps a board that is gone from reading as current.

    Both answers are read before the lock and re-read on every pass, so a
    caller that waited behind someone else's build sees whatever that build
    published rather than the state it queued against. A peer already building
    is waited on to its deadline; the empty observation past that deadline is
    this call giving up, not an answer about the board.
    """
    while True:
        revision = task_config.task_event_revision(selected_root)
        store_generation = _store_generation(_store_stat(selected_root))
        with _task_board_condition:
            cached = _task_board_observations.get(backend_identity)
            if (
                cached is not None
                and cached.revision == revision
                and cached.store_generation == store_generation
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
                        store_generation=store_generation,
                    )
                _task_board_condition.wait(timeout=remaining)
                continue
            _task_board_builds.add(backend_identity)
            return None


def _exported_task_board_observation(
    backend_identity: str, selected_root: Path, wait_deadline: float
) -> TaskBoardObservation:
    """Export until one authority answers for the rows, and settle on it.

    Coherent means both that the revision held still across the export and that
    the rows came out of the store still in place afterwards. Neither check
    subsumes the other: the revision is carried by the wake file rather than by
    the store, and the store is written by the very export being checked.

    So the two comparisons take different witnesses. `_store_identity` is asked
    here, across the export, because it answers which file and nothing that
    writing moves. `_store_generation` is what the observation settles on,
    because the next caller is asking which version it is holding.

    Rows read across a swap belong to no single authority and are discarded for
    another pass. Past the deadline the caller is answered with the emptiness it
    actually got; that observation reports the revision and generation seen last
    so a reader can tell how far the churn had run.
    """
    revision = ""
    try:
        while True:
            revision = task_config.task_event_revision(selected_root)
            store_identity = _store_identity(_store_stat(selected_root))
            rows = _read_task_board(selected_root)
            normalized = tuple(_normalize_task_row(row) for row in rows)
            observed_revision = task_config.task_event_revision(selected_root)
            # One stat answers both questions asked of the store after the
            # export: which file it is, for the held-still check, and which
            # version of it, for the generation this observation settles on.
            # Measuring them apart would let a replacement land between them.
            observed_stat = _store_stat(selected_root)
            if observed_revision == revision and _store_held_still(
                store_identity, _store_identity(observed_stat)
            ):
                return TaskBoardObservation(
                    backend_identity=backend_identity,
                    revision=revision,
                    rows=normalized,
                    store_generation=_store_generation(observed_stat),
                )
            if time.monotonic() >= wait_deadline:
                return TaskBoardObservation(
                    backend_identity=backend_identity,
                    revision=observed_revision,
                    rows=(),
                    error="timed out building the current task board",
                    store_generation=_store_generation(observed_stat),
                )
    except SpiceError as exc:
        return TaskBoardObservation(
            backend_identity=backend_identity,
            revision=revision,
            rows=(),
            error=str(exc),
            store_generation=_store_generation(_store_stat(selected_root)),
        )


def _publish_task_board_observation(
    candidate: TaskBoardObservation,
) -> TaskBoardObservation:
    """Cache the built board, keep any equal one already there, and wake waiters.

    Only the holder of the build slot reaches here, so nothing newer can be in
    the cache to protect. What the comparison protects is object identity: a
    cached board answering for the same revision and the same store generation
    is the same board, and readers that coalesced onto this build are handed
    that one object rather than an equal copy per caller. Anything else is a
    board this build has superseded, and it is replaced.
    """
    backend_identity = candidate.backend_identity
    with _task_board_condition:
        current = _task_board_observations.get(backend_identity)
        if (
            current is None
            or current.revision != candidate.revision
            or current.store_generation != candidate.store_generation
        ):
            _task_board_observations[backend_identity] = candidate
            current = candidate
        _task_board_builds.discard(backend_identity)
        _task_board_condition.notify_all()
        return current


def current_task_board_observation(
    *, backend_root: Path | None = None
) -> TaskBoardObservation:
    """Return the current coherent board, coalescing concurrent cache misses.

    Re-reading the revision and the store generation keeps the export off the
    hot path: an unchanged authority matches on both and returns the cached
    rows without an export, and only a genuine change costs one.

    A backend failure degrades only the current call. Its empty observation is
    deliberately not cached, so a peer or later request can recover without a
    task revision change. The build slot is released on every way out, including
    an exception this module never converted into an observation.
    """

    selected_root = (backend_root or task_config.backend_root()).expanduser().resolve()
    backend_identity = _backend_identity(selected_root)
    wait_deadline = time.monotonic() + TASK_BOARD_OBSERVATION_TIMEOUT_SECONDS

    reusable = _reusable_task_board_observation(
        backend_identity, selected_root, wait_deadline
    )
    if reusable is not None:
        return reusable
    try:
        candidate = _exported_task_board_observation(
            backend_identity, selected_root, wait_deadline
        )
    except BaseException:
        _release_task_board_build(backend_identity)
        raise
    if candidate.error:
        _release_task_board_build(backend_identity)
        return candidate
    return _publish_task_board_observation(candidate)


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
