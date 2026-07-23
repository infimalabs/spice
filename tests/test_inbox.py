"""Inbox steering: durable publish, payload round-trip, ACK retirement."""

import os
import subprocess
import time
from pathlib import Path

from spice.mail.ackarchive import archive_ackd_inbox_items
from spice.mail.ackstate import (
    ACK_DISPOSITION_REFUSED,
    AckStateWrite,
    ack_state_database_path,
    ack_state_records,
    record_acked_inbox_items,
)
from spice.mail.attachments import (
    prepare_inbox_attachments,
)
from spice.mail.inbox import (
    collect_deadlettered_inbox_items,
    INBOX_CONTROL_DRAIN_QUEUE,
    INBOX_CONTINUE_NOTE,
    INBOX_GRACEFUL_NOTE,
    INBOX_TASK_HINT_ROW,
    collect_inbox_items,
    collect_refused_inbox_items,
    compose_inbox_text,
    deadletter_inbox_item,
    inbox_ack_state_context_rows,
    inbox_item_age_seconds,
    inbox_item_readout_rows,
    InboxResendAttempt,
    inbox_ack_format_hint_row,
    inbox_attachment_dir,
    inbox_dir,
    inbox_deadletter_context_rows,
    inbox_item_key,
    inbox_payload_rows,
    parse_inbox_payload,
    pending_inbox_count,
    pending_operator_inbox_items,
    requeue_deadlettered_inbox_item,
    write_inbox_item,
)
from spice.paths import shared_attachment_root
from spice.serve.markdown import render_message_html
from spice.tasks import identity

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="
_ONE_DAY_SECONDS = 24 * 60 * 60
_STORE_FRESH_MAX_SECONDS = 60
# Deep enough that an O(backlog) per-submit scan would read many bodies.
_SUBMIT_BACKLOG_DEPTH = 40
_SUBMIT_DUPLICATE_INDEX = 20


_DATED_EPOCH_MS = 1767225600000  # 2026-01-01T00:00:00Z


def _dated_inbox_name(index: int) -> str:
    return f"{identity.encode_width(_DATED_EPOCH_MS + index)}.txt"


def test_write_then_collect_round_trip(tmp_path):
    composed = compose_inbox_text(body="steer left", priority=None, stop=False)
    written = write_inbox_item(tmp_path, "1jN54zJK.txt", composed)
    items = collect_inbox_items(str(tmp_path))
    assert [item.name for item in items] == ["1jN54zJK.txt"]
    assert items[0].text == composed
    assert written.parent == inbox_dir(tmp_path)
    assert pending_inbox_count(str(tmp_path)) == 1


def test_write_inbox_item_can_dedupe_pending_text(tmp_path):
    composed = compose_inbox_text(body="same steering", priority=None, stop=False)
    first = write_inbox_item(
        tmp_path,
        "1jN54zJK.txt",
        composed,
        dedupe_pending_text=True,
    )
    second = write_inbox_item(
        tmp_path,
        "1jN54zJL.txt",
        composed,
        dedupe_pending_text=True,
    )

    items = collect_inbox_items(tmp_path)

    assert second == first
    assert [item.name for item in items] == ["1jN54zJK.txt"]
    assert pending_inbox_count(tmp_path) == 1


def test_write_inbox_item_does_not_dedupe_attachment_messages_by_text_only(tmp_path):
    _init_repo(tmp_path)
    composed = compose_inbox_text(body="same steering", priority=None, stop=False)
    first = write_inbox_item(
        tmp_path,
        "1jN54zJK.txt",
        composed,
        attachments=prepare_inbox_attachments(
            [
                {
                    "name": "paste.png",
                    "contentType": "image/png",
                    "dataUrl": IMAGE_DATA_URL,
                }
            ]
        ),
        dedupe_pending_text=True,
    )
    second = write_inbox_item(
        tmp_path,
        "1jN54zJL.txt",
        composed,
        dedupe_pending_text=True,
    )

    assert second != first
    assert [item.name for item in collect_inbox_items(tmp_path)] == [
        "1jN54zJK.txt",
        "1jN54zJL.txt",
    ]
    assert pending_inbox_count(tmp_path) == 2


