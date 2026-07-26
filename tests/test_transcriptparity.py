"""One differential harness every transcript-interpreter crossing replays through.

Each crossing in this plane retires a hand-rolled interpreter in favour of the
public typed reader plus the assembled-message reducer, and at the moment of
replacement exactly one question matters: does the new path say what the old one
said, on real transcript? That question is the same question five times over --
serve envelopes, session turn records, watchdog judgments, mail ACK text,
launch narratives -- so the corpus, the replay, and the comparison live here
once and each crossing plugs in its own pair of production interpreters.

The harness owns no semantics. It never decides what a line means: both sides of
a comparison are production code and the corpus is recorded transcript, so a
divergence reports two real outputs and the source locus that produced them
rather than a third opinion about which one is right. A crossing that needs a
freshly recorded lane transcript appends it as one more case through ``extra``.

The corpus is three cases per dialect. `transcript/parity_{claude,codex}.jsonl`
each carry one prompt boundary, one assistant line holding prose plus a task
directive plus an ACK plus an app directive plus a NACK plus an image (and, for
Claude, a tool call and reasoning too), a tool exchange, a reasoning-only turn,
a compaction, a final answer, one corrupt record, and one unterminated record a
live writer is still flushing. The same file replayed from a mid-transcript
cursor is the boundary case. `session/supervised_*.jsonl`, the recorded
supervised lanes, supply turn boundaries and repeated compactions at volume.

The two tails are different shapes and the reader treats them differently: a
corrupt but complete record decodes to `Unknown`, while the mid-flush record is
held back by the cursor-owned forward path until its writer finishes. Every
access path is therefore compared over the same completed span.
"""

from __future__ import annotations

import importlib
from collections import Counter
from collections.abc import Callable, Sequence
from dataclasses import dataclass, replace
from enum import StrEnum
from pathlib import Path

import pytest

from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER, AgentDriver
from spice.transcript.assembly import (
    AssembledMessage,
    AssembledMessageReducer,
    ClassifiedSpan,
    DirectiveKind,
    SpanKind,
)
from spice.transcript.events import Provenance, TranscriptEvent, Unknown
from spice.transcript.reader import (
    TranscriptCursor,
    TranscriptEventRead,
    TranscriptEventReader,
    transcript_size,
)

TRANSCRIPT_FIXTURES = Path(__file__).parent / "fixtures" / "transcript"
SESSION_FIXTURES = Path(__file__).parent / "fixtures" / "session"
CORPUS_SOURCE_ACTOR = "thread:parity-corpus"
DEFAULT_LABELS = ("current", "reducer")


class _NoOutput:
    """Stands in for the output one side never produced.

    A sentinel object rather than a string, so an interpreter that really does
    emit "<no output>" still reads as a value one side has and the other lacks.
    """

    def __repr__(self) -> str:
        return "<no output>"


NO_OUTPUT = _NoOutput()

# What a crossing suite imports from here instead of restating a corpus or a
# comparison of its own.
HARNESS_API = (
    "CorpusCase",
    "CorpusShape",
    "Divergence",
    "ParityOutput",
    "assembled_messages",
    "assert_parity",
    "first_divergence",
    "observed_shapes",
    "parity_corpus",
    "typed_events",
)

# Where the resumed case picks the transcript up, and where the split-read pair
# below cuts one access pass in two. Both come from the reader's own provenance,
# so both land on a record start and neither exercises partial-line alignment,
# which has its own reader tests.
RESUME_RECORD_INDEX = 3
SPLIT_RECORD_INDEX = 2

# Where the deliberately divergent interpreter disagrees. It disagrees twice, so
# a report naming the first one proves the scan stops at the earliest crossing.
DIVERGENT_INDEX = 2
LATER_DIVERGENT_INDEX = 5

# Distinct event types a single multi-frame record has to decode into.
MULTI_EVENT_TYPE_MINIMUM = 2


