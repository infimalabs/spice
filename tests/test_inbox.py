"""Inbox steering: durable publish, payload round-trip, ACK retirement."""

import os
import subprocess
import time

from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.attachments import (
    inbox_attachment_dir,
    prepare_inbox_attachments,
)
from spice.mail.inbox import (
    collect_archived_inbox_items,
    collect_deadlettered_inbox_items,
    INBOX_CONTROL_DRAIN_QUEUE,
    INBOX_CONTINUE_NOTE,
    INBOX_GRACEFUL_NOTE,
    INBOX_TASK_HINT_ROW,
    collect_inbox_items,
    compose_inbox_text,
    deadletter_inbox_item,
    inbox_ack_format_hint_row,
    inbox_dir,
    inbox_deadletter_context_rows,
    inbox_item_key,
    inbox_item_key_aliases,
    inbox_payload_rows,
    parse_inbox_payload,
    pending_inbox_count,
    pending_operator_inbox_count,
    requeue_deadlettered_inbox_item,
    write_inbox_item,
)
from spice.paths import shared_attachment_root
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


def test_inbox_readout_ack_guidance_leaves_response_wording_open(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260101T000000000003Z.txt",
        compose_inbox_text(body="please capture this", priority=None, stop=False),
    )

    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))
    readout = "\n".join(rows)

    assert (
        "ACK by assistant message: "
        "ACK <key> [<key> ...]: <what changed or was captured>"
    ) in readout
    assert "ACK example: include the key and a concise response" in readout
    assert "understood" not in readout
    assert "put this literal text" not in readout


def test_aged_inbox_ack_hint_avoids_literal_response_script(tmp_path):
    written = write_inbox_item(
        tmp_path,
        "20260101T000000000004Z.txt",
        compose_inbox_text(body="please capture this too", priority=None, stop=False),
    )
    old = time.time() - 2 * 60
    os.utime(written, (old, old))
    items = collect_inbox_items(str(tmp_path))

    row = inbox_ack_format_hint_row(items)

    assert "include an ACK header in your next assistant message" in row
    assert "put this literal text" not in row
    assert "understood" not in row


def test_parse_preserves_non_note_parenthetical_suffix():
    parsed = parse_inbox_payload(
        "keep draining\n(DRAIN QUEUE ASAP: spice task next)\n"
        f"Note: {INBOX_CONTINUE_NOTE}\n"
    )

    assert parsed.body == "keep draining\n(DRAIN QUEUE ASAP: spice task next)"
    assert parsed.is_stop is False


def _init_repo(path):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


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
    _init_repo(tmp_path)
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
    assert shared_attachment_root(tmp_path) in items[0].attachments[0].path.parents

    archived = archive_ackd_inbox_items(tmp_path, ["20260102T000000000003"])
    archived_items = collect_archived_inbox_items(tmp_path)

    archive_text = inbox_dir(tmp_path) / "archive" / name
    archive_attachment_dir = inbox_attachment_dir(archive_text)
    assert archived == [inbox_item_key(name)]
    assert archive_text.is_file()
    assert archive_attachment_dir.is_dir()
    assert not pending_attachment_dir.exists()
    assert archived_items[0].attachments[0].path.read_bytes() == b"image-bytes"
    assert (
        shared_attachment_root(tmp_path)
        in archived_items[0].attachments[0].path.parents
    )


def test_inbox_attachment_readout_rows_render_clickable_reference(tmp_path):
    _init_repo(tmp_path)
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
    archived_path = item.attachments[0].path

    assert "[paste.png](.spice/attachments/" in attachment_row
    assert archived_path.as_posix() not in attachment_row
    assert 'href="/work/tree/wt/' in html
    assert ">paste.png</a>" in html
    archive_ackd_inbox_items(tmp_path, [inbox_item_key(name)])
    assert archived_path.is_file()


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
    assert "capture in the moment" in INBOX_TASK_HINT_ROW
    assert "operator asks for a task" in INBOX_TASK_HINT_ROW
    assert "scope/tracking changed" in INBOX_TASK_HINT_ROW
    assert "before resuming work" in INBOX_TASK_HINT_ROW


def test_pending_operator_count_excludes_automated_guidance(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260103T000000000010Z.txt",
        compose_inbox_text(
            body="please pick up the new ask", priority=None, stop=False
        ),
    )
    write_inbox_item(
        tmp_path,
        "20260103T000000000011Z.txt",
        compose_inbox_text(
            body="automated maxim guidance", priority="maxim", stop=False
        ),
    )

    # Both items are pending, but only the genuine operator steering should be
    # able to resurrect an idle agent; the maxim is informational at launch.
    assert pending_inbox_count(str(tmp_path)) == 2
    assert pending_operator_inbox_count(str(tmp_path)) == 1


def test_pending_operator_count_zero_for_only_automated_guidance(tmp_path):
    write_inbox_item(
        tmp_path,
        "20260103T000000000012Z.txt",
        compose_inbox_text(
            body="automated maxim guidance", priority="maxim", stop=False
        ),
    )

    assert pending_inbox_count(str(tmp_path)) == 1
    assert pending_operator_inbox_count(str(tmp_path)) == 0


def test_deadletter_excludes_item_from_pending_and_can_requeue(tmp_path):
    _init_repo(tmp_path)
    name = "20260103T000000000014Z.txt"
    composed = compose_inbox_text(body="operator steering", priority=None, stop=False)
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

    assert deadletter_inbox_item(tmp_path, "20260103T000000000014") == inbox_item_key(
        name
    )
    assert pending_inbox_count(tmp_path) == 0
    assert pending_operator_inbox_count(tmp_path) == 0
    assert collect_inbox_items(tmp_path) == []
    deadletters = collect_deadlettered_inbox_items(tmp_path)
    assert [item.name for item in deadletters] == [name]
    assert deadletters[0].attachments[0].name == "paste.png"
    assert (
        shared_attachment_root(tmp_path) in deadletters[0].attachments[0].path.parents
    )
    rows = inbox_deadletter_context_rows(deadletters)
    assert "requeue=spice agent requeue-deadletter <key>" in rows[0]
    assert "deadlettered_inbox key=20260103T000000000014Z" in rows[1]

    requeued = requeue_deadlettered_inbox_item(tmp_path, "20260103T000000000014Z")

    assert requeued is not None
    assert pending_inbox_count(tmp_path) == 1
    assert pending_operator_inbox_count(tmp_path) == 1
    assert collect_deadlettered_inbox_items(tmp_path) == []
    item = collect_inbox_items(tmp_path)[0]
    assert item.text == composed
    assert item.attachments[0].path.read_bytes() == b"image-bytes"


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
