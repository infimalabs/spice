"""Inbox steering: durable publish, payload round-trip, ACK retirement."""

from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.attachments import (
    find_archived_inbox_attachment_references,
    inbox_attachment_dir,
    prepare_inbox_attachments,
)
from spice.mail.inbox import (
    INBOX_CONTROL_DRAIN_QUEUE,
    INBOX_CONTINUE_NOTE,
    INBOX_GRACEFUL_NOTE,
    INBOX_TASK_HINT_ROW,
    collect_inbox_items,
    compose_inbox_text,
    inbox_dir,
    inbox_item_key,
    inbox_item_key_aliases,
    inbox_payload_rows,
    parse_inbox_payload,
    pending_inbox_count,
    write_inbox_item,
)
from spice.serve.markdown import render_message_html

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="


def test_write_then_collect_round_trip(tmp_path):
    composed = compose_inbox_text(body="steer left", priority=None, stop=False)
    written = write_inbox_item(tmp_path, "20260101T000000000001Z.txt", composed)
    items = collect_inbox_items(str(tmp_path))
    assert [item.name for item in items] == ["20260101T000000000001Z.txt"]
    assert items[0].text == composed
    assert written.parent == inbox_dir(tmp_path)
    assert pending_inbox_count(str(tmp_path)) == 1


def test_compose_parse_round_trip_with_priority_and_stop():
    composed = compose_inbox_text(body="wrap it up", priority="urgent", stop=True)
    parsed = parse_inbox_payload(composed)
    assert parsed.priority == "urgent"
    assert parsed.body == "wrap it up"
    assert parsed.is_stop is True
    assert INBOX_GRACEFUL_NOTE in composed


def test_compose_normal_priority_stays_implicit():
    composed = compose_inbox_text(body="keep going", priority=None, stop=False)
    assert composed == f"keep going\nNote: {INBOX_CONTINUE_NOTE}\n"


def test_compose_parse_and_readout_keep_controls_out_of_body(tmp_path):
    composed = compose_inbox_text(
        body="keep draining",
        priority=None,
        stop=False,
        controls=(INBOX_CONTROL_DRAIN_QUEUE,),
    )
    write_inbox_item(tmp_path, "20260101T000000000002Z.txt", composed)

    parsed = parse_inbox_payload(composed)
    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))

    assert composed == (
        f"Control: {INBOX_CONTROL_DRAIN_QUEUE}\n"
        f"keep draining\n"
        f"Note: {INBOX_CONTINUE_NOTE}\n"
    )
    assert parsed.body == "keep draining"
    assert parsed.controls == (INBOX_CONTROL_DRAIN_QUEUE,)
    assert any("control=drive-drain-queue: DRAIN QUEUE ASAP" in row for row in rows)


def test_parse_preserves_non_note_parenthetical_suffix():
    parsed = parse_inbox_payload(
        "keep draining\n(DRAIN QUEUE ASAP: spice task next)\n"
        f"Note: {INBOX_CONTINUE_NOTE}\n"
    )

    assert parsed.body == "keep draining\n(DRAIN QUEUE ASAP: spice task next)"
    assert parsed.is_stop is False


def test_key_aliases_accept_dropped_z():
    aliases = inbox_item_key_aliases("20260101T000000000001Z.txt")
    assert aliases == {"20260101T000000000001Z", "20260101T000000000001"}


def test_ack_retires_pending_item_via_dropped_z_alias(tmp_path):
    name = "20260102T000000000002Z.txt"
    composed = compose_inbox_text(body="please ack me", priority=None, stop=False)
    write_inbox_item(tmp_path, name, composed)
    archived = archive_ackd_inbox_items(tmp_path, ["20260102T000000000002"])
    assert archived == [inbox_item_key(name)]
    assert pending_inbox_count(str(tmp_path)) == 0


def test_ack_archives_pending_item_attachments(tmp_path):
    name = "20260102T000000000003Z.txt"
    composed = compose_inbox_text(body="please inspect this", priority=None, stop=False)
    attachments = prepare_inbox_attachments(
        [
            {
                "name": "paste.png",
                "contentType": "image/png",
                "dataUrl": IMAGE_DATA_URL,
            }
        ]
    )
    written = write_inbox_item(tmp_path, name, composed, attachments=attachments)
    pending_attachment_dir = inbox_attachment_dir(written)

    items = collect_inbox_items(str(tmp_path))
    assert items[0].attachments[0].name == "paste.png"
    assert items[0].attachments[0].path.read_bytes() == b"image-bytes"

    archived = archive_ackd_inbox_items(tmp_path, ["20260102T000000000003"])

    archive_text = inbox_dir(tmp_path) / "archive" / name
    archive_attachment_dir = inbox_attachment_dir(archive_text)
    assert archived == [inbox_item_key(name)]
    assert archive_text.is_file()
    assert archive_attachment_dir.is_dir()
    assert not pending_attachment_dir.exists()