class CorpusShape(StrEnum):
    """The transcript shapes a crossing has to keep interpreting identically."""

    MULTI_EVENT_LINE = "multi-event-line"
    ACK = "ack"
    NACK = "nack"
    TASK_DIRECTIVE = "task-directive"
    COMPACTION = "compaction"
    IMAGE = "image"
    REASONING_ONLY_TURN = "reasoning-only-turn"
    MALFORMED_TAIL = "malformed-tail"
    PARTIAL_TAIL = "partial-tail"
    CURSOR_BOUNDARY = "cursor-boundary"


SEMANTIC_SHAPES = frozenset(CorpusShape) - {CorpusShape.CURSOR_BOUNDARY}
RECORDED_SHAPES = frozenset(
    {
        CorpusShape.MULTI_EVENT_LINE,
        CorpusShape.ACK,
        CorpusShape.NACK,
        CorpusShape.COMPACTION,
    }
)
SHAPE_SPAN_KINDS = {
    CorpusShape.ACK: SpanKind.ACK,
    CorpusShape.NACK: SpanKind.NACK,
    CorpusShape.COMPACTION: SpanKind.COMPACTION,
    CorpusShape.IMAGE: SpanKind.IMAGE,
}


@dataclass(frozen=True, slots=True)
class CorpusCase:
    """One transcript replayed from one starting cursor by both interpreters."""

    name: str
    path: Path
    driver: AgentDriver
    shapes: frozenset[CorpusShape] = frozenset()
    cursor_offset: int = 0
    source_actor: str | None = CORPUS_SOURCE_ACTOR

    @property
    def label(self) -> str:
        return f"{self.driver.name}/{self.name}"


@dataclass(frozen=True, slots=True)
class ParityOutput:
    """One production output and the transcript locus it was derived from.

    The locus is what makes a divergence actionable rather than merely true, so
    an interpreter that can name where its output came from should. One that
    cannot -- a turn record folded from many lines -- leaves it unset and the
    report falls back to the other side's locus.
    """

    value: object
    at: Provenance | None = None


Interpreter = Callable[[CorpusCase], Sequence[ParityOutput]]


@dataclass(frozen=True, slots=True)
class Divergence:
    """The first place two interpreters stopped agreeing, and what each said."""

    case: CorpusCase
    index: int
    at: Provenance | None
    left: object
    right: object
    labels: tuple[str, str]

    @property
    def locus(self) -> str:
        if self.at is None:
            return f"{self.case.path}:<unlocated>"
        return f"{self.at.source}:{self.at.line}#{self.at.ordinal}"

    def report(self) -> str:
        left_label, right_label = self.labels
        return "\n".join(
            (
                f"transcript parity divergence in corpus case {self.case.label}",
                f"  output index {self.index} at {self.locus}",
                f"  {left_label}: {self.left!r}",
                f"  {right_label}: {self.right!r}",
            )
        )


def parity_corpus(*, extra: Sequence[CorpusCase] = ()) -> tuple[CorpusCase, ...]:
    """The shared recorded corpus, plus whatever live cases a crossing adds."""
    cases: list[CorpusCase] = []
    for driver, shapes_fixture, recorded in (
        (CLAUDE_DRIVER, "parity_claude.jsonl", "supervised_claude.jsonl"),
        (CODEX_DRIVER, "parity_codex.jsonl", "supervised_codex.jsonl"),
    ):
        shaped = CorpusCase(
            name="shapes",
            path=TRANSCRIPT_FIXTURES / shapes_fixture,
            driver=driver,
            shapes=SEMANTIC_SHAPES,
        )
        cases.append(shaped)
        cases.append(
            replace(
                shaped,
                name="resumed",
                shapes=frozenset({CorpusShape.CURSOR_BOUNDARY}),
                cursor_offset=record_offsets(shaped)[RESUME_RECORD_INDEX],
            )
        )
        cases.append(
            CorpusCase(
                name="recorded",
                path=SESSION_FIXTURES / recorded,
                driver=driver,
                shapes=RECORDED_SHAPES,
            )
        )
    return (*cases, *extra)


def record_offsets(case: CorpusCase) -> tuple[int, ...]:
    """Each record start the case reaches, taken from the reader's provenance.

    A cursor has to land on a record boundary, and the reader already stamps the
    boundary it read every fact from, so the harness asks it rather than
    counting newlines itself.
    """
    return tuple(
        dict.fromkeys(
            event.at.offset
            for event in typed_events(case)
            if event.at.offset is not None
        )
    )


