"""Cursor/tail-merge coverage for the reply-card payload merge (UI-1kBNL1Rs).

Mirrors the task-card tests in test_messagepayload.py for the reply-card path,
which reuses the same _merge_synthetic_cards but sources cards from
read_reply_records instead of the task backend.
"""

from spice.serve import messages as message_reader
from spice.serve.payload import message


def _message(timestamp):
    return message_reader.AssistantMessage(
        key=f"{timestamp}#0",
        index=0,
        timestamp=timestamp,
        text="hello",
        display_text="hello",
        display_html="<p>hello</p>",
        ack_count=0,
        ack_keys=[],
        ack_utterances=[],
        kind="assistant",
    )


def test_reply_card_cursor_keeps_append_window_to_transcript_items(
    monkeypatch, tmp_path
):
    records = [
        {"timestamp": "2026-06-10T12:00:01.000001Z", "text": "ACK k1: older reply"},
        {"timestamp": "2026-06-10T12:00:02.000001Z", "text": "ACK k2: newer reply"},
    ]
    monkeypatch.setattr(message, "read_reply_records", lambda _root, _thread: records)
    boundary_key = "2026-06-10T12:00:01.000001Z#reply-card:0"

    merged = message._merge_reply_card_messages(
        "a" * 32,
        [_message("2026-06-10T12:00:03.000000Z")],
        repo_root=tmp_path,
        worktree_id="wt",
        limit=5,
        after=boundary_key,
    )

    keys = [item.key for item in merged]
    assert "2026-06-10T12:00:01.000001Z#reply-card:0" not in keys  # cursor drops older
    assert "2026-06-10T12:00:02.000001Z#reply-card:1" in keys  # newer kept
    boundary = message_reader.parse_timestamp("2026-06-10T12:00:01.000001Z")
    assert boundary is not None
    assert all(
        (ts := message_reader.parse_timestamp(item.timestamp)) is not None
        and ts > boundary
        for item in merged
    )


def test_reply_card_tail_merge_drops_cards_older_than_visible_window(
    monkeypatch, tmp_path
):
    records = [
        {"timestamp": "2026-06-10T06:00:00.000001Z", "text": "ACK k0: stale reply"},
        {"timestamp": "2026-06-10T12:00:01.000001Z", "text": "ACK k1: fresh reply"},
    ]
    monkeypatch.setattr(message, "read_reply_records", lambda _root, _thread: records)

    merged = message._merge_reply_card_messages(
        "a" * 32,
        [_message("2026-06-10T12:00:00.000000Z")],
        repo_root=tmp_path,
        worktree_id="wt",
        limit=5,
    )

    keys = [item.key for item in merged]
    assert "2026-06-10T06:00:00.000001Z#reply-card:0" not in keys  # stale dropped
    assert "2026-06-10T12:00:01.000001Z#reply-card:1" in keys  # fresh kept
