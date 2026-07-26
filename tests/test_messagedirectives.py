"""Where a directive may sit in a message and still act.

One battery over the placement grammar: position within the message, repetition,
markdown markers, supervisor suppression, indentation, and fencing. Every case
writes a transcript and reads the presented message back, so the assertions are
about admission rather than about any payload. How a message renders at all is
``test_messagepresentation``.
"""

from datetime import UTC, datetime

import pytest

from spice.mail.ackarchive import summarize_nack_archival
from spice.mail.ackgrammar import extract_task_batch_lines_from_text
from spice.serve import messages as message_reader
from spice.serve.messagepresentation import SEGMENT_DISPOSITION_WITHHELD
from tests.test_messagepayload import _stamp, _write_response_item


_ACK_KEY = "1k4YggTX"
_NACK_KEY = "1k4Yggrm"
_TASK_DIRECTIVE = (
    "TASK title=Follow up | project=session.transcript | acceptance=Tracked"
)
_APP_DIRECTIVE = '::git-commit{"sha":"abc"}'


def test_a_nack_whose_only_body_is_a_directive_is_not_honored(tmp_path):
    """A refusal that captured a task but gave no reason still needs one.

    Archival strips control lines before it looks for a reason, so this body
    reads as empty to the supervisor and the item stays pending. The display
    keeps the directive whole so it can still become a card, and sharing only
    the predicate was not enough to make the two agree: the same refusal read
    as reasonless to the supervisor and refused on the wire, closing the
    operator's submission while the supervisor was still asking for a reason.

    Withholding the refusal must not withhold the record of what it captured.
    The keys stay pending and the polarity stays unclaimed, so the segment names
    no keys and takes neither ACK-state disposition; the body it carried still
    renders, because the task in it did reach the board. All four hold at once,
    which is the whole contract: an operator sees the capture without the wire
    ever saying this message answered anything.
    """
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = f"NACK {_NACK_KEY}:\n{_TASK_DIRECTIVE}"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    summary = summarize_nack_archival(None, text)
    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert summary.reasonless == [_NACK_KEY]
    assert payload["ack_keys"] == []
    assert payload["nack_keys"] == []
    assert payload["nack_count"] == 0
    assert payload["ack_count"] == 0
    assert [segment["disposition"] for segment in payload["ack_segments"]] == [
        SEGMENT_DISPOSITION_WITHHELD
    ]
    assert [segment["keys"] for segment in payload["ack_segments"]] == [[]]
    assert payload["task_card_count"] == 1
    assert "Follow up" in payload["ack_segments"][0]["html"]
    assert extract_task_batch_lines_from_text(text) == [_TASK_DIRECTIVE]


_DIRECTIVE_POSITIONS = {
    "only-line": f"- {_TASK_DIRECTIVE}",
    "first-line": f"- {_TASK_DIRECTIVE}\nContinuing.",
    "mid-segment": f"Queued.\n- {_TASK_DIRECTIVE}\nContinuing.",
    "last-line": f"Queued.\n- {_TASK_DIRECTIVE}",
    "ack-body-first-line": f"ACK {_ACK_KEY}:\n- {_TASK_DIRECTIVE}\nDone.",
    "ack-header-line": f"ACK {_ACK_KEY}: - {_TASK_DIRECTIVE}\nDone.",
    "nack-body": f"NACK {_NACK_KEY}: cannot comply.\n- {_TASK_DIRECTIVE}",
}


@pytest.mark.parametrize("position", sorted(_DIRECTIVE_POSITIONS))
def test_a_directive_reads_the_same_wherever_it_sits_in_a_message(position, tmp_path):
    """Where a directive sits must not change whether it asks for a task.

    Serve reads segment text the reducer already transformed, so the
    line-leading context recognition depends on can be gone before the
    decision is made: a segment body opens after a stripped ACK header, which
    made a hyphen mid-header look line-leading and invented a card. Every
    position is measured against the supervisor's reading of the raw message,
    which is the only text that still carries that context.
    """
    text = _DIRECTIVE_POSITIONS[position]
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))