def typed_events(case: CorpusCase) -> tuple[TranscriptEvent, ...]:
    """Every typed fact the case carries, read forward from its own cursor."""
    return forward_read(case).events


def forward_read(case: CorpusCase) -> TranscriptEventRead:
    """One cursor-owned forward pass, kept whole for its completed end offset.

    A live transcript can be mid-flush, and the forward path holds that
    unterminated last line back; the read reports where the completed transcript
    ends, which is the span every other access path has to be asked for.
    """
    reader = TranscriptEventReader(case.path, case.driver, case.source_actor)
    cursor = TranscriptCursor(offset=case.cursor_offset)
    return reader.read("forward", cursor=cursor)


def assembled_messages(case: CorpusCase) -> tuple[AssembledMessage, ...]:
    """The case's typed facts folded into classified assistant messages."""
    reducer = AssembledMessageReducer()
    messages: list[AssembledMessage] = []
    for event in typed_events(case):
        messages.extend(reducer.push(event))
    messages.extend(reducer.finish())
    return tuple(messages)


def first_divergence(
    case: CorpusCase,
    left: Sequence[ParityOutput],
    right: Sequence[ParityOutput],
    *,
    labels: tuple[str, str] = DEFAULT_LABELS,
) -> Divergence | None:
    """Compare two output sequences in source order and stop at the first gap."""
    for index in range(max(len(left), len(right))):
        left_output = left[index] if index < len(left) else None
        right_output = right[index] if index < len(right) else None
        left_value = NO_OUTPUT if left_output is None else left_output.value
        right_value = NO_OUTPUT if right_output is None else right_output.value
        if left_value == right_value:
            continue
        # Either side can be the one that carries a locus; take whichever does.
        at = next(
            (out.at for out in (left_output, right_output) if out and out.at), None
        )
        return Divergence(
            case=case,
            index=index,
            at=at,
            left=left_value,
            right=right_value,
            labels=labels,
        )
    return None


def assert_parity(
    left: Interpreter,
    right: Interpreter,
    *,
    corpus: Sequence[CorpusCase] | None = None,
    labels: tuple[str, str] = DEFAULT_LABELS,
) -> None:
    """Replay every case through both interpreters and demand identical output.

    Agreement only means something if something was actually compared, so a
    corpus that selected nothing and a pair that produced nothing are failures
    here rather than the quietest possible green.
    """
    cases = tuple(parity_corpus() if corpus is None else corpus)
    assert cases, "parity corpus selected no cases, so nothing was replayed"
    compared = 0
    for case in cases:
        left_outputs = tuple(left(case))
        right_outputs = tuple(right(case))
        divergence = first_divergence(case, left_outputs, right_outputs, labels=labels)
        assert divergence is None, divergence.report()
        compared += max(len(left_outputs), len(right_outputs))
    assert compared, (
        f"both interpreters produced no output across {len(cases)} corpus case(s)"
    )


def observed_shapes(case: CorpusCase) -> frozenset[CorpusShape]:
    """The shapes the case actually carries, read off its own typed replay."""
    events = typed_events(case)
    messages = assembled_messages(case)
    spans = tuple(span for message in messages for span in message.spans)
    lines = Counter(event.at.line for event in events)
    observed = {
        shape
        for shape, kind in SHAPE_SPAN_KINDS.items()
        if any(span.kind is kind for span in spans)
    }
    carried = (
        (CorpusShape.MULTI_EVENT_LINE, any(count > 1 for count in lines.values())),
        (
            CorpusShape.TASK_DIRECTIVE,
            any(span.directive_kind is DirectiveKind.TASK for span in spans),
        ),
        (
            CorpusShape.REASONING_ONLY_TURN,
            any(_is_reasoning_only(message) for message in messages),
        ),
        (
            CorpusShape.MALFORMED_TAIL,
            any(isinstance(event, Unknown) for event in events),
        ),
        (CorpusShape.PARTIAL_TAIL, _holds_back_a_partial_line(case)),
        (CorpusShape.CURSOR_BOUNDARY, _resumes_mid_transcript(case, events)),
    )
    observed.update(shape for shape, present in carried if present)
    return frozenset(observed)