def test_inbox_attachment_readout_rows_render_clickable_reference(tmp_path):
    name = "20260102T000000000004Z.txt"
    composed = compose_inbox_text(body="please inspect this", priority=None, stop=False)
    attachments = prepare_inbox_attachments(
        [
            {
                "name": "paste.png",
                "contentType": "image/png",
                "dataUrl": IMAGE_DATA_URL,
            }
        ]
    )
    write_inbox_item(tmp_path, name, composed, attachments=attachments)
    item = collect_inbox_items(str(tmp_path))[0]

    rows = inbox_payload_rows([item])
    attachment_row = next(row for row in rows if "attachment 1:" in row)
    html = render_message_html(attachment_row, worktree_id="wt")
    archived_path = (
        inbox_attachment_dir(item.archive_path) / item.attachments[0].path.name
    )

    assert f"[paste.png]({archived_path.as_posix()})" in attachment_row
    assert item.attachments[0].path.as_posix() not in attachment_row
    assert 'href="/work/tree/wt/' in html
    assert ">paste.png</a>" in html
    archive_ackd_inbox_items(tmp_path, [inbox_item_key(name)])
    assert archived_path.is_file()


def test_find_archived_inbox_attachment_references_strips_sentence_punctuation():
    refs = find_archived_inbox_attachment_references(
        "Open .spice/inbox/archive/20260102T000000000004Z.attachments/"
        "01-image.png. Also "
        "/tmp/repo/.spice/inbox/archive/20260102T000000000004Z.attachments/"
        "02-image.png; ignore live "
        ".spice/inbox/20260102T000000000004Z.attachments/03-image.png."
    )

    assert refs == (
        ".spice/inbox/archive/20260102T000000000004Z.attachments/01-image.png",
        "/tmp/repo/.spice/inbox/archive/"
        "20260102T000000000004Z.attachments/02-image.png",
    )


def test_reading_does_not_clear_pending(tmp_path):
    composed = compose_inbox_text(body="sticky until acked", priority=None, stop=False)
    write_inbox_item(tmp_path, "20260103T000000000003Z.txt", composed)
    collect_inbox_items(str(tmp_path))
    collect_inbox_items(str(tmp_path))
    assert pending_inbox_count(str(tmp_path)) == 1


def test_inbox_payload_rows_prompt_immediate_task_offload(tmp_path):
    composed = compose_inbox_text(body="new scope", priority=None, stop=False)
    write_inbox_item(tmp_path, "20260103T000000000004Z.txt", composed)
    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))

    assert INBOX_TASK_HINT_ROW in rows
    assert "decide now" in INBOX_TASK_HINT_ROW
    assert "scope/tracking changed" in INBOX_TASK_HINT_ROW
    assert "before resuming work" in INBOX_TASK_HINT_ROW


def test_inbox_payload_rows_suppress_task_offload_for_maxim_guidance(tmp_path):
    composed = compose_inbox_text(
        body="No separate task is needed for the maxim itself.",
        priority="maxim",
        stop=False,
    )
    write_inbox_item(tmp_path, "20260103T000000000005Z.txt", composed)
    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))

    assert "  priority=maxim" in rows
    assert any(
        "No separate task is needed for the maxim itself." in row for row in rows
    )
    assert INBOX_TASK_HINT_ROW not in rows


def test_inbox_payload_rows_keep_task_offload_for_mixed_user_steering(tmp_path):
    maxim = compose_inbox_text(
        body="No separate task is needed for the maxim itself.",
        priority="maxim",
        stop=False,
    )
    user = compose_inbox_text(body="new scope", priority=None, stop=False)
    write_inbox_item(tmp_path, "20260103T000000000006Z.txt", maxim)
    write_inbox_item(tmp_path, "20260103T000000000007Z.txt", user)
    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))

    assert INBOX_TASK_HINT_ROW in rows
