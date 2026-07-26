"""One contract every driver adapter answers, exercised through the substrate.

These tests never import a dialect decoder. They hold a driver and a raw line and
go through `spice.transcript.decode` and the reader engine that consumes the line
hint, which is the whole of what a consumer above the seam ever touches. What
they pin is therefore the contract itself rather than either adapter's
internals: what the cheap line hint must admit, what a decode must never drop,
and how provenance rides through.

Adding a third driver means adding its adapter, registering it, and adding one
`DriverFixture` entry below with a transcript fixture file. Nothing else in this
file changes -- if a new dialect needs a new assertion here, the contract was not
plane-neutral and the seam is what needs the fix.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import get_args

import pytest

from spice.agent.driver import AgentDriver, all_drivers
from spice.transcript.decode import decode_parsed_line
from spice.transcript.events import (
    UNLOCATED_SOURCE,
    AssistantText,
    TranscriptEvent,
    Unknown,
)
from spice.transcript.reader import (
    TranscriptCursor,
    TranscriptEventReader,
    TranscriptLine,
    read_forward,
    record_assistant_text,
)

FIXTURES = Path(__file__).parent / "fixtures" / "session"

# The closed set itself, read off the union rather than restated, so a kind added
# to the vocabulary widens what adapters may return here without an edit.
VOCABULARY = frozenset(get_args(TranscriptEvent))

PROBE_SOURCE = "probe.jsonl"
PROBE_LINE = 7


@dataclass(frozen=True)
class DriverFixture:
    """One dialect's samples of the shapes the contract is stated over.

    `quiet_line` is a real line from the same transcript that carries no
    assistant prose and that the hint is expected to reject outright, which is
    what keeps a hint from passing the suite by answering True to everything.
    """

    transcript: str
    prose_line: str
    prose: str
    quiet_line: str


FIXTURES_BY_DRIVER: dict[str, DriverFixture] = {
    "codex": DriverFixture(
        transcript="supervised_codex.jsonl",
        prose_line=(
            '{"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":'
            '{"type":"message","role":"assistant","content":'
            '[{"text":"ACK 1jN54zgX: accepted the skill bootstrap from <1kCodex>."}]}}'
        ),
        prose="ACK 1jN54zgX: accepted the skill bootstrap from <1kCodex>.",
        quiet_line=(
            '{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":'
            '{"type":"function_call","name":"exec_command","arguments":"{}"}}'
        ),
    ),
    "claude": DriverFixture(
        transcript="supervised_claude.jsonl",
        prose_line=(
            '{"timestamp":"2026-01-01T01:00:01Z","type":"assistant","message":'
            '{"content":[{"type":"text","text":'
            '"ACK 1jN5ZZg4: accepted the skill bootstrap from <1kClaude>."}]}}'
        ),
        prose="ACK 1jN5ZZg4: accepted the skill bootstrap from <1kClaude>.",
        quiet_line=(
            '{"timestamp":"2026-01-01T01:00:07Z","type":"user","promptId":'
            '"claude-window-1","message":{"content":"recover claude window one"}}'
        ),
    ),
}


def _fixture(driver: AgentDriver) -> DriverFixture:
    fixture = FIXTURES_BY_DRIVER.get(driver.name)
    assert fixture is not None, (
        f"driver {driver.name!r} is registered but declares no conformance "
        f"fixture; add a DriverFixture entry so the shared contract covers it"
    )
    return fixture


def _transcript_lines(driver: AgentDriver) -> list[str]:
    text = (FIXTURES / _fixture(driver).transcript).read_text(encoding="utf-8")
    return [line for line in text.splitlines() if line.strip()]


def _decode(
    line: str,
    driver: AgentDriver,
    *,
    source: str = UNLOCATED_SOURCE,
    number: int = 0,
) -> list[TranscriptEvent]:
    """Decode one fixture line the way the reader hands a record to the crossing.

    Parsing belongs to the reader, so a test stating the decode contract parses
    its own fixture line here rather than reaching for a raw-line entry the
    substrate deliberately no longer offers.
    """
    return decode_parsed_line(json.loads(line), driver, source=source, line=number)


def _record(line: str) -> TranscriptLine:
    """The record the reader engine would carry for one fixture line."""
    return TranscriptLine(raw=line, offset=0, parsed=json.loads(line))


DRIVERS = pytest.mark.parametrize(
    "driver", all_drivers(), ids=lambda driver: driver.name
)


@DRIVERS
def test_substrate_decodes_the_dialects_assistant_prose(driver: AgentDriver) -> None:
    fixture = _fixture(driver)
    texts = record_assistant_text(_record(fixture.prose_line), driver)
    assert [text.text for text in texts] == [fixture.prose]


@DRIVERS
def test_hint_separates_prose_lines_from_quiet_lines(driver: AgentDriver) -> None:
    fixture = _fixture(driver)
    hints = (
        driver.line_may_carry_assistant_text(fixture.prose_line),
        driver.line_may_carry_assistant_text(fixture.quiet_line),
    )
    assert hints == (True, False)


@DRIVERS
def test_quiet_line_carries_no_assistant_prose(driver: AgentDriver) -> None:
    fixture = _fixture(driver)
    assert record_assistant_text(_record(fixture.quiet_line), driver) == ()


@DRIVERS
def test_hint_admits_every_line_the_decoder_reads_as_prose(
    driver: AgentDriver,
) -> None:
    """The prefilter may over-admit; a line it rejects is never parsed again.

    A hint that misses one real prose line silently stops delivering that text
    forever, so this walks a whole real transcript rather than a sample.
    """
    lines = _transcript_lines(driver)
    prose_lines = [
        line
        for line in lines
        if any(isinstance(event, AssistantText) for event in _decode(line, driver))
    ]
    missed = [
        line
        for line in prose_lines
        if driver.line_may_carry_assistant_text(line) is False
    ]
    assert missed == []
    assert len(prose_lines) >= 1


@DRIVERS
def test_the_hint_never_changes_the_prose_the_engine_delivers(
    driver: AgentDriver,
) -> None:
    """The optimization is invisible: same facts, same order, whole transcript.

    The hint exists to skip work, so what makes it safe is that skipping that
    work subtracts nothing. Every `AssistantText` the unfiltered typed stream
    carries comes back from the engine path that consults the hint first,
    stamped identically, across a real transcript of both kinds of line.
    """
    path = FIXTURES / _fixture(driver).transcript
    stream = TranscriptEventReader(path, driver, source_actor=None).read("forward")
    unfiltered = [event for event in stream.events if isinstance(event, AssistantText)]
    hinted = [
        text
        for record in read_forward(path, cursor=TranscriptCursor()).records
        for text in record_assistant_text(record, driver, source=str(path))
    ]
    assert hinted == unfiltered
    assert len(unfiltered) >= 1


@DRIVERS
def test_a_record_the_hint_rejects_never_reaches_the_decoder(
    driver: AgentDriver, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The prefilter has to actually prefilter, not merely agree with the decode.

    Equivalence would hold just as well if the engine ignored the hint and
    decoded everything, so this pins the saving itself: the skipped decode is
    the entire reason the dialect declares the hook.
    """
    decoded: list[int] = []
    decode = type(driver).transcript_line_events

    def counting(
        self: AgentDriver,
        raw: dict[str, object],
        *,
        source: str = UNLOCATED_SOURCE,
        line: int = 0,
    ) -> list[TranscriptEvent]:
        decoded.append(line)
        return decode(self, raw, source=source, line=line)

    monkeypatch.setattr(type(driver), "transcript_line_events", counting)
    fixture = _fixture(driver)
    quiet = record_assistant_text(_record(fixture.quiet_line), driver)
    assert (quiet, decoded) == ((), [])
    prose = record_assistant_text(_record(fixture.prose_line), driver)
    assert ([text.text for text in prose], decoded) == ([fixture.prose], [0])