def test_submit_body_reads_stay_flat_as_unacknowledged_backlog_grows(
    tmp_path, monkeypatch
):
    """Steering submit must not read every queued body as the backlog grows.

    The reported cliff: submit was fast onto an empty inbox and then slowed,
    progressively, as unacknowledged messages piled up, because both the dedup
    pre-check and the pending-identity payload read the full text of every
    pending item on each submit. Identity now derives from stat metadata alone
    and dedup only reads size-collision candidates, so the per-submit body-read
    count is flat regardless of backlog depth.
    """
    from spice.serve.pending import pending_inbox_identity_payload

    inbox = inbox_dir(tmp_path)
    body_reads: list[str] = []
    real_open = Path.open

    # Instrument at the open layer only: Path.read_text opens through Path.open,
    # so this counts every queued-body read exactly once, whether it arrives via
    # read_text (dedup) or a direct open (snapshot).
    def counting_open(self, *args, **kwargs):
        if self.suffix == ".txt" and self.parent == inbox:
            body_reads.append(self.name)
        return real_open(self, *args, **kwargs)

    monkeypatch.setattr(Path, "open", counting_open)

    def _submit(index):
        # Each queued message is a distinct on-disk size, so no size collision
        # ever forces a body read during dedup.
        composed = compose_inbox_text(
            body="steer " + "x" * index, priority=None, stop=False
        )
        body_reads.clear()
        write_inbox_item(
            tmp_path, _dated_inbox_name(index), composed, dedupe_pending_text=True
        )
        pending_inbox_identity_payload(str(tmp_path))
        return list(body_reads)

    total = _SUBMIT_BACKLOG_DEPTH + 1
    first = _submit(1)
    for index in range(2, total):
        _submit(index)
    deep = _submit(total)

    # Zero queued bodies read whether the inbox holds one item or the full
    # backlog: the fast-first-then-progressive-cliff is gone.
    assert first == []
    assert deep == []
    assert pending_inbox_count(str(tmp_path)) == total

    # The dedup gate still collapses an identical still-pending submission, and
    # it does so by reading only the single size-matching candidate, not the
    # whole backlog.
    duplicate = compose_inbox_text(
        body="steer " + "x" * _SUBMIT_DUPLICATE_INDEX, priority=None, stop=False
    )
    body_reads.clear()
    landed = write_inbox_item(
        tmp_path, "1jN5530b.txt", duplicate, dedupe_pending_text=True
    )
    duplicate_reads = list(body_reads)
    assert landed == inbox / _dated_inbox_name(_SUBMIT_DUPLICATE_INDEX)
    assert duplicate_reads == [_dated_inbox_name(_SUBMIT_DUPLICATE_INDEX)]
    assert pending_inbox_count(str(tmp_path)) == total


def test_compose_parse_round_trip_with_priority_and_stop():
    composed = compose_inbox_text(body="wrap it up", priority="urgent", stop=True)
    parsed = parse_inbox_payload(composed)
    assert parsed.priority == "urgent"
    assert parsed.body == "wrap it up"
    assert parsed.is_stop is True
    assert INBOX_GRACEFUL_NOTE in composed


def test_compose_parse_round_trip_with_review_priority():
    composed = compose_inbox_text(
        body="peer review found follow-up work",
        priority="review",
        stop=False,
    )
    parsed = parse_inbox_payload(composed)
    assert "Priority: review" in composed
    assert parsed.priority == "review"
    assert parsed.body == "peer review found follow-up work"
    assert parsed.is_stop is False


