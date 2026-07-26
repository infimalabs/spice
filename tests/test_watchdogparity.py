"""Parity between a supervised lane's live judgment and a transcript replay.

The supervisor judges an agent from stdout while it is being printed; every
other reader of that lane -- session views, forensics, this suite -- reads the
transcript those same lines were persisted to. Two access paths, one dialect,
and -- since this crossing landed -- one classifier, so both have to reach the
same conclusions: the same prose handed downstream, the same starvation
streaks, and the same keys retired from the inbox.

The corpus and the comparison come from the shared parity harness rather than
a fork here; what this suite adds is the supervisor's own judgment as an
interpreter, plus the one shape a recorded corpus cannot carry -- a lane that
goes quiet for long enough to starve.
"""

from __future__ import annotations

import json
from collections.abc import Iterator, Sequence
from dataclasses import dataclass
from enum import StrEnum
from itertools import groupby
from pathlib import Path

from spice.agent.driver import CLAUDE_DRIVER
from spice.agent.watchdog import (
    TEXT_STARVATION_THRESHOLD,
    JsonStdoutScanner,
    SupervisedProseFold,
)
from spice.mail.ackgrammar import (
    AckSegment,
    extract_ack_segments_from_text,
    extract_nack_segments_from_text,
)
from spice.transcript.assembly import AssembledMessage, SpanKind
from spice.transcript.events import TranscriptEvent
from tests.test_transcriptparity import (
    CorpusCase,
    ParityOutput,
    assembled_messages,
    assert_parity,
    forward_read,
    parity_corpus,
    typed_events,
)

# The live lane below goes quiet one record past the nudge, so the run proves
# the supervisor nudges once per streak rather than once per silent turn, then
# speaks and goes quiet again for exactly the threshold.
LIVE_SILENT_RUN = TEXT_STARVATION_THRESHOLD + 1
LIVE_SECOND_SILENT_RUN = TEXT_STARVATION_THRESHOLD
LIVE_TIMESTAMP_PREFIX = "2026-07-26T05:00:"
LIVE_OPENING_PROSE = "ACK 1jN5Xq7C: live lane opened"
LIVE_RESUMED_PROSE = "ACK 1jN5Yb2M: back with prose"
LIVE_TRAILING_PROSE = "ACK 1jN5Zc4T: prose behind a tool call"


class JudgmentKind(StrEnum):
    """What a supervised lane concluded, as the lane's own callbacks name it."""

    PROSE = "prose"
    STARVED = "starved"


@dataclass(frozen=True, slots=True)
class Judgment:
    """One conclusion the supervisor reached, in the order it reached it."""

    kind: JudgmentKind
    value: object


@dataclass(frozen=True, slots=True)
class KeyedHeaders:
    """The inbox keys one assembled message retires, split by disposition."""

    acked: tuple[str, ...]
    nackd: tuple[str, ...]


