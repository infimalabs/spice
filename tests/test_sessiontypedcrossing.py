"""Parity gate for sessions consumers crossed onto typed transcript facts."""

from __future__ import annotations

import dataclasses
import hashlib
import json
from pathlib import Path
from typing import cast

from spice.sessions import analysis, commandrecords, records
from spice.sessions.slices import (
    build_compaction_slices,
    select_compaction_windows_from_files,
)
from spice.transcript.assembly import AssembledMessageReducer
from spice.transcript.events import TranscriptEvent
from spice.transcript.reader import TranscriptEventReader
from tests.test_sessionfixtures import (
    SUPERVISED_FIXTURES,
    transcript_driver_for_fixture,
)
from tests.test_transcriptparity import (
    CorpusCase,
    ParityOutput,
    assert_parity,
    parity_corpus,
    split_pass_events,
    typed_events,
)

SUPERVISED_PROJECTION_SHA256 = (
    "93f94ce87a49d93aef95446541bd73a55a602a5788772982e877ef98c6c59732"
)


def test_typed_crossing_preserves_supervised_fixture_projection_bytes(
    monkeypatch,
) -> None:
    projection: dict[str, dict[str, list[dict]]] = {}
    for transcript in SUPERVISED_FIXTURES:
        with transcript_driver_for_fixture(monkeypatch, transcript):
            messages = analysis.collect_messages([transcript])
            commands = commandrecords.collect_command_records([transcript])
            turns = records.collect_turns([transcript])
            compactions = records.collect_compactions([transcript])
            slices = build_compaction_slices(turns, compactions)
        projection[transcript.name] = {
            "messages": [_portable_record(row) for row in messages],
            "commands": [_portable_record(row) for row in commands],
            "turns": [_portable_record(row) for row in turns],
            "compactions": [_portable_record(row) for row in compactions],
            "slices": [_portable_record(row) for row in slices],
        }

    encoded = json.dumps(
        projection,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode()

    assert hashlib.sha256(encoded).hexdigest() == SUPERVISED_PROJECTION_SHA256


def test_turn_records_match_across_recorded_whole_and_split_replays() -> None:
    recorded = tuple(case for case in parity_corpus() if case.name == "recorded")

    assert_parity(
        _whole_pass_turn_records,
        _split_pass_turn_records,
        corpus=recorded,
        labels=("whole-pass turns", "split-pass turns"),
    )


def test_crossed_consumers_read_and_reduce_the_public_typed_stream(
    monkeypatch,
) -> None:
    transcript = SUPERVISED_FIXTURES[0]
    reads: list[tuple[Path, str]] = []
    reduced: list[TranscriptEvent] = []
    read_typed_events = TranscriptEventReader.read
    reduce_typed_event = AssembledMessageReducer.push

    def track_read(self, mode, **kwargs):
        reads.append((self.path, mode))
        return read_typed_events(self, mode, **kwargs)

    def track_reducer(self, event):
        reduced.append(event)
        return reduce_typed_event(self, event)

    monkeypatch.setattr(TranscriptEventReader, "read", track_read)
    monkeypatch.setattr(AssembledMessageReducer, "push", track_reducer)

    messages = analysis.collect_messages([transcript])
    commands = commandrecords.collect_command_records([transcript])
    turns = records.collect_turns([transcript])
    compactions = records.collect_compactions([transcript])
    selection = select_compaction_windows_from_files([transcript], count=2)

    assert reads == [
        *((transcript, "forward"),) * 4,
        (transcript, "reverse"),
    ]
    assert reduced
    assert messages[0].text.startswith("The linked skill below")
    assert [record.status for record in commands] == ["called", "called"]
    assert turns[0].assistant_commentary
    assert len(compactions) == 3
    assert len(selection.selected_boundaries) == 2


def _whole_pass_turn_records(case: CorpusCase) -> tuple[ParityOutput, ...]:
    return _turn_record_outputs(case, typed_events(case))


def _split_pass_turn_records(case: CorpusCase) -> tuple[ParityOutput, ...]:
    events = cast(
        tuple[TranscriptEvent, ...],
        tuple(output.value for output in split_pass_events(case)),
    )
    return _turn_record_outputs(case, events)


def _turn_record_outputs(
    case: CorpusCase,
    events: tuple[TranscriptEvent, ...],
) -> tuple[ParityOutput, ...]:
    return tuple(
        ParityOutput(value=_portable_record(turn))
        for turn in records.collect_turns_from_events(case.path, events)
    )


def _portable_record(value: object) -> dict:
    record = dataclasses.asdict(value)
    record["source_file"] = Path(record["source_file"]).name
    for counter_field in ("tool_calls", "touched_files"):
        if hasattr(value, counter_field):
            record[counter_field] = dict(getattr(value, counter_field))
    return record