def test_compose_parse_round_trip_with_resend_lineage():
    composed = compose_inbox_text(
        body="keep going",
        priority="urgent",
        stop=False,
        controls=(INBOX_CONTROL_DRAIN_QUEUE,),
        resend_attempts=(
            InboxResendAttempt(
                attempt=1,
                at="2026-01-01T00:00:00Z",
                messages_elapsed=3,
            ),
            InboxResendAttempt(
                attempt=2,
                at="2026-01-01T00:01:00Z",
                messages_elapsed=4,
            ),
        ),
    )
    parsed = parse_inbox_payload(composed)

    assert parsed.priority == "urgent"
    assert parsed.controls == (INBOX_CONTROL_DRAIN_QUEUE,)
    assert parsed.body == "keep going"
    assert parsed.resend_count == 2
    assert parsed.resend_attempts == (
        InboxResendAttempt(
            attempt=1,
            at="2026-01-01T00:00:00Z",
            messages_elapsed=3,
        ),
        InboxResendAttempt(
            attempt=2,
            at="2026-01-01T00:01:00Z",
            messages_elapsed=4,
        ),
    )


def test_inbox_readout_labels_resend_lineage(tmp_path):
    composed = compose_inbox_text(
        body="keep going",
        priority="critical",
        stop=False,
        resend_attempts=(
            InboxResendAttempt(
                attempt=1,
                at="2026-01-01T00:00:00Z",
                messages_elapsed=3,
            ),
            InboxResendAttempt(
                attempt=2,
                at="2026-01-01T00:01:00Z",
                messages_elapsed=4,
            ),
        ),
    )
    write_inbox_item(tmp_path, "1jN54zJL.txt", composed)

    readout = "\n".join(inbox_payload_rows(collect_inbox_items(str(tmp_path))))

    assert "key=1jN54zJL: age=" in readout
    assert "resend #2" in readout
    assert "priority=critical" in readout


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
    write_inbox_item(tmp_path, "1jN54zJL.txt", composed)

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
    assert "resend #" not in "\n".join(rows)


def test_inbox_readout_ack_guidance_leaves_response_wording_open(tmp_path):
    write_inbox_item(
        tmp_path,
        "1jN54zJM.txt",
        compose_inbox_text(body="please capture this", priority=None, stop=False),
    )

    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))
    readout = "\n".join(rows)

    assert (
        "Real-time N/ACK loop: put a plain-text ACK or reasoned NACK header "
        "near the start of each working assistant message"
    ) in readout
    assert "ACK <key> [<key> ...]: <what changed or was captured>" in readout
    assert "acknowledged keys clear once processed" in readout
    assert "NACK <key>: <why this cannot be done>" in readout
    assert "refused keys clear once processed" in readout
    assert "Do not bury ACKs or NACKs mid-message" in readout
    assert (
        "N/ACK example: lead the next working assistant message with a concise "
        "ACK response or reasoned NACK"
    ) in readout
    assert "understood" not in readout
    assert "put this literal text" not in readout


def test_aged_inbox_ack_hint_avoids_literal_response_script(tmp_path):
    written = write_inbox_item(
        tmp_path,
        "1jN54zJN.txt",
        compose_inbox_text(body="please capture this too", priority=None, stop=False),
    )
    old = time.time() - 2 * 60
    os.utime(written, (old, old))
    items = collect_inbox_items(str(tmp_path))

    row = inbox_ack_format_hint_row(items)

    assert "include an ACK or reasoned NACK header near the start" in row
    assert "NACK " in row
    assert ": <why this cannot be done>" in row
    assert "put this literal text" not in row
    assert "understood" not in row


def test_aged_inbox_ack_nag_names_both_reply_paths(tmp_path):
    written = write_inbox_item(
        tmp_path,
        "1jN54zJP.txt",
        compose_inbox_text(body="please respond", priority=None, stop=False),
    )
    fresh_row = inbox_ack_format_hint_row(collect_inbox_items(str(tmp_path)))
    assert "spice agent reply" not in fresh_row

    for age_seconds in (20, 2 * 60, 6 * 60):
        old = time.time() - age_seconds
        os.utime(written, (old, old))
        row = inbox_ack_format_hint_row(collect_inbox_items(str(tmp_path)))
        assert "Two paths retire keys" in row
        assert 'spice agent reply "ACK <key>: ..."' in row
        assert "not reaching the surface" in row