def test_a_directive_sharing_an_ack_header_line_stays_acknowledgment_prose(tmp_path):
    """A token mid-header asks for nothing, so the acknowledgment must survive.

    The supervisor reads no directive here, because the hyphen follows the ACK
    header rather than opening the line. Serve saw the body with the header
    already stripped and rewrote the whole acknowledgment to a capture
    summary, showing a card for a task that was never created and destroying
    what the agent actually said.
    """
    text = _DIRECTIVE_POSITIONS["ack-header-line"]
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert extract_task_batch_lines_from_text(text) == []
    assert payload["task_card_count"] == 0
    assert _TASK_DIRECTIVE in payload["display_text"]


_REPEATED_DIRECTIVE_SHAPES = {
    "ack-header-then-live-body": (
        f"ACK {_ACK_KEY}: - {_TASK_DIRECTIVE}\n{_TASK_DIRECTIVE}"
    ),
    "ack-header-then-live-marker-body": (
        f"ACK {_ACK_KEY}: - {_TASK_DIRECTIVE}\n- {_TASK_DIRECTIVE}"
    ),
    "nack-header-then-live-body": (
        f"NACK {_NACK_KEY}: - {_TASK_DIRECTIVE}\n{_TASK_DIRECTIVE}"
    ),
    "nack-header-then-live-marker-body": (
        f"NACK {_NACK_KEY}: - {_TASK_DIRECTIVE}\n- {_TASK_DIRECTIVE}"
    ),
}


@pytest.mark.parametrize("shape", sorted(_REPEATED_DIRECTIVE_SHAPES))
def test_a_repeated_directive_is_admitted_only_where_it_acts(shape, tmp_path):
    """The same directive text twice must not admit the occurrence that asks
    for nothing.

    A card was admitted by directive text, so two identical directives were
    indistinguishable and the occurrence sharing an ACK header borrowed the
    admission the standalone one earned: the supervisor created one task and
    the display showed two cards, rewriting the acknowledgment into a second
    capture summary. Position is what separates them, because the header is
    stripped before this layer sees the line and what remains is character
    for character the genuine directive.
    """
    text = _REPEATED_DIRECTIVE_SHAPES[shape]
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert f"- {_TASK_DIRECTIVE}" in payload["display_text"]


def test_a_shown_directive_keeps_its_card_on_the_one_that_acts(tmp_path):
    """An identical directive shown and issued must card the issued one.

    Admitting by text could only count the pair, never place the card, so a
    fenced example and the live directive below it were interchangeable. The
    card belongs to the line the supervisor read, and the fence has to come
    through intact around the line it was only displaying.
    """
    text = f"```\n{_TASK_DIRECTIVE}\n```\n{_TASK_DIRECTIVE}"
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert f"```\n{_TASK_DIRECTIVE}\n```" in payload["display_text"]


_MARKER_PREFIXED_DIRECTIVES = {
    "bullet-hyphen": f"- {_TASK_DIRECTIVE}",
    "bullet-asterisk": f"* {_TASK_DIRECTIVE}",
    "numbered-item": f"1. {_TASK_DIRECTIVE}",
    "heading": f"## {_TASK_DIRECTIVE}",
    "emphasized-token": _TASK_DIRECTIVE.replace("TASK", "**TASK**", 1),
    "emphasized-line": f"**{_TASK_DIRECTIVE}**",
}


@pytest.mark.parametrize("shape", sorted(_MARKER_PREFIXED_DIRECTIVES))
def test_a_directive_behind_a_markdown_marker_still_renders_its_card(shape, tmp_path):
    """A captured task must be visible however the writer decorated its line.

    The supervisor accepts the list, heading, and emphasis decoration a writer
    naturally puts in front of a directive, but serve required the line to
    begin with the bare token. A bulleted TASK line therefore created a task
    and showed nothing for it — worse than a spurious card, because the board
    moved with no visible confirmation. Prose that merely mentions the token
    stays prose on both sides.
    """
    text = f"Queued the follow-up.\n{_MARKER_PREFIXED_DIRECTIVES[shape]}\nContinuing."
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert len(extract_task_batch_lines_from_text(text)) == 1
    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert "Task capture: Follow up (session.transcript)" in payload["display_text"]


