"""Presented assistant messages: directive cards, display text, ack/nack polarity.

Each case writes a transcript and reads it straight back through
``read_assistant_messages``, so nothing here needs a payload, a store, or a task
board. Where a directive may sit and still act is ``test_messagedirectives``;
the task-card rows a payload projects are in ``test_messagepayload``.
"""

from datetime import UTC, datetime

from spice.mail.ackarchive import summarize_nack_archival
from spice.serve import messages as message_reader
from tests.test_messagepayload import _stamp, _write_response_item


def test_inline_task_directive_renders_quote_like_block_in_message(tmp_path):
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
                        "Queued the follow-up.\n"
                        "TASK title=Inline follow-up | project=task.unit | "
                        "acceptance=Tracked from UI | "
                        "acceptance=Rendered on another row\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    assert item.kind == "assistant"
    assert item.task_card_count == 1
    assert item.to_payload()["task_card_count"] == 1
    assert item.display_text == (
        "Queued the follow-up.\nTask capture: Inline follow-up (task.unit)\nContinuing."
    )
    assert "TASK title" not in item.display_text
    assert "TASK title" not in item.display_html
    assert '<blockquote class="task-directive-quote">' in item.display_html
    assert '<div class="task-directive-kicker">Task capture</div>' in item.display_html
    assert "<dt>title</dt><dd>Inline follow-up</dd>" in item.display_html
    assert "<dt>project</dt><dd>task.unit</dd>" in item.display_html
    assert (
        "<dt>acceptance</dt><dd>Tracked from UI</dd></div>"
        '<div class="task-directive-property">'
        "<dt>acceptance</dt><dd>Rendered on another row</dd>" in item.display_html
    )


def test_inline_task_directive_without_acceptance_renders_card(tmp_path):
    # UI-1kD6hDJ6 regression: the supervisor converts a directive that carries
    # only title+project (acceptance omitted -> plan phase), so the UI must
    # render it as a capture card exactly like an acceptance-bearing one --
    # matching the attached example, which stayed raw with an ACK badge.
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
                        "Captured as a separate planning task.\n"
                        "TASK title=Enable and verify the default AFM judge for Spice "
                        "| project=hooks.maxims\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    assert item.task_card_count == 1
    assert item.to_payload()["task_card_count"] == 1
    assert item.display_text == (
        "Captured as a separate planning task.\n"
        "Task capture: Enable and verify the default AFM judge for Spice "
        "(hooks.maxims)\n"
        "Continuing."
    )
    assert '<blockquote class="task-directive-quote">' in item.display_html
    assert '<div class="task-directive-kicker">Task capture</div>' in item.display_html
    assert (
        "<dt>title</dt><dd>Enable and verify the default AFM judge for Spice</dd>"
        in item.display_html
    )
    assert "<dt>project</dt><dd>hooks.maxims</dd>" in item.display_html


def test_malformed_task_like_progress_update_remains_plain_message(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = (
        "TASK badges now use the plum task accent with the count after the label. "
        "I am validating the focused tests next."
    )
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    payload = item.to_payload()
    expected_html = f"<p>{text}</p>"
    assert item.task_card_count == 0
    assert payload["task_card_count"] == item.task_card_count
    assert item.display_text == text
    assert item.display_html == expected_html
    assert payload["display_text"] == text
    assert payload["display_html"] == expected_html
    assert payload["preview"] == text
    assert payload["text"] == text


def test_ordinary_assistant_message_display_replaces_only_terminal_colon(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "Status: ready:"
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

    assert item.text == text
    assert item.display_text == "Status: ready."
    assert item.display_html == "<p>Status: ready.</p>"
    assert item.preview == "Status: ready."


def test_ack_display_replaces_terminal_colon_and_keeps_spoken_text(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "ACK 1k4YggTX: follow-up: queued:"
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

    assert item.text == text
    assert item.display_text == "Follow-up: queued."
    assert item.ack_segments[0]["html"] == "<p>Follow-up: queued.</p>"
    assert item.ack_utterances == ["follow-up: queued:"]
    assert item.preview == "Follow-up: queued."


def test_assistant_message_display_preserves_nonterminal_colon(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "Status: ready."
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

    assert item.display_text == text
    assert item.display_html == f"<p>{text}</p>"
    assert item.preview == text


def test_inline_task_directive_renders_inside_ack_segment_at_written_position(
    tmp_path,
):
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
                        "ACK 1k4YggTX: captured.\n"
                        "TASK: title=ACK follow-up | project=serve.ui | "
                        "acceptance=Inline block appears\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    segment_html = item.ack_segments[0]["html"]

    assert item.ack_count == 1
    assert item.task_card_count == 1
    assert item.ack_utterances == ["captured.\nContinuing."]
    assert item.display_text == (
        "Captured.\nTask capture: ACK follow-up (serve.ui)\nContinuing."
    )
    assert "TASK:" not in segment_html
    assert segment_html.index("<p>Captured.</p>") < segment_html.index(
        '<blockquote class="task-directive-quote">'
    )
    assert segment_html.index('<blockquote class="task-directive-quote">') < (
        segment_html.index("<p>Continuing.</p>")
    )
    assert "<dt>title</dt><dd>ACK follow-up</dd>" in segment_html
    assert "<dt>project</dt><dd>serve.ui</dd>" in segment_html


def test_assistant_message_payload_splits_ack_and_nack_polarity(tmp_path):
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
                        "ACK 1k4YggTX: shipped the doctor rollup.\n"
                        "NACK 1k4Yggrm: refusing — that weakens "
                        "the gate."
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    payload = item.to_payload()

    # ack_keys is the polarity-agnostic union of responded keys, in source order;
    # the counts and nack_keys carry the positive/negative split for tinting.
    assert payload["ack_keys"] == [
        "1k4YggTX",
        "1k4Yggrm",
    ]
    assert payload["ack_count"] == 1
    assert payload["nack_count"] == 1
    assert payload["nack_keys"] == ["1k4Yggrm"]
    assert [seg["disposition"] for seg in payload["ack_segments"]] == [
        "acked",
        "refused",
    ]


def test_assistant_message_payload_marks_a_pure_nack_without_ack_count(tmp_path):
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
                    "text": ("NACK 1k4YggTX: cannot comply with that."),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    payload = item.to_payload()

    # A NACK-led message must not spill its refusal into the preamble, and it
    # must not read as an acknowledgment (no acked tint / ACK chip).
    assert payload["ack_count"] == 0
    assert payload["nack_count"] == 1
    assert payload["nack_keys"] == ["1k4YggTX"]
    assert payload["preamble_html"] == ""
    assert [seg["disposition"] for seg in payload["ack_segments"]] == ["refused"]
    assert "Cannot comply with that." in payload["ack_segments"][0]["html"]


def test_assistant_message_payload_does_not_honor_a_reasonless_nack(tmp_path):
    """A refusal that said nothing renders nothing, not an empty segment.

    A withheld refusal keeps whatever body it carried, so this is the case that
    holds the other edge: with no body there is nothing to keep, and the segment
    stays absent rather than reaching the browser as a blank one to draw.
    """
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "NACK 1k4YggTX:"
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

    assert summary.refused == []
    assert summary.reasonless == ["1k4YggTX"]
    assert payload["ack_count"] == 0
    assert payload["ack_keys"] == []
    assert payload["nack_count"] == 0
    assert payload["nack_keys"] == []
    assert [segment["disposition"] for segment in payload["ack_segments"]] == []