@DRIVERS
def test_whole_transcript_decodes_into_the_closed_vocabulary(
    driver: AgentDriver,
) -> None:
    """No adapter smuggles a bespoke type up through the seam.

    A line may legitimately decode to no events -- a turn boundary carries no
    fact the vocabulary spells yet -- so what is pinned here is the type of what
    does come back, not how much of it there is.
    """
    kinds = {
        type(event)
        for number, line in enumerate(_transcript_lines(driver), start=1)
        for event in _decode(line, driver, source=PROBE_SOURCE, number=number)
    }
    assert sorted(kind.__name__ for kind in kinds) == sorted(
        kind.__name__ for kind in kinds if kind in VOCABULARY
    )


@DRIVERS
def test_provenance_rides_through_to_every_event(driver: AgentDriver) -> None:
    events = _decode(
        _fixture(driver).prose_line, driver, source=PROBE_SOURCE, number=PROBE_LINE
    )
    loci = [(event.at.source, event.at.line, event.at.ordinal) for event in events]
    assert loci == [(PROBE_SOURCE, PROBE_LINE, index) for index in range(len(events))]


@DRIVERS
def test_provenance_stays_well_formed_across_a_whole_transcript(
    driver: AgentDriver,
) -> None:
    """Ordinals restart per line and ascend from zero, on every real line."""
    observed = []
    expected = []
    for number, line in enumerate(_transcript_lines(driver), start=1):
        events = _decode(line, driver, source=PROBE_SOURCE, number=number)
        observed.append([(e.at.source, e.at.line, e.at.ordinal) for e in events])
        expected.append([(PROBE_SOURCE, number, index) for index in range(len(events))])
    assert observed == expected


@DRIVERS
def test_unparseable_line_stays_one_typed_fact(driver: AgentDriver) -> None:
    """A line the reader could not read as an object arrives here as None."""
    events = decode_parsed_line(None, driver, source=PROBE_SOURCE, line=1)
    assert [type(event) for event in events] == [Unknown]


@DRIVERS
def test_foreign_dialect_line_decodes_totally(driver: AgentDriver) -> None:
    """Handed another dialect's grammar, an adapter still answers in kind.

    An adapter owes no understanding of a foreign line and may read it as no
    events at all. What it may not do is raise, or invent a type: a misrouted
    transcript has to degrade into silence rather than into a crash.
    """
    foreign = [
        fixture.prose_line
        for name, fixture in FIXTURES_BY_DRIVER.items()
        if name != driver.name
    ]
    decoded: list[list[TranscriptEvent]] = [
        _decode(line, driver, source=PROBE_SOURCE, number=1) for line in foreign
    ]
    kinds = {type(event) for events in decoded for event in events}
    assert len(decoded) == len(foreign)
    assert sorted(kind.__name__ for kind in kinds) == sorted(
        kind.__name__ for kind in kinds if kind in VOCABULARY
    )


def test_every_registered_driver_declares_a_conformance_fixture() -> None:
    assert sorted(driver.name for driver in all_drivers()) == sorted(FIXTURES_BY_DRIVER)
