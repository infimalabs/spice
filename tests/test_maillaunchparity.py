"""Reducer parity for mail ACK presentation and launch-history replay.

Both crossings consume the shared recorded corpus plus one freshly written
lane fixture.  The mail side compares the public Serve envelope path with a
direct projection of assembled messages; the launch side compares its typed
event entry point with the same already-assembled messages.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

from spice.agent import launchhistory
from spice.agent.driver import CLAUDE_DRIVER
from spice.serve import messages as message_reader
from spice.serve.messagepresentation import (
    AssistantMessage,
    MessagePresenter,
    reply_card_message,
)
from spice.serve.messages import RolloutCursor
from tests.test_transcriptparity import (
    CorpusCase,
    ParityOutput,
    assembled_messages,
    assert_parity,
    parity_corpus,
    typed_events,
)

LIVE_ACK_KEY = "1jN5Xq7C"
LIVE_NACK_KEY = "1jN5Yb2M"
LIVE_RESET_EPOCH = 1_784_280_000
LIVE_START_KEY = "mail-launch-parity-start"
LIVE_TEXT = "\n".join(
    (
        "live preamble",
        "TASK title=Live parity | project=session.transcript",
        f"ACK {LIVE_ACK_KEY}: accepted the live transcript",
        '::git-commit{"sha":"live"}',
        f"NACK {LIVE_NACK_KEY}: refused stale steering",
    )
)


@dataclass(frozen=True, slots=True)
class AckPresentation:
    """Portable user-visible ACK result from one assistant envelope."""

    text: str
    display_text: str
    ack_keys: tuple[str, ...]
    nack_keys: tuple[str, ...]
    utterances: tuple[str, ...]
    dispositions: tuple[str, ...]


def live_mail_launch_case(directory: Path) -> CorpusCase:
    """One live-shaped lane carrying both crossings' load-bearing facts."""
    path = directory / "live_mail_launch.jsonl"
    records = (
        {
            "timestamp": "2026-07-26T06:00:00.000Z",
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": LIVE_TEXT},
                ]
            },
        },
        {
            "timestamp": "2026-07-26T06:00:01.000Z",
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu-live",
                        "name": "Bash",
                        "input": {"command": "spice task status"},
                    }
                ]
            },
        },
        {
            "timestamp": "2026-07-26T06:00:02.000Z",
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "rejected",
                "resetsAt": LIVE_RESET_EPOCH,
            },
        },
    )
    path.write_text(
        "".join(f"{json.dumps(record, separators=(',', ':'))}\n" for record in records),
        encoding="utf-8",
    )
    return CorpusCase(name="live-mail-launch", path=path, driver=CLAUDE_DRIVER)


def served_ack_presentations(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """ACK text emitted by the public transcript-to-envelope path."""
    cursor = RolloutCursor(offset=case.cursor_offset, last_key=LIVE_START_KEY)
    messages = reversed(
        message_reader.read_assistant_messages(
            case.path,
            limit=400,
            after=LIVE_START_KEY,
            cursor=cursor,
            driver=case.driver,
        )
    )
    return tuple(
        ParityOutput(value=_ack_presentation(message))
        for message in messages
        if message.ack_keys
    )


def assembled_ack_presentations(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """The same presentation projected directly from reducer output."""
    outputs: list[ParityOutput] = []
    presenter = MessagePresenter()
    for assembled in assembled_messages(case):
        message = presenter.present(assembled)
        if message is not None and message.ack_keys:
            outputs.append(
                ParityOutput(
                    value=_ack_presentation(message),
                    at=assembled.at,
                )
            )
    return tuple(outputs)


def replayed_launch_narratives(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """Launch history's production event-stream projection."""
    events = typed_events(case)
    return (
        ParityOutput(
            value=launchhistory._launch_log_projection(events),
            at=events[0].at if events else None,
        ),
    )


def assembled_launch_narratives(case: CorpusCase) -> tuple[ParityOutput, ...]:
    """The same launch narrative read from completed reducer messages."""
    messages = assembled_messages(case)
    return (
        ParityOutput(
            value=launchhistory._launch_message_projection(messages),
            at=messages[0].at if messages else None,
        ),
    )


def test_mail_and_launch_match_reducer_spans_on_recorded_and_one_live_transcript(
    tmp_path: Path,
) -> None:
    live = live_mail_launch_case(tmp_path)
    corpus = parity_corpus(extra=(live,))

    assert_parity(
        served_ack_presentations,
        assembled_ack_presentations,
        corpus=corpus,
        labels=("serve envelopes", "reducer spans"),
    )
    assert [output.value for output in served_ack_presentations(live)] == [
        AckPresentation(
            text=LIVE_TEXT,
            display_text="\n".join(
                (
                    "live preamble",
                    "Task capture: Live parity (session.transcript)",
                    "Accepted the live transcript",
                    "Refused stale steering",
                )
            ),
            ack_keys=(LIVE_ACK_KEY, LIVE_NACK_KEY),
            nack_keys=(LIVE_NACK_KEY,),
            utterances=(
                "accepted the live transcript",
                "refused stale steering",
            ),
            dispositions=("acked", "refused"),
        )
    ]
    assert_parity(
        replayed_launch_narratives,
        assembled_launch_narratives,
        corpus=corpus,
        labels=("launch replay", "reducer messages"),
    )
    assert [output.value for output in replayed_launch_narratives(live)] == [
        {
            "assistant_messages": 1,
            "tool_calls": 1,
            "kind": "out-of-credits",
            "reset_epoch": LIVE_RESET_EPOCH,
        }
    ]


def test_bodyless_keyed_reply_honors_ack_but_leaves_reasonless_nack_pending() -> None:
    message = reply_card_message(
        "bodyless",
        1,
        "2026-07-26T06:01:00.000Z",
        f"ACK {LIVE_ACK_KEY}:\nNACK {LIVE_NACK_KEY}:",
    )

    assert message.ack_keys == [LIVE_ACK_KEY]
    assert message.nack_keys == []
    assert message.ack_count == 1
    assert message.nack_count == 0
    assert message.ack_utterances == []
    assert [
        (segment["keys"], segment["disposition"]) for segment in message.ack_segments
    ] == [
        ([LIVE_ACK_KEY], "acked"),
    ]


def _ack_presentation(message: AssistantMessage) -> AckPresentation:
    return AckPresentation(
        text=message.text,
        display_text=message.display_text,
        ack_keys=tuple(message.ack_keys),
        nack_keys=tuple(message.nack_keys),
        utterances=tuple(message.ack_utterances),
        dispositions=tuple(
            str(segment["disposition"]) for segment in message.ack_segments
        ),
    )