def test_parse_preserves_non_note_parenthetical_suffix():
    parsed = parse_inbox_payload(
        "keep draining\n(DRAIN QUEUE ASAP: spice task next)\n"
        f"Note: {INBOX_CONTINUE_NOTE}\n"
    )

    assert parsed.body == "keep draining\n(DRAIN QUEUE ASAP: spice task next)"
    assert parsed.is_stop is False


def _init_repo(path):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


def test_inbox_item_key_strips_extension_and_keeps_collision_suffix():
    assert inbox_item_key("1jN54zJK.txt") == "1jN54zJK"
    assert inbox_item_key("1jN54zJK-2.txt") == "1jN54zJK-2"
    assert inbox_item_key("1jN54zJK") == "1jN54zJK"


def test_ack_retires_pending_item_by_exact_key(tmp_path):
    _init_repo(tmp_path)
    name = "1jNJvRyp.txt"
    composed = compose_inbox_text(body="please ack me", priority=None, stop=False)
    write_inbox_item(tmp_path, name, composed)
    archived = archive_ackd_inbox_items(tmp_path, ["1jNJvRyp"])
    assert archived == [inbox_item_key(name)]
    assert pending_inbox_count(str(tmp_path)) == 0


def test_ack_records_pending_item_with_attachments_in_sqlite_state(tmp_path):
    _init_repo(tmp_path)
    name = "1jNJvRyq.txt"
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

    items = collect_inbox_items(str(tmp_path))
    attachment_path = items[0].attachments[0].path
    assert items[0].attachments[0].name == "paste.png"
    assert attachment_path.read_bytes() == b"image-bytes"
    assert shared_attachment_root(tmp_path) in attachment_path.parents

    archived = archive_ackd_inbox_items(tmp_path, ["1jNJvRyq"])
    archived_records = ack_state_records(tmp_path)
    archived_attachment = archived_records[0].attachments[0]
    assert archived == [inbox_item_key(name)]
    assert [(record.inbox_name, record.text) for record in archived_records] == [
        (name, composed)
    ]
    assert archived_attachment["name"] == "paste.png"
    assert archived_attachment["content_type"] == "image/png"
    assert Path(archived_attachment["path"]) == attachment_path
    assert archived_attachment["size"] == len(b"image-bytes")
    assert attachment_path.is_file()


def test_inbox_attachment_readout_rows_render_clickable_reference(tmp_path):
    _init_repo(tmp_path)
    name = "1jNJvRyr.txt"
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

    assert f"[paste.png]({archived_path.as_posix()})" in attachment_row
    assert shared_attachment_root(tmp_path) in archived_path.parents
    assert 'href="/work/tree/wt/' in html
    assert ">paste.png</a>" in html


def test_reading_does_not_clear_pending(tmp_path):
    composed = compose_inbox_text(body="sticky until acked", priority=None, stop=False)
    write_inbox_item(tmp_path, "1jNXjwdH.txt", composed)
    collect_inbox_items(str(tmp_path))
    collect_inbox_items(str(tmp_path))
    assert pending_inbox_count(str(tmp_path)) == 1