def test_prose_that_mentions_the_task_token_stays_prose(tmp_path):
    """Recognition is line-leading, so a mid-sentence token asks for nothing.

    This is the boundary the marker rule has to hold: widening recognition to
    reach a bulleted directive must not widen it to reach a sentence that
    happens to name the token and its fields.
    """
    text = f"Note. {_TASK_DIRECTIVE}"
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert extract_task_batch_lines_from_text(text) == []
    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert payload["display_text"] == text


_SUPPRESSED_DIRECTIVE_SHAPES = {
    "fenced": "Use this form:\n```\n{directive}\n```\nNothing was captured.",
    "indented": "Use this form:\n\n    {directive}\n\nNothing was captured.",
    "quoted": "They wrote:\n> {directive}\nNothing was captured.",
    "source-context": "spice/serve/taskdirectives.py:88: {directive}",
}


@pytest.mark.parametrize("shape", sorted(_SUPPRESSED_DIRECTIVE_SHAPES))
def test_a_directive_the_supervisor_suppresses_renders_no_card(shape, tmp_path):
    """Serve calls a line a directive exactly when the supervisor would.

    Serve recognized directives with its own line walk, so a TASK line shown
    inside a fence or an indented block — prose documenting the form rather
    than asking for a task — rendered a card for a capture that would never
    happen, and rewrote the line to a summary that destroyed the example it
    was quoting. The quoted and source-context shapes already agreed by
    accident, because their prefix survives into the strip; comparing every
    shape against the supervisor's own reading makes the agreement principled
    instead of coincidental.
    """
    text = _SUPPRESSED_DIRECTIVE_SHAPES[shape].format(directive=_TASK_DIRECTIVE)
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert extract_task_batch_lines_from_text(text) == []
    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert _TASK_DIRECTIVE in payload["display_text"]


_OPENING_DIRECTIVE_SEGMENTS = {
    "preamble": "    {directive}\n\nNothing was captured.",
    "ack-body": f"ACK {_ACK_KEY}:\n" + "    {directive}\n\nNothing was captured.",
    "nack-body": f"NACK {_NACK_KEY}:\n" + "    {directive}\n\nNo capture happened.",
}


@pytest.mark.parametrize("position", sorted(_OPENING_DIRECTIVE_SEGMENTS))
def test_a_directive_opening_its_segment_indented_renders_no_card(position, tmp_path):
    """Indentation is read where the segment starts, not only after it.

    Every reader that trimmed a segment stripped it whole, which took the
    indentation off its first content line and left a bare directive for the
    next reader to believe. The shape only reached production from the opening
    position, because a directive further in kept a preceding line to anchor
    its indentation, so that is the position under test here: the same example
    is written first in a preamble, in an ACK body, and in a NACK body.
    """
    text = _OPENING_DIRECTIVE_SEGMENTS[position].format(directive=_TASK_DIRECTIVE)
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert extract_task_batch_lines_from_text(text) == []
    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert f"    {_TASK_DIRECTIVE}" in payload["display_text"]


_INDENTED_OPENING_LINES = {
    "directive": _TASK_DIRECTIVE,
    "code": "def render(self):",
}