def _holds_back_a_partial_line(case: CorpusCase) -> bool:
    """True when the case ends mid-flush, so the forward pass stops short."""
    return forward_read(case).end_offset < (transcript_size(case.path) or 0)


def _is_reasoning_only(message: AssembledMessage) -> bool:
    return bool(message.spans) and all(
        span.kind is SpanKind.REASONING for span in message.spans
    )


def _resumes_mid_transcript(
    case: CorpusCase, events: Sequence[TranscriptEvent]
) -> bool:
    if not case.cursor_offset or not events:
        return False
    whole = typed_events(replace(case, cursor_offset=0))
    return len(events) < len(whole) and tuple(whole[-len(events) :]) == tuple(events)


def whole_pass_events(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """One forward access pass, as the reader engine delivers it."""
    return tuple(ParityOutput(value=event, at=event.at) for event in typed_events(case))


def split_pass_events(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """The same span of transcript, taken as two bounded reads instead of one."""
    reader = TranscriptEventReader(case.path, case.driver, case.source_actor)
    offsets = record_offsets(case)
    split = (
        offsets[min(SPLIT_RECORD_INDEX, len(offsets) - 1)]
        if offsets
        else case.cursor_offset
    )
    complete = forward_read(case).end_offset
    head = reader.read("bounded", start_offset=case.cursor_offset, end_offset=split)
    tail = reader.read("bounded", start_offset=split, end_offset=complete)
    return tuple(
        ParityOutput(value=event, at=event.at) for event in (*head.events, *tail.events)
    )


DRIVERS = pytest.mark.parametrize(
    "driver", (CLAUDE_DRIVER, CODEX_DRIVER), ids=lambda driver: driver.name
)


def test_two_production_access_paths_agree_across_the_whole_corpus() -> None:
    """The harness passes when both sides are real, differently-taken readings.

    A harness that only ever fails is untrustworthy for the opposite reason a
    harness that only ever passes is, so the agreeing pair here is genuinely two
    production access paths -- one forward pass against two bounded reads spliced
    at a record boundary -- rather than one path compared with itself.
    """
    assert_parity(
        whole_pass_events,
        split_pass_events,
        labels=("whole-pass", "split-pass"),
    )


def test_a_divergent_pair_reports_the_first_locus_and_both_outputs() -> None:
    case = _case("shapes", CLAUDE_DRIVER)
    expected = whole_pass_events(case)[DIVERGENT_INDEX]

    with pytest.raises(AssertionError) as failure:
        assert_parity(whole_pass_events, _altered_outputs, corpus=(case,))

    report = str(failure.value)
    assert case.label in report
    assert f"output index {DIVERGENT_INDEX} " in report
    assert f"{expected.at.source}:{expected.at.line}#{expected.at.ordinal}" in report
    assert repr(expected.value) in report
    assert repr(f"altered at {DIVERGENT_INDEX}") in report


def test_a_missing_trailing_output_diverges_where_the_sequence_ends() -> None:
    case = _case("shapes", CODEX_DRIVER)
    complete = whole_pass_events(case)

    def truncated(replayed: CorpusCase) -> tuple[ParityOutput, ...]:
        return whole_pass_events(replayed)[:-1]

    with pytest.raises(AssertionError) as failure:
        assert_parity(whole_pass_events, truncated, corpus=(case,))

    report = str(failure.value)
    assert f"output index {len(complete) - 1} " in report
    assert repr(complete[-1].value) in report
    assert repr(NO_OUTPUT) in report


def test_a_corpus_that_selected_nothing_fails_instead_of_passing_quietly() -> None:
    with pytest.raises(AssertionError) as failure:
        assert_parity(whole_pass_events, split_pass_events, corpus=())

    assert "nothing was replayed" in str(failure.value)


def test_two_interpreters_that_produce_nothing_fail_instead_of_agreeing() -> None:
    case = _case("shapes", CLAUDE_DRIVER)

    with pytest.raises(AssertionError) as failure:
        assert_parity(_no_outputs, _no_outputs, corpus=(case,))

    assert "produced no output" in str(failure.value)


@DRIVERS
def test_the_corpus_covers_every_named_shape_for_each_driver(
    driver: AgentDriver,
) -> None:
    """Coverage is declared per case and answered by the transcript itself.

    A corpus that merely claims to carry a shape decays silently as fixtures are
    edited, so each case's declaration is checked against what its own typed
    replay actually produces.
    """
    cases = [case for case in parity_corpus() if case.driver is driver]
    declared = frozenset(shape for case in cases for shape in case.shapes)

    assert declared == frozenset(CorpusShape)
    assert [
        (case.label, sorted(case.shapes - observed_shapes(case))) for case in cases
    ] == [(case.label, []) for case in cases]


@DRIVERS
def test_a_multi_event_source_line_keeps_its_typed_multiplicity(
    driver: AgentDriver,
) -> None:
    """One source line reaches the harness as every fact it carried, in order."""
    events = typed_events(_case("shapes", driver))
    by_line: dict[int, list[TranscriptEvent]] = {}
    for event in events:
        by_line.setdefault(event.at.line, []).append(event)
    _line, carried = max(by_line.items(), key=lambda item: len(item[1]))

    assert [event.at.ordinal for event in carried] == list(range(len(carried)))
    assert len({type(event) for event in carried}) >= MULTI_EVENT_TYPE_MINIMUM


@DRIVERS
def test_a_resumed_case_replays_only_the_tail_after_its_cursor(
    driver: AgentDriver,
) -> None:
    resumed = _case("resumed", driver)
    whole = typed_events(_case("shapes", driver))
    tail = typed_events(resumed)

    assert tail == whole[len(whole) - len(tail) :]
    assert len(tail) < len(whole)
    assert min(event.at.offset or 0 for event in tail) >= resumed.cursor_offset


def test_a_live_fixture_replays_beside_the_recorded_corpus(tmp_path: Path) -> None:
    """A crossing pins one freshly recorded lane transcript without a fork here."""
    path = tmp_path / "live.jsonl"
    path.write_text(
        '{"timestamp":"2026-07-26T04:00:00.000Z","type":"assistant","message":'
        '{"stop_reason":"end_turn","content":[{"type":"text","text":'
        '"ACK 1jN54zgX: live lane"}]}}\n',
        encoding="utf-8",
    )
    live = CorpusCase(name="live", path=path, driver=CLAUDE_DRIVER)

    corpus = parity_corpus(extra=(live,))

    assert corpus[-1] == live
    assert [span.keys for span in _spans(live) if span.kind is SpanKind.ACK] == [
        ("1jN54zgX",)
    ]
    assert_parity(whole_pass_events, split_pass_events, corpus=(live,))


def test_a_crossing_suite_can_import_the_harness_by_package_path() -> None:
    """Serve, sessions, watchdog, mail, and launch history all reach it this way.

    The five crossings land one at a time, each in its own lane, and every one of
    them needs the corpus and the comparison rather than a copy. The seam they
    reach through is the suite's existing shared-helper import, so it is pinned
    here rather than discovered by whichever crossing lands first.
    """
    module = importlib.import_module(f"tests.{Path(__file__).stem}")

    assert [name for name in HARNESS_API if hasattr(module, name)] == list(HARNESS_API)


def _case(name: str, driver: AgentDriver) -> CorpusCase:
    return next(
        case for case in parity_corpus() if case.name == name and case.driver is driver
    )


def _spans(case: CorpusCase) -> list[ClassifiedSpan]:
    return [span for message in assembled_messages(case) for span in message.spans]


def _no_outputs(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """An interpreter that reads nothing at all, which is never parity."""
    return ()


def _altered_outputs(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """An interpreter that disagrees twice, so the report has to pick the first."""
    outputs = list(whole_pass_events(case))
    for index in (DIVERGENT_INDEX, LATER_DIVERGENT_INDEX):
        outputs[index] = replace(outputs[index], value=f"altered at {index}")
    return tuple(outputs)