def supervised_judgments(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """What the supervisor concludes watching this transcript arrive as stdout.

    A supervisor sees bytes, not a file it can seek in, so it has no locus to
    report and leaves it unset; the replay below supplies the locus a
    divergence gets reported at.
    """
    judgments: list[Judgment] = []
    scanner = JsonStdoutScanner(
        lambda text: judgments.append(Judgment(JudgmentKind.PROSE, text)),
        case.driver,
        on_text_starvation=lambda count: judgments.append(
            Judgment(JudgmentKind.STARVED, count)
        ),
    )
    for line in _completed_lines(case):
        scanner.process_line(line)
    scanner.close()
    return tuple(ParityOutput(value=judgment) for judgment in judgments)


def replayed_judgments(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """The same conclusions when the reader replays the persisted transcript."""
    reached: list[Judgment] = []
    fold = SupervisedProseFold(
        lambda text: reached.append(Judgment(JudgmentKind.PROSE, text)),
        on_text_starvation=lambda count: reached.append(
            Judgment(JudgmentKind.STARVED, count)
        ),
        on_activity=lambda: None,
    )
    outputs: list[ParityOutput] = []
    for record in _records(case):
        fold.push(record)
        outputs.extend(
            ParityOutput(value=judgment, at=record[0].at) for judgment in reached
        )
        reached.clear()
    return tuple(outputs)


def archived_headers(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """The keys archival reads out of each message the supervisor delivered."""
    outputs: list[ParityOutput] = []
    for text in _delivered_prose(case):
        headers = KeyedHeaders(
            acked=_segment_keys(extract_ack_segments_from_text(text)),
            nackd=_segment_keys(extract_nack_segments_from_text(text)),
        )
        if headers.acked or headers.nackd:
            outputs.append(ParityOutput(value=headers))
    return tuple(outputs)


def classified_headers(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """The keys the reducer already classified onto the message's spans."""
    outputs: list[ParityOutput] = []
    for message in assembled_messages(case):
        headers = KeyedHeaders(
            acked=_span_keys(message, SpanKind.ACK),
            nackd=_span_keys(message, SpanKind.NACK),
        )
        if headers.acked or headers.nackd:
            outputs.append(ParityOutput(value=headers, at=message.at))
    return tuple(outputs)


def live_starvation_case(directory: Path) -> CorpusCase:
    """One freshly recorded lane that goes quiet, speaks, then goes quiet again.

    The recorded corpus carries every transcript shape the crossing has to
    interpret, but no run of tool calls long enough to starve -- a streak is
    only observable on a lane written to sustain one. It closes on a record
    whose tool call precedes its prose, which is the order that punishes any
    reader keeping only the first fact a line carried.
    """
    records = [_text_record(LIVE_OPENING_PROSE)]
    records.extend(_tool_record(index) for index in range(LIVE_SILENT_RUN))
    records.append(_text_record(LIVE_RESUMED_PROSE))
    records.extend(
        _tool_record(LIVE_SILENT_RUN + index) for index in range(LIVE_SECOND_SILENT_RUN)
    )
    records.append(_tool_then_text_record(len(records), LIVE_TRAILING_PROSE))
    path = directory / "live_starvation.jsonl"
    path.write_text(
        "".join(f"{_stamped(index, record)}\n" for index, record in enumerate(records)),
        encoding="utf-8",
    )
    return CorpusCase(name="live-starvation", path=path, driver=CLAUDE_DRIVER)


def test_the_supervisor_and_a_transcript_replay_reach_the_same_judgments(
    tmp_path: Path,
) -> None:
    """Stdout and the persisted file are one lane, so they say the same things."""
    corpus = parity_corpus(extra=(live_starvation_case(tmp_path),))

    assert_parity(
        supervised_judgments,
        replayed_judgments,
        corpus=corpus,
        labels=("stdout", "replay"),
    )


def test_a_starving_lane_nudges_once_per_streak_on_the_counted_threshold(
    tmp_path: Path,
) -> None:
    """The streak counts silent tool turns and any prose at all starts it over."""
    live = live_starvation_case(tmp_path)

    judgments = [output.value for output in supervised_judgments(live)]

    assert judgments == [
        Judgment(JudgmentKind.PROSE, LIVE_OPENING_PROSE),
        Judgment(JudgmentKind.STARVED, TEXT_STARVATION_THRESHOLD),
        Judgment(JudgmentKind.PROSE, LIVE_RESUMED_PROSE),
        Judgment(JudgmentKind.STARVED, TEXT_STARVATION_THRESHOLD),
        Judgment(JudgmentKind.PROSE, LIVE_TRAILING_PROSE),
    ]
    assert [output.value for output in replayed_judgments(live)] == judgments


def test_the_keys_archival_requests_are_the_keys_the_reducer_classified(
    tmp_path: Path,
) -> None:
    """Archival reads the agent's own text; the ACK header has to survive it."""
    corpus = parity_corpus(extra=(live_starvation_case(tmp_path),))

    assert_parity(
        archived_headers,
        classified_headers,
        corpus=corpus,
        labels=("archived", "spans"),
    )


def test_a_delivered_message_keeps_its_headers_beside_its_directives(
    tmp_path: Path,
) -> None:
    """One record carries prose, both header kinds, and directive lines at once."""
    shapes = next(case for case in parity_corpus() if case.name == "shapes")

    headers = [output.value for output in archived_headers(shapes)]

    assert headers == [KeyedHeaders(acked=("1jN54zgX",), nackd=("1jN552dN",))]
    assert [
        output.value for output in archived_headers(live_starvation_case(tmp_path))
    ] == [
        KeyedHeaders(acked=("1jN5Xq7C",), nackd=()),
        KeyedHeaders(acked=("1jN5Yb2M",), nackd=()),
        KeyedHeaders(acked=("1jN5Zc4T",), nackd=()),
    ]


def _records(case: CorpusCase) -> Iterator[tuple[TranscriptEvent, ...]]:
    """The case's typed facts, regrouped into the records they arrived on.

    A supervisor is handed one printed line at a time, so a replay that pushed
    the whole file at once would be folding a stream shape production never
    sees.
    """
    for _line, events in groupby(typed_events(case), key=lambda event: event.at.line):
        yield tuple(events)


def _completed_lines(case: CorpusCase) -> tuple[str, ...]:
    """The case's finished records, as the bytes a supervisor would have read.

    The forward read reports where the completed transcript ends; a live file's
    half-written last line is one the agent never finished printing, so it is
    not one the supervisor ever scans either.
    """
    data = case.path.read_bytes()[case.cursor_offset : forward_read(case).end_offset]
    return tuple(data.decode("utf-8").splitlines())


def _delivered_prose(case: CorpusCase) -> Iterator[str]:
    """Every message the supervisor handed downstream, in delivery order."""
    for output in supervised_judgments(case):
        judgment = output.value
        if isinstance(judgment, Judgment) and judgment.kind is JudgmentKind.PROSE:
            yield str(judgment.value)


def _segment_keys(segments: Sequence[AckSegment]) -> tuple[str, ...]:
    return tuple(key for segment in segments for key in segment.keys)


def _span_keys(message: AssembledMessage, kind: SpanKind) -> tuple[str, ...]:
    return tuple(
        key for span in message.spans if span.kind is kind for key in span.keys
    )


def _stamped(index: int, record: dict[str, object]) -> str:
    return json.dumps(
        {"timestamp": f"{LIVE_TIMESTAMP_PREFIX}{index:02d}.000Z", **record}
    )


def _text_record(text: str) -> dict[str, object]:
    return {
        "type": "assistant",
        "message": {"stop_reason": "end_turn", "content": [_text_block(text)]},
    }


def _tool_record(index: int) -> dict[str, object]:
    return {"type": "assistant", "message": {"content": [_tool_block(index)]}}


def _tool_then_text_record(index: int, text: str) -> dict[str, object]:
    return {
        "type": "assistant",
        "message": {"content": [_tool_block(index), _text_block(text)]},
    }


def _text_block(text: str) -> dict[str, object]:
    return {"type": "text", "text": text}


def _tool_block(index: int) -> dict[str, object]:
    return {
        "type": "tool_use",
        "id": f"toolu-live-{index}",
        "name": "Bash",
        "input": {"command": "spice task status"},
    }
