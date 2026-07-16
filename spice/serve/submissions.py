"""Per-connection submission lifecycle correlation for the serve live bus."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from threading import Lock
from typing import Any, Callable

from spice.agent.lifecycle import utc_now
from spice.errors import SpiceError
from spice.mail.ackstate import ack_state_records
from spice.mail.inbox import inbox_item_key_aliases

SUBMISSION_STAGES = ("accepted", "received", "completed")
MAX_TRACKED_SUBMISSIONS = 200
_FINAL_MESSAGE_KINDS = frozenset({"final", "reply"})
_MILLISECONDS_PER_SECOND = 1000


@dataclass(frozen=True)
class SubmissionStage:
    at: str
    observed_epoch: float
    source: str
    evidence: str
    source_at: str = ""
    source_epoch: float | None = None

    def to_payload(self) -> dict[str, str]:
        payload = {
            "at": self.at,
            "source": self.source,
            "evidence": self.evidence,
        }
        if self.source_at:
            payload["sourceAt"] = self.source_at
        return payload


@dataclass
class SubmissionLifecycle:
    target_id: str
    key: str
    disposition: str = ""
    stages: dict[str, SubmissionStage] = field(default_factory=dict)

    def event_payload(self, stage: str) -> dict[str, Any]:
        return {
            "key": self.key,
            "stage": stage,
            "disposition": self.disposition,
            "stages": {
                name: self.stages[name].to_payload()
                for name in SUBMISSION_STAGES
                if name in self.stages
            },
            "durationsMs": self._durations_payload(),
        }

    def _durations_payload(self) -> dict[str, float]:
        accepted = self.stages["accepted"].observed_epoch
        durations: dict[str, float] = {}
        received = self.stages.get("received")
        if received is not None:
            durations["acceptedToReceived"] = _elapsed_ms(
                accepted, received.observed_epoch
            )
        completed = self.stages.get("completed")
        if completed is not None:
            durations["acceptedToCompleted"] = _elapsed_ms(
                accepted, completed.observed_epoch
            )
            if received is not None:
                durations["receivedToCompleted"] = _elapsed_ms(
                    received.observed_epoch, completed.observed_epoch
                )
        return durations


class SubmissionLifecycleTracker:
    """Join send acceptance to ACK/NACK and final message watcher events."""

    def __init__(self, *, now: Callable[[], str] = utc_now) -> None:
        self._now = now
        self._items: dict[tuple[str, str], SubmissionLifecycle] = {}
        self._lock = Lock()

    def accept(
        self,
        *,
        target_id: str,
        key: str,
        evidence: str,
    ) -> dict[str, Any]:
        with self._lock:
            item_key = (target_id, key)
            existing = self._items.get(item_key)
            if existing is not None:
                return existing.event_payload("accepted")
            observed_at = self._now()
            lifecycle = SubmissionLifecycle(target_id=target_id, key=key)
            lifecycle.stages["accepted"] = SubmissionStage(
                at=observed_at,
                observed_epoch=_timestamp_epoch(observed_at),
                source="inbox-write",
                evidence=evidence or key,
            )
            self._items[item_key] = lifecycle
            self._enforce_total_limit()
            return lifecycle.event_payload("accepted")

    def advance(
        self,
        *,
        target_id: str,
        repo_root: str | Path | None,
        payload: dict[str, Any],
        include_ack_state: bool = False,
    ) -> list[dict[str, Any]]:
        with self._lock:
            if not self._active_for_target(target_id):
                return []
        messages = [
            message
            for message in payload.get("messages", [])
            if isinstance(message, dict)
        ]
        ack_records = _ack_records_by_key(repo_root) if include_ack_state else {}
        with self._lock:
            active = self._active_for_target(target_id)
            events: list[dict[str, Any]] = []
            for lifecycle in active:
                matching = [
                    message
                    for message in messages
                    if _message_matches_submission(message, lifecycle)
                ]
                if "received" not in lifecycle.stages:
                    receipt = matching[0] if matching else None
                    if receipt is not None:
                        self._record_received_from_message(lifecycle, receipt)
                        events.append(lifecycle.event_payload("received"))
                    else:
                        ack_record = ack_records.get(lifecycle.key)
                        if ack_record is not None:
                            self._record_received_from_ack_state(lifecycle, ack_record)
                            events.append(lifecycle.event_payload("received"))
                if "received" not in lifecycle.stages:
                    continue
                completion = _completion_message(lifecycle, messages, matching)
                if completion is None:
                    continue
                self._record_completed(lifecycle, completion)
                events.append(lifecycle.event_payload("completed"))
            return events

    def _active_for_target(self, target_id: str) -> list[SubmissionLifecycle]:
        return [
            item
            for (item_target_id, _key), item in self._items.items()
            if item_target_id == target_id and "completed" not in item.stages
        ]

    def _enforce_total_limit(self) -> None:
        """Keep newest rows across every state; dict order is acceptance order."""
        overflow = len(self._items) - MAX_TRACKED_SUBMISSIONS
        for item_key in list(self._items)[: max(0, overflow)]:
            del self._items[item_key]

    def _record_received_from_message(
        self, lifecycle: SubmissionLifecycle, message: dict[str, Any]
    ) -> None:
        source_at = str(message.get("timestamp") or "")
        observed_at = self._now()
        lifecycle.disposition = (
            "refused"
            if _key_in_values(lifecycle.key, message.get("nack_keys"))
            else "acked"
        )
        lifecycle.stages["received"] = SubmissionStage(
            at=observed_at,
            observed_epoch=_timestamp_epoch(observed_at),
            source=_message_source(message),
            evidence=str(message.get("key") or lifecycle.key),
            source_at=source_at,
            source_epoch=_optional_timestamp_epoch(source_at),
        )

    def _record_received_from_ack_state(
        self, lifecycle: SubmissionLifecycle, record: Any
    ) -> None:
        observed_at = self._now()
        source_at = _iso_timestamp(float(record.archived_at))
        lifecycle.disposition = str(record.disposition or "acked")
        lifecycle.stages["received"] = SubmissionStage(
            at=observed_at,
            observed_epoch=_timestamp_epoch(observed_at),
            source="ack-state",
            evidence=lifecycle.disposition,
            source_at=source_at,
            source_epoch=float(record.archived_at),
        )

    def _record_completed(
        self, lifecycle: SubmissionLifecycle, message: dict[str, Any]
    ) -> None:
        source_at = str(message.get("timestamp") or "")
        observed_at = self._now()
        lifecycle.stages["completed"] = SubmissionStage(
            at=observed_at,
            observed_epoch=_timestamp_epoch(observed_at),
            source=_message_source(message),
            evidence=str(message.get("key") or lifecycle.key),
            source_at=source_at,
            source_epoch=_optional_timestamp_epoch(source_at),
        )


def _completion_message(
    lifecycle: SubmissionLifecycle,
    messages: list[dict[str, Any]],
    matching: list[dict[str, Any]],
) -> dict[str, Any] | None:
    for message in matching:
        if str(message.get("kind") or "") in _FINAL_MESSAGE_KINDS:
            return message
    received = lifecycle.stages["received"]
    lower_bound = received.source_epoch
    if lower_bound is None:
        lower_bound = lifecycle.stages["accepted"].observed_epoch
    for message in messages:
        if str(message.get("kind") or "") != "final":
            continue
        source_epoch = _optional_timestamp_epoch(str(message.get("timestamp") or ""))
        if source_epoch is None or source_epoch >= lower_bound:
            return message
    return None


def _message_matches_submission(
    message: dict[str, Any], lifecycle: SubmissionLifecycle
) -> bool:
    accepted_epoch = lifecycle.stages["accepted"].observed_epoch
    source_epoch = _optional_timestamp_epoch(str(message.get("timestamp") or ""))
    if source_epoch is not None and source_epoch < accepted_epoch:
        return False
    return _key_in_values(lifecycle.key, message.get("ack_keys"))


def _key_in_values(key: str, values: Any) -> bool:
    if not isinstance(values, list):
        return False
    wanted = inbox_item_key_aliases(key)
    return any(inbox_item_key_aliases(str(value)) & wanted for value in values if value)


def _message_source(message: dict[str, Any]) -> str:
    if str(message.get("kind") or "") == "reply":
        return "reply-log"
    return "transcript"


def _ack_records_by_key(repo_root: str | Path | None) -> dict[str, Any]:
    if repo_root is None:
        return {}
    try:
        records = ack_state_records(repo_root)
    except (OSError, RuntimeError, SpiceError):
        return {}
    result: dict[str, Any] = {}
    for record in records:
        for alias in inbox_item_key_aliases(record.key):
            result[alias] = record
    return result


def _timestamp_epoch(value: str) -> float:
    parsed = _optional_timestamp_epoch(value)
    if parsed is None:
        return datetime.now(UTC).timestamp()
    return parsed


def _optional_timestamp_epoch(value: str) -> float | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def _iso_timestamp(epoch: float) -> str:
    return (
        datetime.fromtimestamp(epoch, UTC)
        .isoformat(timespec="microseconds")
        .replace("+00:00", "Z")
    )


def _elapsed_ms(start: float, end: float) -> float:
    return max(0.0, (end - start) * _MILLISECONDS_PER_SECOND)
