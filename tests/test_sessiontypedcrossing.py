"""Parity gate for sessions consumers crossed onto typed transcript facts."""

from __future__ import annotations

import dataclasses
import hashlib
import json
from pathlib import Path

from spice.sessions import analysis, commandrecords
from spice.transcript.reader import TranscriptEventReader
from tests.test_sessionfixtures import (
    SUPERVISED_FIXTURES,
    transcript_driver_for_fixture,
)

SUPERVISED_PROJECTION_SHA256 = (
    "a95513be96154822bfe58aa4d88f665619ac44c01cee82677f78719dd5253190"
)


def test_typed_crossing_preserves_supervised_fixture_projection_bytes(
    monkeypatch,
) -> None:
    projection: dict[str, dict[str, list[dict]]] = {}
    for transcript in SUPERVISED_FIXTURES:
        with transcript_driver_for_fixture(monkeypatch, transcript):
            messages = analysis.collect_messages([transcript])
            commands = commandrecords.collect_command_records([transcript])
        projection[transcript.name] = {
            "messages": [_portable_record(row) for row in messages],
            "commands": [_portable_record(row) for row in commands],
        }

    encoded = json.dumps(
        projection,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode()

    assert hashlib.sha256(encoded).hexdigest() == SUPERVISED_PROJECTION_SHA256


def test_crossed_consumers_read_the_public_typed_stream(monkeypatch) -> None:
    transcript = SUPERVISED_FIXTURES[0]
    reads: list[tuple[Path, str]] = []
    read_typed_events = TranscriptEventReader.read

    def track_read(self, mode, **kwargs):
        reads.append((self.path, mode))
        return read_typed_events(self, mode, **kwargs)

    monkeypatch.setattr(TranscriptEventReader, "read", track_read)

    messages = analysis.collect_messages([transcript])
    commands = commandrecords.collect_command_records([transcript])

    assert reads == [(transcript, "forward"), (transcript, "forward")]
    assert messages[0].text.startswith("The linked skill below")
    assert [record.status for record in commands] == ["called", "called"]


def _portable_record(value: object) -> dict:
    record = dataclasses.asdict(value)
    record["source_file"] = Path(record["source_file"]).name
    return record
