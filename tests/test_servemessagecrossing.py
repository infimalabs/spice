"""Serve envelopes stay identical however the transcript was reached.

Serve's message projection lost its private transcript scanner in favour of the
shared reader engine plus the assembled-message reducer, and the question that
crossing has to answer is whether the envelopes the browser receives still say
what they said. It is the same question the other transcript crossings ask, so
the corpus, the replay and the comparison come from the shared parity harness
and this suite supplies only serve's own pair of production interpreters.

Both sides are production reads of the same completed span: the tail window the
browser opens a lane with, and the cursor-resumed forward read its live stream
resumes through. They reach the transcript through different access modes, keep
different caches, and page differently, so agreement between them is evidence
about the projection rather than about one path compared with itself.

Presence records are excluded from the comparison deliberately. Which activity
records survive a read is paging policy -- the tail window retains the newest
one plus supervisor feedback, a resumed read retains what it just scanned -- and
policy is exactly what this crossing did not move.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from spice.agent.driver import CLAUDE_DRIVER
from spice.serve.messages import (
    MAX_MESSAGE_LIMIT,
    AssistantMessage,
    RolloutCursor,
    read_assistant_messages,
)
from spice.transcript.events import Provenance
from tests.test_transcriptparity import (
    CorpusCase,
    ParityOutput,
    assert_parity,
    parity_corpus,
)

WORKTREE_ID = "wt"
RESUME_KEY = "resume"
PRESENCE_PREFIX = "presence:"
LIVE_LANE_LINES = (
    '{"timestamp":"2026-07-26T05:00:00.000Z","type":"assistant","message":'
    '{"content":[{"type":"text","text":"Reading the lane transcript back.\\n\\n'
    'ACK 1jN54zgX: replayed the recorded corpus"}]}}',
    '{"timestamp":"2026-07-26T05:00:01.000Z","type":"assistant","message":'
    '{"content":[{"type":"tool_use","id":"toolu-live","name":"Bash",'
    '"input":{"command":"spice task list"}}]}}',
    '{"timestamp":"2026-07-26T05:00:02.000Z","type":"user","message":'
    '{"content":[{"type":"tool_result","tool_use_id":"toolu-live",'
    '"content":"claim TRANSCR-1kGsjN4V"}]}}',
    '{"timestamp":"2026-07-26T05:00:03.000Z","type":"assistant","message":'
    '{"stop_reason":"end_turn","content":[{"type":"text","text":'
    '"The crossing holds on live transcript too."}]}}',
)


def _visible(messages: list[AssistantMessage]) -> tuple[ParityOutput, ...]:
    """Every envelope an operator sees, oldest first, with its source locus."""
    return tuple(
        ParityOutput(
            value=message.to_payload(),
            at=Provenance(
                source=message.key,
                line=message.index,
                ordinal=0,
                timestamp=message.timestamp,
                offset=message.index,
            ),
        )
        for message in reversed(messages)
        if not message.kind.startswith(PRESENCE_PREFIX)
    )


def tail_window_envelopes(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """What a lane shows on open: one reverse window over the whole transcript."""
    return _visible(
        read_assistant_messages(
            case.path,
            limit=MAX_MESSAGE_LIMIT,
            worktree_id=WORKTREE_ID,
            driver=case.driver,
        )
    )


def resumed_stream_envelopes(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """What the live stream delivers: a forward read resumed from its cursor."""
    cursor = RolloutCursor(offset=case.cursor_offset, last_key=RESUME_KEY)
    return _visible(
        read_assistant_messages(
            case.path,
            limit=MAX_MESSAGE_LIMIT,
            after=RESUME_KEY,
            cursor=cursor,
            worktree_id=WORKTREE_ID,
            driver=case.driver,
        )
    )


@pytest.fixture
def live_lane(tmp_path: Path) -> CorpusCase:
    """One freshly recorded lane transcript, replayed beside the corpus.

    The recorded corpus fixes the shapes; a live lane fixes that the crossing
    still holds on transcript written the way an agent is writing one right now,
    prose and ACK on one line through a tool exchange to a final answer.
    """
    path = tmp_path / "live.jsonl"
    path.write_text("\n".join(LIVE_LANE_LINES) + "\n", encoding="utf-8")
    return CorpusCase(name="live", path=path, driver=CLAUDE_DRIVER)


def _whole_transcript_corpus(live: CorpusCase) -> tuple[CorpusCase, ...]:
    """The cases both paths can be asked for: every one read from its start.

    A resumed case starts mid-transcript by construction, and the tail window
    has no cursor to honour, so the two paths are answering different questions
    there. Those cases are covered by the harness's own cursor-boundary replay.
    """
    return tuple(
        case for case in parity_corpus(extra=(live,)) if not case.cursor_offset
    )


def test_the_tail_window_and_the_resumed_stream_carry_identical_envelopes(
    live_lane: CorpusCase,
) -> None:
    corpus = _whole_transcript_corpus(live_lane)

    assert_parity(
        tail_window_envelopes,
        resumed_stream_envelopes,
        corpus=corpus,
        labels=("tail-window", "resumed-stream"),
    )


def test_the_live_lane_case_is_replayed_alongside_the_recorded_corpus(
    live_lane: CorpusCase,
) -> None:
    """The live case is compared, not merely appended to a list."""
    corpus = _whole_transcript_corpus(live_lane)
    envelopes = tail_window_envelopes(live_lane)

    assert corpus[-1] == live_lane
    assert [payload.value["kind"] for payload in envelopes] == ["assistant", "final"]
    assert [payload.value["ack_keys"] for payload in envelopes] == [["1jN54zgX"], []]


def test_a_projection_that_drops_an_envelope_is_reported_against_the_tail(
    live_lane: CorpusCase,
) -> None:
    """The pair fails when it should: parity here is a claim, not a formality."""

    def dropped(case: CorpusCase) -> tuple[ParityOutput, ...]:
        return resumed_stream_envelopes(case)[:-1]

    with pytest.raises(AssertionError) as failure:
        assert_parity(tail_window_envelopes, dropped, corpus=(live_lane,))

    assert live_lane.label in str(failure.value)