def test_inbox_payload_rows_prompt_immediate_task_offload(tmp_path):
    composed = compose_inbox_text(body="new scope", priority=None, stop=False)
    write_inbox_item(tmp_path, "1jNXjwdJ.txt", composed)
    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))

    assert INBOX_TASK_HINT_ROW in rows
    assert "capture in the moment" in INBOX_TASK_HINT_ROW
    assert "standalone TASK line" in INBOX_TASK_HINT_ROW
    assert "TASK title=... | project=<stem.child> [| acceptance=...]" in (
        INBOX_TASK_HINT_ROW
    )
    assert "omitted acceptance with no flow starts in plan" in INBOX_TASK_HINT_ROW
    assert "repeat acceptance=... for multiple criteria" in INBOX_TASK_HINT_ROW
    assert "ACK prose first and then the TASK line on its own line" in (
        INBOX_TASK_HINT_ROW
    )
    assert "same task-add batch format" in INBOX_TASK_HINT_ROW
    assert "resume allocator flow" in INBOX_TASK_HINT_ROW


def test_pending_operator_count_excludes_automated_guidance(tmp_path):
    write_inbox_item(
        tmp_path,
        "1jNXjwdQ.txt",
        compose_inbox_text(
            body="please pick up the new ask", priority=None, stop=False
        ),
    )
    write_inbox_item(
        tmp_path,
        "1jNXjwdR.txt",
        compose_inbox_text(
            body="automated maxim guidance", priority="maxim", stop=False
        ),
    )
    write_inbox_item(
        tmp_path,
        "1jNXjwdS.txt",
        compose_inbox_text(
            body="automated review guidance", priority="review", stop=False
        ),
    )

    # Both items are pending, but only the genuine operator steering should be
    # able to resurrect an idle agent; automated guidance is informational at launch.
    assert pending_inbox_count(str(tmp_path)) == 3
    assert len(pending_operator_inbox_items(str(tmp_path))) == 1


def test_pending_operator_count_zero_for_only_automated_guidance(tmp_path):
    write_inbox_item(
        tmp_path,
        "1jNXjwdS.txt",
        compose_inbox_text(
            body="automated maxim guidance", priority="maxim", stop=False
        ),
    )
    write_inbox_item(
        tmp_path,
        "1jNXjwdT.txt",
        compose_inbox_text(
            body="automated review guidance", priority="review", stop=False
        ),
    )

    assert pending_inbox_count(str(tmp_path)) == 2
    assert len(pending_operator_inbox_items(str(tmp_path))) == 0


def test_deadletter_excludes_item_from_pending_and_can_requeue(tmp_path):
    _init_repo(tmp_path)
    name = "1jNXjwdV.txt"
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

    assert deadletter_inbox_item(tmp_path, "1jNXjwdV") == inbox_item_key(name)
    assert pending_inbox_count(tmp_path) == 0
    assert len(pending_operator_inbox_items(tmp_path)) == 0
    assert collect_inbox_items(tmp_path) == []
    deadletters = collect_deadlettered_inbox_items(tmp_path)
    assert [item.name for item in deadletters] == [name]
    assert deadletters[0].attachments[0].name == "paste.png"
    assert (
        shared_attachment_root(tmp_path) in deadletters[0].attachments[0].path.parents
    )
    rows = inbox_deadletter_context_rows(deadletters)
    assert "requeue=spice agent requeue-deadletter <key>" in rows[0]
    assert "deadlettered_inbox key=1jNXjwdV" in rows[1]
    deadletter_attachment_dir = inbox_attachment_dir(deadletters[0].source_path)
    assert deadletter_attachment_dir.is_dir()

    requeued = requeue_deadlettered_inbox_item(tmp_path, "1jNXjwdV")

    assert requeued is not None
    assert not deadletter_attachment_dir.exists()
    assert pending_inbox_count(tmp_path) == 1
    assert len(pending_operator_inbox_items(tmp_path)) == 1
    assert collect_deadlettered_inbox_items(tmp_path) == []
    item = collect_inbox_items(tmp_path)[0]
    assert item.text == composed
    assert item.attachments[0].path.read_bytes() == b"image-bytes"