@pytest.mark.parametrize("opening", sorted(_INDENTED_OPENING_LINES))
def test_an_indented_line_opening_a_message_keeps_its_indentation(opening, tmp_path):
    """Where an example sits must not change how it is shown.

    The display trim stripped the rendered message whole, so the first line
    lost its indentation while every line under it kept theirs — an indented
    block came out with a flush opening line and an indented body. Asserting
    the exact text is the point: the earlier regression asked only that the
    directive appear somewhere in display_text, which stayed true while the
    indentation it was written with was being dropped.
    """
    line = _INDENTED_OPENING_LINES[opening]
    body = f"    {line}\n        return 1\n\nThat is the shape."
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": body}],
        },
    )
    moved = tmp_path / "moved.jsonl"
    _write_response_item(
        moved,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": f"Shown here:\n\n{body}"}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()
    moved_payload = message_reader.read_assistant_messages(moved, limit=5)[
        0
    ].to_payload()

    assert payload["display_text"] == body
    assert moved_payload["display_text"] == f"Shown here:\n\n{body}"


def test_an_ack_body_on_the_header_line_still_drops_the_separator_space(tmp_path):
    """Keeping indentation must not mean keeping the marker's own padding.

    The space after `ACK <key>:` belongs to the marker, so the body that
    continues that line still opens flush. Reading it any other way would make
    every ordinary ACK present with a leading space, which is what stripping
    the segment whole used to hide.
    """
    text = f"ACK {_ACK_KEY}: Acknowledged the ask\nand did the work."
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert payload["ack_keys"] == [_ACK_KEY]
    assert payload["display_text"] == "Acknowledged the ask\nand did the work."


def test_a_fenced_app_directive_survives_into_displayed_prose(tmp_path):
    """A shown app directive is prose, and deleting it empties its own fence.

    App-directive lines are dropped from prose rather than rewritten, so
    filtering one that suppression had already excused left the fence around
    it standing with nothing inside.
    """
    text = f"The commit line looks like:\n```\n{_APP_DIRECTIVE}\n```\nThat is all."
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert _APP_DIRECTIVE in payload["display_text"]
    assert payload["display_text"].endswith("That is all.")


@pytest.mark.parametrize(
    ("text", "expected", "expected_dispositions"),
    [
        pytest.param(
            f"NACK {_NACK_KEY}:\n{_TASK_DIRECTIVE}\ncannot comply with that.",
            {
                "ack_count": 0,
                "ack_keys": [_NACK_KEY],
                "nack_count": 1,
                "nack_keys": [_NACK_KEY],
                "task_card_count": 1,
            },
            ["refused"],
            id="nack-opens-with-task-directive",
        ),
        pytest.param(
            f"NACK {_NACK_KEY}:\n{_APP_DIRECTIVE}\ncannot comply with that.",
            {
                "ack_count": 0,
                "ack_keys": [_NACK_KEY],
                "nack_count": 1,
                "nack_keys": [_NACK_KEY],
                "task_card_count": 0,
            },
            ["refused"],
            id="nack-opens-with-app-directive",
        ),
        pytest.param(
            f"ACK {_ACK_KEY}:\n{_TASK_DIRECTIVE}\nshipped the doctor rollup.",
            {
                "ack_count": 1,
                "ack_keys": [_ACK_KEY],
                "nack_count": 0,
                "nack_keys": [],
                "task_card_count": 1,
            },
            ["acked"],
            id="ack-opens-with-task-directive",
        ),
    ],
)
def test_keyed_response_opening_with_a_directive_keeps_its_polarity(
    tmp_path,
    text: str,
    expected: dict[str, object],
    expected_dispositions: list[str],
):
    """A control line belongs to the run it sits in and cannot retint it.

    Here the directive is the first thing the run classifies. While the run's
    polarity had to be inferred for a line that carried none, a refusal that
    captured a task first was published as an acknowledgment: one extra acked
    segment, and a key acked and refused at once vanished from `nack_keys`.
    Each shape stays one segment, and the task card still counts.
    """
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    payload = item.to_payload()

    assert {field: payload[field] for field in expected} == expected
    assert [
        seg["disposition"] for seg in payload["ack_segments"]
    ] == expected_dispositions


def test_inline_task_directive_counts_multiple_task_cards(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "TASK title=First follow-up | project=serve.ui | "
                        "acceptance=First card\n"
                        "TASK title=Second follow-up | project=task.unit | "
                        "acceptance=Second card"
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]

    assert item.task_card_count == 2
    assert item.to_payload()["task_card_count"] == 2
    assert '<div class="task-directive-stack">' in item.display_html
    assert item.display_html.count('class="task-directive-quote"') == 2