def test_inbox_payload_rows_suppress_task_offload_for_automated_guidance(tmp_path):
    maxim = compose_inbox_text(
        body="No separate task is needed for the maxim itself.",
        priority="maxim",
        stop=False,
    )
    review = compose_inbox_text(
        body="Peer review feedback already links follow-up tasks.",
        priority="review",
        stop=False,
    )
    write_inbox_item(tmp_path, "1jNXjwdK.txt", maxim)
    write_inbox_item(tmp_path, "1jNXjwdL.txt", review)
    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))

    assert "  priority=maxim" in rows
    assert "  priority=review" in rows
    assert any(
        "No separate task is needed for the maxim itself." in row for row in rows
    )
    assert any(
        "Peer review feedback already links follow-up tasks." in row for row in rows
    )
    assert INBOX_TASK_HINT_ROW not in rows


def test_inbox_payload_rows_keep_task_offload_for_mixed_user_steering(tmp_path):
    maxim = compose_inbox_text(
        body="No separate task is needed for the maxim itself.",
        priority="maxim",
        stop=False,
    )
    user = compose_inbox_text(body="new scope", priority=None, stop=False)
    review = compose_inbox_text(
        body="Peer review feedback already links follow-up tasks.",
        priority="review",
        stop=False,
    )
    write_inbox_item(tmp_path, "1jNXjwdL.txt", maxim)
    write_inbox_item(tmp_path, "1jNXjwdM.txt", review)
    write_inbox_item(tmp_path, "1jNXjwdN.txt", user)
    rows = inbox_payload_rows(collect_inbox_items(str(tmp_path)))

    assert INBOX_TASK_HINT_ROW in rows


def test_ack_state_row_age_derives_from_archived_at_not_store_mtime(tmp_path):
    _init_repo(tmp_path)
    name = "1k9yC5yC.txt"
    four_days = 4 * _ONE_DAY_SECONDS
    archived_at = time.time() - four_days
    record_acked_inbox_items(
        tmp_path,
        [
            AckStateWrite(
                key=inbox_item_key(name),
                inbox_name=name,
                text=compose_inbox_text(
                    body="stale refusal", priority=None, stop=False
                ),
                disposition=ACK_DISPOSITION_REFUSED,
            )
        ],
        now=archived_at,
    )

    # The shared sqlite store was just written, so its mtime looks fresh — the
    # exact divergence that made 4-day-old rows read as minutes old.
    store_path = ack_state_database_path(tmp_path)
    assert time.time() - store_path.stat().st_mtime < _STORE_FRESH_MAX_SECONDS

    items = collect_refused_inbox_items(tmp_path)
    assert len(items) == 1
    item = items[0]
    assert item.source_path == store_path
    assert item.age_epoch == archived_at
    assert inbox_item_age_seconds(item) >= four_days

    assert inbox_ack_state_context_rows(items) == [
        "source=ack_state; status=already_consumed_operator_steering; store=sqlite",
        "refused_inbox key=1k9yC5yC age=4d ago text=stale refusal",
    ]
    assert inbox_item_readout_rows(item) == [
        "key=1k9yC5yC: age=4d ago",
        "  stale refusal",
        f"  note={INBOX_CONTINUE_NOTE}",
    ]


def test_ack_state_row_age_falls_back_to_key_timestamp_without_archived_at(tmp_path):
    _init_repo(tmp_path)
    name = "1k9yC5yC.txt"
    record_acked_inbox_items(
        tmp_path,
        [
            AckStateWrite(
                key=inbox_item_key(name),
                inbox_name=name,
                text=compose_inbox_text(
                    body="legacy refusal", priority=None, stop=False
                ),
                disposition=ACK_DISPOSITION_REFUSED,
            )
        ],
        now=0.0,
    )

    item = collect_refused_inbox_items(tmp_path)[0]
    # archived_at is 0; the key's mint moment (2026-07-04) anchors age instead
    # of falling back to the store mtime.
    key_epoch = identity.incepted_datetime(inbox_item_key(name)).timestamp()
    assert item.age_epoch == key_epoch
    assert inbox_item_age_seconds(item) >= _ONE_DAY_SECONDS
